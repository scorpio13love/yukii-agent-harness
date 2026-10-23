use crate::model::{ToolCall, ToolResult};
use thiserror::Error;
use yukii_agent_domain::TaskError;
use yukii_contracts::{Artifact, CreateTask, Task, TaskEvent, TaskStatus};

#[derive(Debug, Error)]
pub enum RepositoryError {
    #[error("当前会话仍在执行，请等待完成或取消后再发送")]
    ConversationBusy,
    #[error(transparent)]
    Embedding(#[from] crate::embedding::EmbeddingError),
    #[error("{0}")]
    InvalidRequest(&'static str),
    #[error(transparent)]
    Validation(#[from] TaskError),
    #[error("资源不存在")]
    NotFound,
    #[error("同一幂等键已用于不同请求")]
    IdempotencyConflict,
    #[error("任务队列已满，请稍后重试")]
    QueueFull,
    #[error("模型未配置，请检查 .env 并重启服务")]
    Unavailable,
    #[error("服务正在关闭，暂不接受新任务")]
    ShuttingDown,
    #[error("任务存储操作失败")]
    Backend,
}

pub struct CreatedTask {
    pub task: Task,
    pub replayed: bool,
}

/// 一次提交工具结果及数据库内副作用；禁止先创建文档再单独保存回执。
pub enum ToolCompletion {
    Result(ToolResult),
    Document {
        title: String,
        content: String,
    },
    /// 二进制产物以 base64 存入现有 artifacts.content；metadata 记录媒体类型和文件名。
    Artifact {
        title: String,
        filename: String,
        media_type: String,
        base64: String,
    },
    Plan {
        steps: Vec<String>,
    },
}

/// 同步存储端口：调用方必须通过阻塞线程池访问，驱动错误不穿透到领域层。
pub trait TaskRepository: Send + Sync {
    fn create_once(
        &self,
        input: CreateTask,
        key: &str,
        enqueue: bool,
    ) -> Result<CreatedTask, RepositoryError>;
    fn conversations(&self) -> Result<Vec<Task>, RepositoryError>;
    fn conversation(&self, id: &str) -> Result<Vec<Task>, RepositoryError>;
    fn list(&self) -> Result<Vec<Task>, RepositoryError>;
    fn get(&self, id: &str) -> Result<Task, RepositoryError>;
    fn events(&self, id: &str) -> Result<Vec<TaskEvent>, RepositoryError>;
    fn advance(
        &self,
        id: &str,
        next: TaskStatus,
        message: &str,
        result: Option<String>,
        error: Option<String>,
    ) -> Result<Task, RepositoryError>;
    fn save_sources(
        &self,
        id: &str,
        sources: &[yukii_contracts::knowledge::KnowledgeHit],
    ) -> Result<(), RepositoryError>;
    fn record(&self, id: &str, message: &str) -> Result<(), RepositoryError>;
    fn record_delta(&self, id: &str, channel: &str, delta: &str) -> Result<(), RepositoryError>;
    fn add_usage(
        &self,
        id: &str,
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
        context_window: u64,
    ) -> Result<(), RepositoryError>;
    fn document(&self, id: &str, title: &str, content: &str) -> Result<Artifact, RepositoryError>;
    fn artifacts(&self, id: &str) -> Result<Vec<Artifact>, RepositoryError>;
    fn artifact(&self, id: &str) -> Result<Artifact, RepositoryError>;
    fn artifact_file(&self, id: &str) -> Option<std::path::PathBuf>;
    fn record_task_attachments(
        &self,
        task_id: &str,
        attachments: &[serde_json::Value],
    ) -> Result<(), RepositoryError>;
    fn task_attachments(&self, task_id: &str) -> Result<Vec<serde_json::Value>, RepositoryError>;
    fn task_tool_calls(&self, task_id: &str) -> Result<Vec<serde_json::Value>, RepositoryError>;
    fn tool_call_images(&self, task_id: &str) -> Result<Vec<serde_json::Value>, RepositoryError>;
    fn tool_image(
        &self,
        task_id: &str,
        call_id: &str,
        ordinal: i64,
    ) -> Result<Option<(String, String)>, RepositoryError>;
    fn delete_session(&self, session_id: &str) -> Result<usize, RepositoryError>;
    fn session_history(
        &self,
        session_id: &str,
        before_task: &str,
    ) -> Result<Vec<crate::model::Message>, RepositoryError>;
    fn fork_session(&self, source: &str) -> Result<String, RepositoryError>;
    fn session_events(&self, session_id: &str) -> Result<Vec<serde_json::Value>, RepositoryError>;
    fn record_approval_asked(
        &self,
        approval_id: &str,
        task_id: &str,
        app_id: &str,
        application: &str,
    ) -> Result<(), RepositoryError>;
    fn record_approval(
        &self,
        task_id: &str,
        app_id: &str,
        application: &str,
        allowed: bool,
    ) -> Result<(), RepositoryError>;
    fn search_sessions(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<serde_json::Value>, bool), RepositoryError>;
    fn session_workspace(
        &self,
        session_id: &str,
    ) -> Result<Option<serde_json::Value>, RepositoryError>;
    fn session_workspaces(&self) -> Result<Vec<serde_json::Value>, RepositoryError>;
    fn set_session_workspace(
        &self,
        session_id: &str,
        path: &str,
        name: &str,
        kind: &str,
    ) -> Result<serde_json::Value, RepositoryError>;
    fn clear_session_workspace(&self, session_id: &str) -> Result<(), RepositoryError>;
    fn session_mode(&self, session_id: &str) -> Result<Option<serde_json::Value>, RepositoryError>;
    fn set_goal(
        &self,
        session_id: &str,
        objective: &str,
        max_rounds: i64,
    ) -> Result<serde_json::Value, RepositoryError>;
    fn set_goal_phase(
        &self,
        session_id: &str,
        phase: &str,
    ) -> Result<serde_json::Value, RepositoryError>;
    fn clear_goal(&self, session_id: &str) -> Result<serde_json::Value, RepositoryError>;
    fn set_plan_active(
        &self,
        session_id: &str,
        active: bool,
    ) -> Result<serde_json::Value, RepositoryError>;
    fn set_plan_pending(
        &self,
        session_id: &str,
        plan: &str,
    ) -> Result<serde_json::Value, RepositoryError>;
    fn clear_plan_pending(&self, session_id: &str) -> Result<serde_json::Value, RepositoryError>;
    fn increment_goal_round(&self, session_id: &str) -> Result<serde_json::Value, RepositoryError>;
    fn active_goals(&self) -> Result<Vec<serde_json::Value>, RepositoryError>;
    #[allow(clippy::too_many_arguments)]
    fn add_compaction(
        &self,
        conversation_id: &str,
        through_turn: usize,
        summary: &str,
        trigger: &str,
        retained_turns: usize,
        estimated_tokens: usize,
        summary_source: &str,
        duration_ms: u64,
    ) -> Result<serde_json::Value, RepositoryError>;
    fn latest_compaction(
        &self,
        conversation_id: &str,
    ) -> Result<Option<serde_json::Value>, RepositoryError>;
    fn compactions(&self, conversation_id: &str)
    -> Result<Vec<serde_json::Value>, RepositoryError>;
    fn create_schedule(
        &self,
        conversation_id: Option<&str>,
        prompt: &str,
        target: &str,
        run_at: i64,
        repeat_seconds: i64,
        max_attempts: i64,
    ) -> Result<serde_json::Value, RepositoryError>;
    fn schedules(&self) -> Result<Vec<serde_json::Value>, RepositoryError>;
    fn cancel_schedule(&self, id: &str) -> Result<(), RepositoryError>;
    fn create_child_task(
        &self,
        prompt: &str,
        target: yukii_contracts::ExecutionTarget,
    ) -> Result<yukii_contracts::Task, RepositoryError>;
    fn create_child_turn(
        &self,
        session_id: &str,
        prompt: &str,
        target: yukii_contracts::ExecutionTarget,
    ) -> Result<yukii_contracts::Task, RepositoryError>;
    fn record_subagent_catalog(
        &self,
        parent_task_id: &str,
        run_id: &str,
        child_session_id: &str,
        prompt: &str,
    ) -> Result<(), RepositoryError>;
    fn subagent_run(&self, run_id: &str) -> Result<Option<serde_json::Value>, RepositoryError>;
    fn begin_subagent(
        &self,
        parent_task_id: &str,
        parent_call_id: &str,
        prompt: &str,
        child_session_id: &str,
        child_task_id: &str,
    ) -> Result<String, RepositoryError>;
    fn finish_subagent(
        &self,
        id: &str,
        result: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), RepositoryError>;
    fn subagents_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<serde_json::Value>, RepositoryError>;
    fn subagents(&self, task_id: &str) -> Result<Vec<serde_json::Value>, RepositoryError>;
    fn set_schedule_cron(&self, id: &str, expression: &str) -> Result<(), RepositoryError>;
    fn artifact_content(&self, id: &str) -> Result<String, RepositoryError>;
    fn cancel(&self, id: &str) -> Result<Task, RepositoryError>;
    fn enqueue(&self, id: &str) -> Result<Task, RepositoryError>;
    fn claim_next(&self) -> Result<Option<Task>, RepositoryError>;
    /// 只有数据库中从未领取的队列条目可以在重启后恢复。
    fn recover_interrupted(&self) -> Result<usize, RepositoryError>;
    fn begin_tool(
        &self,
        id: &str,
        call: &ToolCall,
        schema: Option<&str>,
        parent_call_id: Option<&str>,
        depth: u32,
    ) -> Result<Option<ToolResult>, RepositoryError>;
    fn finish_tool(
        &self,
        id: &str,
        call: &ToolCall,
        result: &ToolResult,
    ) -> Result<(), RepositoryError>;
    fn complete_tool(
        &self,
        id: &str,
        call: &ToolCall,
        completion: ToolCompletion,
    ) -> Result<ToolResult, RepositoryError>;
}
