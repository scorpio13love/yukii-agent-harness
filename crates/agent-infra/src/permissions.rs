//! 审批账本：决策与审计事件同事务保存；领取与撤销通过 SQLite 写事务串行化。
use crate::{TaskStore, sqlite};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use yukii_agent_domain::permissions::{
    PermissionError, decide_state, validate_binding, validate_expiration, validate_new_expiration,
};
use yukii_agent_ports::permissions::PermissionRepository;
use yukii_contracts::permissions::{
    ExecutionPermit, PermissionBinding, PermissionDecision, PermissionRecord, PermissionState,
};

struct StoredPermission {
    record: PermissionRecord,
    tool_name: String,
    arguments: String,
    claim_id: Option<String>,
}

fn storage_error(_: impl std::fmt::Display) -> PermissionError {
    PermissionError::Storage
}

impl TaskStore {
    fn permission_transaction<T>(
        &self,
        operation: impl FnOnce(&Connection, i64) -> Result<T, PermissionError>,
    ) -> Result<T, PermissionError> {
        let mut connection = self.connection.lock().map_err(storage_error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        // 取锁之后读取时钟，避免排队等待期间过期的授权被接受。
        let result = operation(&transaction, sqlite::now())?;
        transaction.commit().map_err(storage_error)?;
        Ok(result)
    }
}

impl PermissionRepository for TaskStore {
    fn request_permission(
        &self,
        binding: PermissionBinding,
        expires_at: i64,
    ) -> Result<PermissionRecord, PermissionError> {
        validate_binding(&binding)?;
        self.permission_transaction(|connection, now| {
            validate_new_expiration(expires_at, now)?;
            let (tool_name, arguments) = active_invocation(connection, &binding)?;
            let existing: Option<String> = connection.query_row(
                "SELECT id FROM permissions WHERE task_id=?1 AND call_id=?2",
                params![binding.task_id, binding.call_id], |row| row.get(0),
            ).optional().map_err(storage_error)?;
            if let Some(id) = existing {
                let saved = read_permission(connection, &id)?;
                check_binding(&saved, &binding)?;
                if saved.record.expires_at != expires_at
                    || saved.tool_name != tool_name || saved.arguments != arguments {
                    return Err(PermissionError::BindingMismatch);
                }
                // 重复请求返回现状，绝不恢复已拒绝、领取或撤销的授权。
                return Ok(saved.record);
            }
            let record = PermissionRecord {
                id: uuid::Uuid::new_v4().to_string(),
                binding,
                expires_at,
                state: PermissionState::Pending,
            };
            connection.execute(
                "INSERT INTO permissions(id,task_id,call_id,binding,tool_name,arguments,expires_at,state)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,'pending')",
                params![record.id, record.binding.task_id, record.binding.call_id,
                    serde_json::to_string(&record.binding).map_err(storage_error)?,
                    tool_name, arguments, record.expires_at],
            ).map_err(storage_error)?;
            append_audit(connection, &record, "pending", now)?;
            Ok(record)
        })
    }

    fn decide_permission(
        &self,
        id: &str,
        binding: &PermissionBinding,
        decision: PermissionDecision,
    ) -> Result<PermissionRecord, PermissionError> {
        self.permission_transaction(|connection, now| {
            let mut saved = read_permission(connection, id)?;
            check_binding(&saved, binding)?;
            check_live(connection, &saved, now)?;
            let next = decide_state(saved.record.state, decision)?;
            let label = match decision {
                PermissionDecision::Approve => "approved",
                PermissionDecision::Deny => "denied",
            };
            if saved.record.state == next {
                return Ok(saved.record);
            }
            update_state(connection, &saved.record, label, now)?;
            saved.record.state = next;
            Ok(saved.record)
        })
    }

    fn revoke_permission(
        &self,
        id: &str,
        binding: &PermissionBinding,
    ) -> Result<(), PermissionError> {
        self.permission_transaction(|connection, now| {
            let saved = read_permission(connection, id)?;
            check_binding(&saved, binding)?;
            // 即使任务结束或授权过期也可撤销，不能因执行上下文消失阻止用户撤权。
            if saved.record.state != PermissionState::Revoked {
                update_state(connection, &saved.record, "revoked", now)?;
            }
            Ok(())
        })
    }

    fn claim_permission(
        &self,
        id: &str,
        binding: &PermissionBinding,
    ) -> Result<ExecutionPermit, PermissionError> {
        self.permission_transaction(|connection, now| {
            let saved = read_permission(connection, id)?;
            check_binding(&saved, binding)?;
            check_live(connection, &saved, now)?;
            if saved.record.state != PermissionState::Approved {
                return Err(PermissionError::InvalidState);
            }
            let claim_id = uuid::Uuid::new_v4().to_string();
            connection
                .execute(
                    "UPDATE permissions SET claim_id=?1 WHERE id=?2",
                    params![claim_id, id],
                )
                .map_err(storage_error)?;
            update_state(connection, &saved.record, "claimed", now)?;
            Ok(ExecutionPermit {
                permission_id: id.into(),
                claim_id,
                binding: binding.clone(),
            })
        })
    }

    fn validate_permit(&self, permit: &ExecutionPermit) -> Result<(), PermissionError> {
        self.permission_transaction(|connection, now| {
            let saved = read_permission(connection, &permit.permission_id)?;
            check_binding(&saved, &permit.binding)?;
            if saved.record.state != PermissionState::Claimed
                || saved.claim_id.as_deref() != Some(permit.claim_id.as_str())
            {
                return Err(PermissionError::InvalidState);
            }
            check_live(connection, &saved, now)
        })
    }
}

fn read_permission(connection: &Connection, id: &str) -> Result<StoredPermission, PermissionError> {
    let row = connection.query_row(
        "SELECT binding,tool_name,arguments,expires_at,state,claim_id FROM permissions WHERE id=?1",
        [id],
        |row| Ok((
            row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?, row.get::<_, String>(4)?, row.get::<_, Option<String>>(5)?,
        )),
    ).optional().map_err(storage_error)?.ok_or(PermissionError::NotFound)?;
    Ok(StoredPermission {
        record: PermissionRecord {
            id: id.into(),
            binding: serde_json::from_str(&row.0).map_err(storage_error)?,
            expires_at: row.3,
            state: serde_json::from_value(serde_json::Value::String(row.4))
                .map_err(storage_error)?,
        },
        tool_name: row.1,
        arguments: row.2,
        claim_id: row.5,
    })
}

fn check_binding(
    saved: &StoredPermission,
    binding: &PermissionBinding,
) -> Result<(), PermissionError> {
    if &saved.record.binding != binding {
        return Err(PermissionError::BindingMismatch);
    }
    Ok(())
}

fn active_invocation(
    connection: &Connection,
    binding: &PermissionBinding,
) -> Result<(String, String), PermissionError> {
    let task = sqlite::read_task(connection, &binding.task_id).map_err(|error| match error {
        crate::StoreError::NotFound => PermissionError::InvocationUnavailable,
        _ => PermissionError::Storage,
    })?;
    sqlite::ensure_active(&task).map_err(|_| PermissionError::InvocationUnavailable)?;
    connection
        .query_row(
            "SELECT name,arguments FROM tool_calls
         WHERE task_id=?1 AND call_id=?2 AND execution_state='running' AND result IS NULL",
            params![binding.task_id, binding.call_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(storage_error)?
        .ok_or(PermissionError::InvocationUnavailable)
}

fn check_live(
    connection: &Connection,
    saved: &StoredPermission,
    now: i64,
) -> Result<(), PermissionError> {
    validate_expiration(saved.record.expires_at, now)?;
    let (name, arguments) = active_invocation(connection, &saved.record.binding)?;
    if name != saved.tool_name || arguments != saved.arguments {
        return Err(PermissionError::BindingMismatch);
    }
    Ok(())
}

fn update_state(
    connection: &Connection,
    record: &PermissionRecord,
    state: &str,
    now: i64,
) -> Result<(), PermissionError> {
    connection
        .execute(
            "UPDATE permissions SET state=?1 WHERE id=?2",
            params![state, record.id],
        )
        .map_err(storage_error)?;
    append_audit(connection, record, state, now)
}

fn append_audit(
    connection: &Connection,
    record: &PermissionRecord,
    state: &str,
    now: i64,
) -> Result<(), PermissionError> {
    connection
        .execute(
            "INSERT INTO permission_events(permission_id,state,created_at) VALUES(?1,?2,?3)",
            params![record.id, state, now],
        )
        .map_err(storage_error)?;
    // 任务事件不包含路径、内容或凭据。与授权审计共享事务，不伪报工具已执行。
    let task = sqlite::read_task(connection, &record.binding.task_id).map_err(storage_error)?;
    let message = match state {
        "pending" => "工具操作正在等待审批。",
        "approved" => "工具操作已获批准，尚未执行。",
        "denied" => "工具操作已被拒绝。",
        "claimed" => "执行端已领取本次操作授权，尚待执行结果。",
        "revoked" => "工具操作授权已撤销。",
        _ => return Err(PermissionError::Storage),
    };
    sqlite::append_event(connection, &task, message).map_err(storage_error)
}
