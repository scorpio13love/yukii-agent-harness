use std::{
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use yukii_agent_infra::TaskStore;
use yukii_agent_ports::{
    model::ToolCall,
    permissions::{PermissionError, PermissionRepository},
};
use yukii_contracts::{
    CreateTask, ExecutionTarget,
    permissions::{
        FileOperation, FileScope, PermissionBinding, PermissionDecision, PermissionState,
    },
};

struct DatabaseFile(PathBuf);
impl DatabaseFile {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("permission-{}.sqlite", uuid::Uuid::new_v4())))
    }
}
impl Drop for DatabaseFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn expires() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + 600
}

fn setup(store: &TaskStore) -> PermissionBinding {
    // 当前没有桌面任务入口：使用内部账本调用验证权限仓储，不代表网页获得本机能力。
    let task = store
        .create_once(
            CreateTask {
                conversation_id: None,
                prompt: "permission storage test".into(),
                target: ExecutionTarget::CloudOffice,
            },
            "request",
            true,
        )
        .unwrap()
        .task;
    store.claim_next().unwrap().unwrap();
    store
        .begin_tool(
            &task.id,
            &ToolCall {
                id: "call".into(),
                name: "local_create".into(),
                arguments: serde_json::json!({"relative_path":"report.md","content":"hello"}),
            },
            None,
            None,
            0,
        )
        .unwrap();
    PermissionBinding {
        session_id: "session".into(),
        task_id: task.id,
        call_id: "call".into(),
        scope: FileScope {
            workspace_id: "workspace".into(),
            relative_path: "report.md".into(),
            operation: FileOperation::Create {
                content_sha256: "a".repeat(64),
            },
        },
    }
}

#[test]
fn approval_survives_reopen_is_claimed_once_and_revocation_invalidates_existing_permit() {
    let file = DatabaseFile::new();
    let store = TaskStore::open(&file.0).unwrap();
    let binding = setup(&store);
    let deadline = expires();
    let record = store.request_permission(binding.clone(), deadline).unwrap();
    assert_eq!(record.state, PermissionState::Pending);
    assert!(store.claim_permission(&record.id, &binding).is_err());
    let count = store.events(&binding.task_id).unwrap().len();
    assert_eq!(
        store
            .request_permission(binding.clone(), deadline)
            .unwrap()
            .id,
        record.id
    );
    assert_eq!(store.events(&binding.task_id).unwrap().len(), count);
    store
        .decide_permission(&record.id, &binding, PermissionDecision::Approve)
        .unwrap();
    drop(store);
    let store = TaskStore::open(&file.0).unwrap();
    let permit = store.claim_permission(&record.id, &binding).unwrap();
    store.validate_permit(&permit).unwrap();
    assert!(store.claim_permission(&record.id, &binding).is_err());
    let mut forged = permit.clone();
    forged.claim_id = "forged".into();
    assert!(store.validate_permit(&forged).is_err());
    store.revoke_permission(&record.id, &binding).unwrap();
    assert!(store.validate_permit(&permit).is_err());
    let count = store.events(&binding.task_id).unwrap().len();
    store.revoke_permission(&record.id, &binding).unwrap();
    assert_eq!(store.events(&binding.task_id).unwrap().len(), count);
    assert!(
        store
            .decide_permission(&record.id, &binding, PermissionDecision::Approve)
            .is_err()
    );
    assert_eq!(
        store.request_permission(binding, deadline).unwrap().state,
        PermissionState::Revoked
    );
}

#[test]
fn changed_scope_identity_or_expiration_cannot_reuse_approval() {
    let store = TaskStore::open(":memory:").unwrap();
    let binding = setup(&store);
    let deadline = expires();
    let record = store.request_permission(binding.clone(), deadline).unwrap();
    store
        .decide_permission(&record.id, &binding, PermissionDecision::Approve)
        .unwrap();
    let mut variants = Vec::new();
    let mut changed = binding.clone();
    changed.session_id = "another".into();
    variants.push(changed);
    let mut changed = binding.clone();
    changed.task_id = "another".into();
    variants.push(changed);
    let mut changed = binding.clone();
    changed.call_id = "another".into();
    variants.push(changed);
    let mut changed = binding.clone();
    changed.scope.workspace_id = "another".into();
    variants.push(changed);
    let mut changed = binding.clone();
    changed.scope.relative_path = "other.md".into();
    variants.push(changed);
    let mut changed = binding.clone();
    changed.scope.operation = FileOperation::Read;
    variants.push(changed);
    let mut changed = binding.clone();
    changed.scope.operation = FileOperation::Create {
        content_sha256: "b".repeat(64),
    };
    variants.push(changed);
    for changed in variants {
        assert!(matches!(
            store.claim_permission(&record.id, &changed),
            Err(PermissionError::BindingMismatch)
        ));
        assert!(store.revoke_permission(&record.id, &changed).is_err());
    }
    assert!(
        store
            .request_permission(binding.clone(), deadline + 1)
            .is_err()
    );
    store.claim_permission(&record.id, &binding).unwrap();
}

#[test]
fn denied_request_cannot_be_approved_or_recreated() {
    let store = TaskStore::open(":memory:").unwrap();
    let binding = setup(&store);
    let deadline = expires();
    let record = store.request_permission(binding.clone(), deadline).unwrap();
    store
        .decide_permission(&record.id, &binding, PermissionDecision::Deny)
        .unwrap();
    assert!(
        store
            .decide_permission(&record.id, &binding, PermissionDecision::Approve)
            .is_err()
    );
    assert!(store.claim_permission(&record.id, &binding).is_err());
    assert_eq!(
        store.request_permission(binding, deadline).unwrap().state,
        PermissionState::Denied
    );
}

#[test]
fn expired_approval_and_expired_claim_are_rejected_using_store_clock() {
    let file = DatabaseFile::new();
    let store = TaskStore::open(&file.0).unwrap();
    let binding = setup(&store);
    assert!(store.request_permission(binding.clone(), 1).is_err());
    assert!(store.request_permission(binding.clone(), i64::MAX).is_err());
    let record = store
        .request_permission(binding.clone(), expires())
        .unwrap();
    store
        .decide_permission(&record.id, &binding, PermissionDecision::Approve)
        .unwrap();
    let connection = rusqlite::Connection::open(&file.0).unwrap();
    connection
        .execute("UPDATE permissions SET expires_at=1", [])
        .unwrap();
    assert!(matches!(
        store.claim_permission(&record.id, &binding),
        Err(PermissionError::Expired)
    ));
    connection
        .execute("UPDATE permissions SET expires_at=?1", [expires()])
        .unwrap();
    let permit = store.claim_permission(&record.id, &binding).unwrap();
    connection
        .execute("UPDATE permissions SET expires_at=1", [])
        .unwrap();
    assert!(matches!(
        store.validate_permit(&permit),
        Err(PermissionError::Expired)
    ));
    store.revoke_permission(&record.id, &binding).unwrap();
}

#[test]
fn cancellation_and_restart_invalidate_claims_without_reexecuting_calls() {
    for recover in [false, true] {
        let store = TaskStore::open(":memory:").unwrap();
        let binding = setup(&store);
        let record = store
            .request_permission(binding.clone(), expires())
            .unwrap();
        store
            .decide_permission(&record.id, &binding, PermissionDecision::Approve)
            .unwrap();
        let permit = store.claim_permission(&record.id, &binding).unwrap();
        if recover {
            store.recover_interrupted().unwrap();
        } else {
            store.cancel(&binding.task_id).unwrap();
        }
        assert!(matches!(
            store.validate_permit(&permit),
            Err(PermissionError::InvocationUnavailable)
        ));
        store.revoke_permission(&record.id, &binding).unwrap();
    }
}

#[test]
fn current_tool_parameters_are_rechecked_before_claim() {
    let file = DatabaseFile::new();
    let store = TaskStore::open(&file.0).unwrap();
    let binding = setup(&store);
    let record = store
        .request_permission(binding.clone(), expires())
        .unwrap();
    store
        .decide_permission(&record.id, &binding, PermissionDecision::Approve)
        .unwrap();
    let connection = rusqlite::Connection::open(&file.0).unwrap();
    connection
        .execute("UPDATE tool_calls SET arguments='{}'", [])
        .unwrap();
    assert!(matches!(
        store.claim_permission(&record.id, &binding),
        Err(PermissionError::BindingMismatch)
    ));
}

#[test]
fn audit_failure_rolls_back_decision_and_claim() {
    let file = DatabaseFile::new();
    let store = TaskStore::open(&file.0).unwrap();
    let binding = setup(&store);
    let record = store
        .request_permission(binding.clone(), expires())
        .unwrap();
    let connection = rusqlite::Connection::open(&file.0).unwrap();
    let count = store.events(&binding.task_id).unwrap().len();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_audit BEFORE INSERT ON events
        BEGIN SELECT RAISE(ABORT,'injected failure'); END;",
        )
        .unwrap();
    assert!(matches!(
        store.decide_permission(&record.id, &binding, PermissionDecision::Approve),
        Err(PermissionError::Storage)
    ));
    let state: String = connection
        .query_row("SELECT state FROM permissions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(state, "pending");
    assert_eq!(store.events(&binding.task_id).unwrap().len(), count);
    let audit_count: i64 = connection
        .query_row("SELECT count(*) FROM permission_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(audit_count, 1);
    connection.execute_batch("DROP TRIGGER fail_audit").unwrap();
    store
        .decide_permission(&record.id, &binding, PermissionDecision::Approve)
        .unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_audit BEFORE INSERT ON events
        BEGIN SELECT RAISE(ABORT,'injected failure'); END;",
        )
        .unwrap();
    assert!(store.claim_permission(&record.id, &binding).is_err());
    let saved: (String, Option<String>) = connection
        .query_row("SELECT state,claim_id FROM permissions", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(saved, ("approved".into(), None));
}

#[test]
fn concurrent_connections_cannot_claim_twice() {
    let file = DatabaseFile::new();
    let store = TaskStore::open(&file.0).unwrap();
    let binding = setup(&store);
    let record = store
        .request_permission(binding.clone(), expires())
        .unwrap();
    store
        .decide_permission(&record.id, &binding, PermissionDecision::Approve)
        .unwrap();
    let second = TaskStore::open(&file.0).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let handles = [store, second]
        .into_iter()
        .map(|store| {
            let binding = binding.clone();
            let id = record.id.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.claim_permission(&id, &binding).is_ok()
            })
        })
        .collect::<Vec<_>>();
    let successes = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .filter(|value| *value)
        .count();
    assert_eq!(successes, 1);
}
