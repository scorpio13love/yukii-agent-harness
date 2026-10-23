use crate::StoreError;
use rusqlite::{Connection, TransactionBehavior};

/// 迁移与版本登记同一事务提交；拒绝旧二进制打开更新的数据格式。
pub(crate) fn apply(connection: &mut Connection) -> Result<(), StoreError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations(version INTEGER PRIMARY KEY);",
    )?;
    let version: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(version),0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    let migrations = [
        include_str!("../migrations/0001_tasks.sql"),
        include_str!("../migrations/0002_execution.sql"),
        include_str!("../migrations/0003_knowledge_plugins.sql"),
        include_str!("../migrations/0004_embeddings.sql"),
        include_str!("../migrations/0005_invocations.sql"),
        include_str!("../migrations/0006_permissions.sql"),
        include_str!("../migrations/0007_desktop_receipts.sql"),
        include_str!("../migrations/0008_conversations.sql"),
        include_str!("../migrations/0009_computer_use.sql"),
        include_str!("../migrations/0010_task_attachments.sql"),
        include_str!("../migrations/0011_tool_timing.sql"),
        include_str!("../migrations/0012_tool_schema.sql"),
        include_str!("../migrations/0013_compactions.sql"),
        include_str!("../migrations/0014_subtools.sql"),
        include_str!("../migrations/0015_compaction_meta.sql"),
        include_str!("../migrations/0016_compaction_duration.sql"),
        include_str!("../migrations/0017_schedules.sql"),
        include_str!("../migrations/0018_schedule_repeat.sql"),
        include_str!("../migrations/0019_schedule_retry.sql"),
        include_str!("../migrations/0020_subagent_runs.sql"),
        include_str!("../migrations/0021_scheduled_cron.sql"),
        include_str!("../migrations/0022_tool_images.sql"),
        include_str!("../migrations/0023_session_workspaces.sql"),
        include_str!("../migrations/0024_session_outbox.sql"),
        include_str!("../migrations/0025_session_search_fts.sql"),
        include_str!("../migrations/0026_subagent_sessions.sql"),
        include_str!("../migrations/0027_session_workspace_kind.sql"),
        include_str!("../migrations/0028_session_modes.sql"),
        include_str!("../migrations/0029_pending_plan.sql"),
    ];
    if version > migrations.len() as i64 {
        return Err(StoreError::SchemaTooNew);
    }
    for (index, sql) in migrations.iter().enumerate() {
        let next = index as i64 + 1;
        if next > version {
            transaction.execute_batch(sql)?;
            transaction.execute("INSERT INTO schema_migrations(version) VALUES(?1)", [next])?;
        }
    }
    transaction.commit()?;
    Ok(())
}
