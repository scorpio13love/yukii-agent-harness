//! 基础设施实现与领域端口适配。
mod desktop_receipts;
mod instance;
pub use desktop_receipts::DesktopReceipt;
mod computer;
mod invocations;
mod knowledge;
mod migrations;
mod permissions;
mod plugins;
mod session_log;
mod sqlite;
mod vectors;
pub use instance::InstanceLock;
pub use sqlite::{StoreError, TaskStore};
use yukii_agent_ports::{
    computer::{ComputerRepository, ComputerRepositoryError},
    model::{ToolCall, ToolResult},
    repository::{CreatedTask, RepositoryError, TaskRepository, ToolCompletion},
};
use yukii_contracts::{Artifact, CreateTask, Task, TaskEvent, TaskStatus};
impl From<StoreError> for RepositoryError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::ConversationBusy => Self::ConversationBusy,
            StoreError::Validation(value) => Self::Validation(value),
            StoreError::NotFound => Self::NotFound,
            StoreError::Unavailable => Self::Unavailable,
            StoreError::IdempotencyConflict => Self::IdempotencyConflict,
            StoreError::QueueFull => Self::QueueFull,
            _ => Self::Backend,
        }
    }
}
impl TaskRepository for TaskStore {
    fn create_once(
        &self,
        input: CreateTask,
        key: &str,
        enqueue: bool,
    ) -> Result<CreatedTask, RepositoryError> {
        TaskStore::create_once(self, input, key, enqueue).map_err(Into::into)
    }
    fn conversations(&self) -> Result<Vec<Task>, RepositoryError> {
        TaskStore::conversations(self).map_err(Into::into)
    }
    fn conversation(&self, id: &str) -> Result<Vec<Task>, RepositoryError> {
        TaskStore::conversation(self, id).map_err(Into::into)
    }
    fn list(&self) -> Result<Vec<Task>, RepositoryError> {
        TaskStore::list(self).map_err(Into::into)
    }
    fn get(&self, id: &str) -> Result<Task, RepositoryError> {
        TaskStore::get(self, id).map_err(Into::into)
    }
    fn events(&self, id: &str) -> Result<Vec<TaskEvent>, RepositoryError> {
        TaskStore::events(self, id).map_err(Into::into)
    }
    fn advance(
        &self,
        id: &str,
        next: TaskStatus,
        message: &str,
        result: Option<String>,
        error: Option<String>,
    ) -> Result<Task, RepositoryError> {
        TaskStore::advance(self, id, next, message, result, error).map_err(Into::into)
    }
    fn save_sources(
        &self,
        id: &str,
        sources: &[yukii_contracts::knowledge::KnowledgeHit],
    ) -> Result<(), RepositoryError> {
        TaskStore::save_sources(self, id, sources).map_err(Into::into)
    }
    fn record(&self, id: &str, message: &str) -> Result<(), RepositoryError> {
        TaskStore::record(self, id, message).map_err(Into::into)
    }
    fn record_delta(&self, id: &str, channel: &str, delta: &str) -> Result<(), RepositoryError> {
        TaskStore::record_delta(self, id, channel, delta).map_err(Into::into)
    }

    fn add_usage(
        &self,
        id: &str,
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
        context_window: u64,
    ) -> Result<(), RepositoryError> {
        TaskStore::add_usage(
            self,
            id,
            input,
            output,
            cache_read,
            cache_write,
            context_window,
        )
        .map_err(Into::into)
    }
    fn document(&self, id: &str, title: &str, content: &str) -> Result<Artifact, RepositoryError> {
        TaskStore::document(self, id, title, content).map_err(Into::into)
    }
    fn artifacts(&self, id: &str) -> Result<Vec<Artifact>, RepositoryError> {
        TaskStore::artifacts(self, id).map_err(Into::into)
    }
    fn artifact_file(&self, id: &str) -> Option<std::path::PathBuf> {
        TaskStore::artifact_file(self, id)
    }

    fn record_task_attachments(
        &self,
        task_id: &str,
        attachments: &[serde_json::Value],
    ) -> Result<(), RepositoryError> {
        TaskStore::record_task_attachments(self, task_id, attachments).map_err(Into::into)
    }

    fn task_attachments(&self, task_id: &str) -> Result<Vec<serde_json::Value>, RepositoryError> {
        TaskStore::task_attachments(self, task_id).map_err(Into::into)
    }

    fn task_tool_calls(&self, task_id: &str) -> Result<Vec<serde_json::Value>, RepositoryError> {
        TaskStore::task_tool_calls(self, task_id).map_err(Into::into)
    }

    fn tool_call_images(&self, task_id: &str) -> Result<Vec<serde_json::Value>, RepositoryError> {
        TaskStore::tool_call_images(self, task_id).map_err(Into::into)
    }

    fn tool_image(
        &self,
        task_id: &str,
        call_id: &str,
        ordinal: i64,
    ) -> Result<Option<(String, String)>, RepositoryError> {
        TaskStore::tool_image(self, task_id, call_id, ordinal).map_err(Into::into)
    }

    fn delete_session(&self, session_id: &str) -> Result<usize, RepositoryError> {
        TaskStore::delete_session(self, session_id).map_err(Into::into)
    }

    fn session_history(
        &self,
        session_id: &str,
        before_task: &str,
    ) -> Result<Vec<yukii_agent_ports::model::Message>, RepositoryError> {
        TaskStore::session_history(self, session_id, before_task).map_err(Into::into)
    }

    fn fork_session(&self, source: &str) -> Result<String, RepositoryError> {
        TaskStore::fork_session(self, source).map_err(Into::into)
    }

    fn session_events(&self, session_id: &str) -> Result<Vec<serde_json::Value>, RepositoryError> {
        TaskStore::session_events(self, session_id).map_err(Into::into)
    }

    fn record_approval_asked(
        &self,
        approval_id: &str,
        task_id: &str,
        app_id: &str,
        application: &str,
    ) -> Result<(), RepositoryError> {
        TaskStore::record_approval_asked(self, approval_id, task_id, app_id, application)
            .map_err(Into::into)
    }

    fn record_approval(
        &self,
        task_id: &str,
        app_id: &str,
        application: &str,
        allowed: bool,
    ) -> Result<(), RepositoryError> {
        TaskStore::record_approval(self, task_id, app_id, application, allowed).map_err(Into::into)
    }

    fn search_sessions(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<serde_json::Value>, bool), RepositoryError> {
        TaskStore::search_sessions(self, query, limit, offset).map_err(Into::into)
    }

    fn session_workspace(
        &self,
        session_id: &str,
    ) -> Result<Option<serde_json::Value>, RepositoryError> {
        TaskStore::session_workspace(self, session_id).map_err(Into::into)
    }

    fn set_session_workspace(
        &self,
        session_id: &str,
        path: &str,
        name: &str,
    ) -> Result<serde_json::Value, RepositoryError> {
        TaskStore::set_session_workspace(self, session_id, path, name).map_err(Into::into)
    }

    fn clear_session_workspace(&self, session_id: &str) -> Result<(), RepositoryError> {
        TaskStore::clear_session_workspace(self, session_id).map_err(Into::into)
    }

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
    ) -> Result<serde_json::Value, RepositoryError> {
        TaskStore::add_compaction(
            self,
            conversation_id,
            through_turn,
            summary,
            trigger,
            retained_turns,
            estimated_tokens,
            summary_source,
            duration_ms,
        )
        .map_err(Into::into)
    }

    fn latest_compaction(
        &self,
        conversation_id: &str,
    ) -> Result<Option<serde_json::Value>, RepositoryError> {
        TaskStore::latest_compaction(self, conversation_id).map_err(Into::into)
    }

    fn compactions(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<serde_json::Value>, RepositoryError> {
        TaskStore::compactions(self, conversation_id).map_err(Into::into)
    }

    fn create_schedule(
        &self,
        conversation_id: Option<&str>,
        prompt: &str,
        target: &str,
        run_at: i64,
        repeat_seconds: i64,
        max_attempts: i64,
    ) -> Result<serde_json::Value, RepositoryError> {
        TaskStore::create_schedule(
            self,
            conversation_id,
            prompt,
            target,
            run_at,
            repeat_seconds,
            max_attempts,
        )
        .map_err(Into::into)
    }

    fn schedules(&self) -> Result<Vec<serde_json::Value>, RepositoryError> {
        TaskStore::schedules(self).map_err(Into::into)
    }

    fn cancel_schedule(&self, id: &str) -> Result<(), RepositoryError> {
        TaskStore::cancel_schedule(self, id).map_err(Into::into)
    }

    fn create_child_task(
        &self,
        prompt: &str,
        target: yukii_contracts::ExecutionTarget,
    ) -> Result<yukii_contracts::Task, RepositoryError> {
        TaskStore::create_child_task(self, prompt, target).map_err(Into::into)
    }

    fn create_child_turn(
        &self,
        session_id: &str,
        prompt: &str,
        target: yukii_contracts::ExecutionTarget,
    ) -> Result<yukii_contracts::Task, RepositoryError> {
        TaskStore::create_child_turn(self, session_id, prompt, target).map_err(Into::into)
    }

    fn record_subagent_catalog(
        &self,
        parent_task_id: &str,
        run_id: &str,
        child_session_id: &str,
        prompt: &str,
    ) -> Result<(), RepositoryError> {
        TaskStore::record_subagent_catalog(self, parent_task_id, run_id, child_session_id, prompt)
            .map_err(Into::into)
    }

    fn subagent_run(&self, run_id: &str) -> Result<Option<serde_json::Value>, RepositoryError> {
        TaskStore::subagent_run(self, run_id).map_err(Into::into)
    }

    fn begin_subagent(
        &self,
        parent_task_id: &str,
        parent_call_id: &str,
        prompt: &str,
        child_session_id: &str,
        child_task_id: &str,
    ) -> Result<String, RepositoryError> {
        TaskStore::begin_subagent(
            self,
            parent_task_id,
            parent_call_id,
            prompt,
            child_session_id,
            child_task_id,
        )
        .map_err(Into::into)
    }

    fn finish_subagent(
        &self,
        id: &str,
        result: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), RepositoryError> {
        TaskStore::finish_subagent(self, id, result, error).map_err(Into::into)
    }

    fn subagents_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<serde_json::Value>, RepositoryError> {
        TaskStore::subagents_for_session(self, session_id).map_err(Into::into)
    }

    fn subagents(&self, task_id: &str) -> Result<Vec<serde_json::Value>, RepositoryError> {
        TaskStore::subagents(self, task_id).map_err(Into::into)
    }

    fn set_schedule_cron(&self, id: &str, expression: &str) -> Result<(), RepositoryError> {
        TaskStore::set_schedule_cron(self, id, expression).map_err(Into::into)
    }

    fn artifact(&self, id: &str) -> Result<Artifact, RepositoryError> {
        TaskStore::artifact(self, id).map_err(Into::into)
    }

    fn artifact_content(&self, id: &str) -> Result<String, RepositoryError> {
        TaskStore::artifact_content(self, id).map_err(Into::into)
    }
    fn cancel(&self, id: &str) -> Result<Task, RepositoryError> {
        TaskStore::cancel(self, id).map_err(Into::into)
    }
    fn enqueue(&self, id: &str) -> Result<Task, RepositoryError> {
        TaskStore::enqueue(self, id).map_err(Into::into)
    }
    fn claim_next(&self) -> Result<Option<Task>, RepositoryError> {
        TaskStore::claim_next(self).map_err(Into::into)
    }
    fn recover_interrupted(&self) -> Result<usize, RepositoryError> {
        TaskStore::recover_interrupted(self).map_err(Into::into)
    }
    fn begin_tool(
        &self,
        id: &str,
        call: &ToolCall,
        schema: Option<&str>,
        parent_call_id: Option<&str>,
        depth: u32,
    ) -> Result<Option<ToolResult>, RepositoryError> {
        TaskStore::begin_tool(self, id, call, schema, parent_call_id, depth).map_err(Into::into)
    }
    fn finish_tool(
        &self,
        id: &str,
        call: &ToolCall,
        result: &ToolResult,
    ) -> Result<(), RepositoryError> {
        TaskStore::finish_tool(self, id, call, result).map_err(Into::into)
    }
    fn complete_tool(
        &self,
        id: &str,
        call: &ToolCall,
        completion: ToolCompletion,
    ) -> Result<ToolResult, RepositoryError> {
        TaskStore::complete_tool(self, id, call, completion).map_err(Into::into)
    }
}

impl ComputerRepository for TaskStore {
    fn start_session(
        &self,
        session_id: &str,
        control: &yukii_contracts::desktop::ControlSession,
    ) -> Result<(), ComputerRepositoryError> {
        TaskStore::start_computer_session(self, session_id, control).map_err(computer_error)
    }
    fn set_session_state(
        &self,
        control_id: &str,
        state: yukii_contracts::desktop::ControlState,
    ) -> Result<yukii_contracts::desktop::ControlSession, ComputerRepositoryError> {
        TaskStore::set_computer_session_state(self, control_id, state).map_err(computer_error)
    }
    fn record_observation(
        &self,
        control_id: &str,
        observation: &yukii_contracts::desktop::DesktopObservation,
        image_sha256: &str,
    ) -> Result<(), ComputerRepositoryError> {
        TaskStore::record_computer_observation(self, control_id, observation, image_sha256)
            .map_err(computer_error)
    }
    fn begin_action(
        &self,
        control_id: &str,
        observation_id: &str,
        task_id: &str,
        call_id: &str,
        action: &yukii_contracts::desktop::GuiAction,
        action_id: &str,
    ) -> Result<(), ComputerRepositoryError> {
        TaskStore::begin_computer_action(
            self,
            control_id,
            observation_id,
            task_id,
            call_id,
            action,
            action_id,
        )
        .map_err(computer_error)
    }
    fn finish_action(
        &self,
        action_id: &str,
        state: &str,
        result: Option<&str>,
    ) -> Result<(), ComputerRepositoryError> {
        TaskStore::finish_computer_action(self, action_id, state, result).map_err(computer_error)
    }
    fn close_stale_sessions(&self, session_id: &str) -> Result<usize, ComputerRepositoryError> {
        TaskStore::close_stale_computer_sessions(self, session_id).map_err(computer_error)
    }
}

fn computer_error(error: StoreError) -> ComputerRepositoryError {
    match error {
        StoreError::ControlUnavailable => ComputerRepositoryError::ControlUnavailable,
        StoreError::ObservationUnavailable => ComputerRepositoryError::ObservationUnavailable,
        StoreError::Validation(_) => ComputerRepositoryError::TaskInactive,
        StoreError::IdempotencyConflict => ComputerRepositoryError::Conflict,
        StoreError::NotFound => ComputerRepositoryError::ControlUnavailable,
        _ => ComputerRepositoryError::Backend,
    }
}
