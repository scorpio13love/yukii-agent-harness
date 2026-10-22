//! 本机执行结果交付账本。记录与事件原子保存，文件系统副作用不在事务内。
use crate::{
    StoreError, TaskStore,
    sqlite::{append_event, read_task},
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use yukii_contracts::{desktop::DesktopExecutionResult, permissions::PermissionBinding};

pub enum DesktopReceipt {
    New,
    Completed(DesktopExecutionResult),
    OutcomeUnknown,
}

impl TaskStore {
    /// 同一审批 ID、身份和请求摘要才可重放。未完成记录不能重新成为 New。
    pub fn begin_desktop_execution(
        &self,
        permission_id: &str,
        binding: &PermissionBinding,
        digest: &str,
    ) -> Result<DesktopReceipt, StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored_binding: String = transaction
            .query_row(
                "SELECT binding FROM permissions WHERE id=?1",
                [permission_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        let stored_binding: PermissionBinding = serde_json::from_str(&stored_binding)?;
        if &stored_binding != binding {
            return Err(StoreError::IdempotencyConflict);
        }
        let existing: Option<(String,String,Option<String>)> = transaction.query_row(
            "SELECT session_id,request_digest,result FROM desktop_receipts WHERE permission_id=?1",
            [permission_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).optional()?;
        if let Some((session, previous_digest, result)) = existing {
            if session != binding.session_id || previous_digest != digest {
                return Err(StoreError::IdempotencyConflict);
            }
            return match result {
                Some(result) => Ok(DesktopReceipt::Completed(serde_json::from_str(&result)?)),
                None => Ok(DesktopReceipt::OutcomeUnknown),
            };
        }
        transaction.execute(
            "INSERT INTO desktop_receipts(permission_id,session_id,request_digest) VALUES(?1,?2,?3)",
            params![permission_id,binding.session_id,digest],
        )?;
        let task = read_task(&transaction, &binding.task_id)?;
        append_event(&transaction, &task, "本机执行请求已登记，尚无执行结果。")?;
        transaction.commit()?;
        Ok(DesktopReceipt::New)
    }

    /// 即使任务在外部操作后被取消，也保留真实结果用于核对；不改写任务终态。
    pub fn finish_desktop_execution(
        &self,
        permission_id: &str,
        session_id: &str,
        digest: &str,
        result: &DesktopExecutionResult,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE desktop_receipts SET result=?1 WHERE permission_id=?2 AND session_id=?3 AND request_digest=?4 AND result IS NULL",
            params![serde_json::to_string(result)?,permission_id,session_id,digest],
        )?;
        if changed != 1 {
            return Err(StoreError::IdempotencyConflict);
        }
        let task_id: String = transaction.query_row(
            "SELECT task_id FROM permissions WHERE id=?1",
            [permission_id],
            |row| row.get(0),
        )?;
        let task = read_task(&transaction, &task_id)?;
        let message = match result {
            DesktopExecutionResult::Completed { .. } => "本机操作结果已保存，等待宿主接收。",
            DesktopExecutionResult::Rejected { .. } => "本机操作未完成，错误结果已保存。",
            DesktopExecutionResult::OutcomeUnknown { .. } => "本机操作结果不确定，已保存核对记录。",
        };
        append_event(&transaction, &task, message)?;
        transaction.commit()?;
        Ok(())
    }

    /// ACK 只确认结果交付，不删除回执；迟到重试仍能读取原结果而不重跑操作。
    pub fn acknowledge_desktop_result(
        &self,
        permission_id: &str,
        session_id: &str,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<(bool, bool)> = transaction.query_row(
            "SELECT acknowledged,result IS NOT NULL FROM desktop_receipts WHERE permission_id=?1 AND session_id=?2",
            params![permission_id,session_id],|row|Ok((row.get(0)?,row.get(1)?)),
        ).optional()?;
        match current {
            Some((true, true)) => return Ok(()),
            Some((false, true)) => {}
            _ => return Err(StoreError::IdempotencyConflict),
        }
        transaction.execute(
            "UPDATE desktop_receipts SET acknowledged=1 WHERE permission_id=?1",
            [permission_id],
        )?;
        let task_id: String = transaction.query_row(
            "SELECT task_id FROM permissions WHERE id=?1",
            [permission_id],
            |row| row.get(0),
        )?;
        append_event(
            &transaction,
            &read_task(&transaction, &task_id)?,
            "宿主已确认接收本机操作结果。",
        )?;
        transaction.commit()?;
        Ok(())
    }
}
