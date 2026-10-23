//! 单任务工具执行上下文。工具调用登记、产物落盘与回执保存由 Runtime 统一管理。
use super::*;
use async_trait::async_trait;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use yukii_agent_core::runner::{self, AgentHost};
use yukii_agent_ports::{
    model::{Message, ToolCall, ToolDefinition, ToolResult, Usage},
    repository::ToolCompletion,
    tools::ToolContext,
};
use yukii_contracts::{CreateTask, ExecutionTarget, Task};
use yukii_tool_gateway::ToolEffect;

/// 父任务共享的子 Agent 并发/预算计量；permit 在子任务结束时释放并发槽。
#[allow(dead_code)]
pub(crate) struct SubagentBudget {
    active: AtomicUsize,
    tokens: AtomicU64,
    max_active: usize,
    max_tokens: u64,
}
#[allow(dead_code)]
impl SubagentBudget {
    pub(crate) fn new(max_active: usize, max_tokens: u64) -> Arc<Self> {
        Arc::new(Self {
            active: AtomicUsize::new(0),
            tokens: AtomicU64::new(0),
            max_active,
            max_tokens,
        })
    }
    fn try_begin(self: &Arc<Self>) -> Option<SubagentPermit> {
        loop {
            let current = self.active.load(Ordering::Acquire);
            if current >= self.max_active {
                return None;
            }
            if self
                .active
                .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(SubagentPermit {
                    budget: self.clone(),
                });
            }
        }
    }
    fn add_tokens(&self, amount: u64) -> bool {
        let total = self.tokens.fetch_add(amount, Ordering::AcqRel) + amount;
        total <= self.max_tokens
    }
}
#[allow(dead_code)]
struct SubagentPermit {
    budget: Arc<SubagentBudget>,
}
#[allow(dead_code)]
impl Drop for SubagentPermit {
    fn drop(&mut self) {
        self.budget.active.fetch_sub(1, Ordering::AcqRel);
    }
}
pub(crate) struct TaskHost {
    pub(crate) store: Arc<dyn TaskRepository>,
    pub(crate) id: String,
    pub(crate) gateway: ToolGateway,
    /// 由持久任务记录决定，不接受模型参数切换；网页创建的任务固定为云端办公。
    pub(crate) target: yukii_contracts::ExecutionTarget,
    pub(crate) model: Arc<dyn LanguageModel>,
    pub(crate) limits: AgentLimits,
    pub(crate) system: String,
    pub(crate) cancel: CancellationToken,
    /// 子 Agent 继承父工具集，但只能再嵌套一层。
    pub(crate) allow_subagent: bool,
    pub(crate) parent_call_id: Option<String>,
    pub(crate) budget: Arc<SubagentBudget>,
    pub(crate) context_window_tokens: u64,
    pub(crate) steer: Option<std::sync::Arc<subagent_inbox::ChildInbox>>,
}
async fn inherit_workspace(host: &TaskHost, child_session: &str) -> Result<(), AgentError> {
    let child_session = child_session.to_owned();
    host.io(move |store, parent_task_id| {
        let parent = store.get(parent_task_id)?;
        if let Some(workspace) = store.session_workspace(&parent.conversation_id)? {
            let path = workspace["path"].as_str().unwrap_or_default();
            let name = workspace["name"].as_str().unwrap_or("workspace");
            let kind = workspace["kind"].as_str().unwrap_or("project");
            if !path.is_empty() {
                store.set_session_workspace(&child_session, path, name, kind)?;
            }
        }
        Ok(())
    })
    .await
}

async fn notify_parent(
    store: std::sync::Arc<dyn yukii_agent_ports::repository::TaskRepository>,
    session_id: String,
    agent_id: String,
    child_task_id: String,
    target: ExecutionTarget,
    text: Option<String>,
    error: Option<String>,
) {
    let body = match (text, error) {
        (Some(text), _) => text,
        (None, Some(error)) => format!("失败：{error}"),
        (None, None) => "未产生结果".into(),
    };
    let prompt = format!(
        "[子 Agent {agent_id} 完成]\n{}\n请基于这个结果继续当前工作。",
        body.chars().take(8000).collect::<String>()
    );
    // 同一 child turn 的通知必须幂等，避免重复唤醒父级。
    let notification_key = format!("subagent-notify:{agent_id}:{child_task_id}");
    for _ in 0..120 {
        let store = store.clone();
        let session = session_id.clone();
        let prompt = prompt.clone();
        let notification_key = notification_key.clone();
        let result = tokio::task::spawn_blocking(move || {
            store.create_once(
                CreateTask {
                    conversation_id: Some(session),
                    prompt,
                    target,
                },
                &notification_key,
                true,
            )
        })
        .await;
        match result {
            Ok(Ok(_)) => return,
            Ok(Err(_)) => tokio::time::sleep(std::time::Duration::from_secs(1)).await,
            Err(_) => return,
        }
    }
}

async fn run_child_loop(
    host: TaskHost,
    mut task: Task,
    mut prompt: String,
    agent_id: String,
    parent_session: String,
    inbox: std::sync::Arc<crate::subagent_inbox::ChildInbox>,
) {
    let session_id = task.conversation_id.clone();
    let mut history = Vec::new();
    loop {
        crate::subagent_inbox::set_status(&agent_id, "running");
        let result = host
            .run_child_task(task.clone(), prompt.clone(), history, agent_id.clone())
            .await;
        crate::subagent_inbox::set_status(&agent_id, "idle");
        match &result {
            Ok(text) => {
                notify_parent(
                    host.store.clone(),
                    parent_session.clone(),
                    agent_id.clone(),
                    task.id.clone(),
                    host.target,
                    Some(text.clone()),
                    None,
                )
                .await;
            }
            Err(error) => {
                notify_parent(
                    host.store.clone(),
                    parent_session.clone(),
                    agent_id.clone(),
                    task.id.clone(),
                    host.target,
                    None,
                    Some(error.to_string()),
                )
                .await;
            }
        }
        let message = tokio::select! {
            _ = host.cancel.cancelled() => break,
            value = inbox.recv() => match value { Some(value) => value, None => break },
        };
        task = match host
            .io({
                let session_id = session_id.clone();
                let message = message.clone();
                let target = host.target;
                move |store, _| store.create_child_turn(&session_id, &message, target)
            })
            .await
        {
            Ok(task) => task,
            Err(_) => break,
        };
        history = host
            .io({
                let session_id = session_id.clone();
                let before = task.id.clone();
                move |store, _| store.session_history(&session_id, &before)
            })
            .await
            .unwrap_or_default();
        prompt = message;
    }
    crate::subagent_inbox::remove(&agent_id);
}

impl TaskHost {
    async fn io<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&dyn TaskRepository, &str) -> Result<T, RepositoryError> + Send + 'static,
    ) -> Result<T, AgentError> {
        let store = self.store.clone();
        let id = self.id.clone();
        tokio::task::spawn_blocking(move || operation(store.as_ref(), &id))
            .await
            .map_err(|_| AgentError::Host)?
            .map_err(|_| AgentError::Host)
    }
    /// 运行一个独立 child task/session，并把它推进到终态。
    async fn run_child_task(
        &self,
        child_task: Task,
        prompt: String,
        history: Vec<Message>,
        run_id: String,
    ) -> Result<String, AgentError> {
        let child_id = child_task.id.clone();
        self.io({
            let child_id = child_id.clone();
            move |store, _| {
                store.advance(
                    &child_id,
                    yukii_contracts::TaskStatus::Running,
                    "subagent running",
                    None,
                    None,
                )
            }
        })
        .await?;
        let child_cancel = CancellationToken::new();
        subagent_registry::register(&run_id, child_cancel.clone());
        let parent_cancel = self.cancel.clone();
        let child_cancel_watch = child_cancel.clone();
        tokio::spawn(async move {
            parent_cancel.cancelled().await;
            child_cancel_watch.cancel();
        });
        let child = TaskHost {
            store: self.store.clone(),
            id: child_id.clone(),
            gateway: self.gateway.clone(),
            target: self.target,
            model: self.model.clone(),
            limits: self.limits.clone(),
            system: self.system.clone(),
            cancel: child_cancel.clone(),
            allow_subagent: false,
            parent_call_id: None,
            budget: self.budget.clone(),
            context_window_tokens: self.context_window_tokens,
            steer: self.steer.clone(),
        };
        let result = runner::run_with_system(
            self.model.as_ref(),
            &child,
            &child.system,
            &prompt,
            &history,
            &self.limits,
            &child_cancel,
        )
        .await;
        let (text, error) = match &result {
            Ok(text) => (Some(text.clone()), None),
            Err(error) => (None, Some(error.to_string())),
        };
        subagent_registry::unregister(&run_id);
        self.io({
            let child_id = child_id.clone();
            let text = text.clone();
            let error = error.clone();
            move |store, _| {
                let _ = store.advance(
                    &child_id,
                    yukii_contracts::TaskStatus::Verifying,
                    "subagent verifying",
                    None,
                    None,
                );
                if let Some(error) = &error {
                    let _ = store.advance(
                        &child_id,
                        yukii_contracts::TaskStatus::Failed,
                        "subagent failed",
                        None,
                        Some(error.clone()),
                    );
                } else {
                    let _ = store.advance(
                        &child_id,
                        yukii_contracts::TaskStatus::Completed,
                        "subagent completed",
                        text.clone(),
                        None,
                    );
                }
                store.finish_subagent(&run_id, text.as_deref(), error.as_deref())
            }
        })
        .await?;
        result
    }
}
#[async_trait]
impl AgentHost for TaskHost {
    fn tools(&self) -> Vec<ToolDefinition> {
        self.gateway.definitions()
    }
    async fn progress(&self, message: &str) -> Result<(), AgentError> {
        if self.parent_call_id.is_some() || message.trim().is_empty() {
            return Ok(());
        }
        let message = message.to_owned();
        self.io(move |store, id| store.record(id, &message)).await
    }
    async fn delta(&self, channel: &str, text: &str) -> Result<(), AgentError> {
        if self.parent_call_id.is_some() || text.is_empty() {
            return Ok(());
        }
        let channel = channel.to_owned();
        let text = text.to_owned();
        self.io(move |store, id| store.record_delta(id, &channel, &text))
            .await
    }

    async fn steer(&self) -> Option<String> {
        self.steer.as_ref()?.try_recv()
    }

    async fn attachments(&self) -> Vec<yukii_agent_ports::model::ImageContent> {
        let attachments = self
            .io(|store, id| store.task_attachments(id))
            .await
            .unwrap_or_default();
        let root = std::env::var_os("YUKII_UPLOADS_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(".data/uploads"));
        let mut images = Vec::new();
        for attachment in attachments {
            let media_type = attachment["media_type"].as_str().unwrap_or("");
            if !matches!(media_type, "image/png" | "image/jpeg") {
                continue;
            }
            let file_id = attachment["file_id"].as_str().unwrap_or("");
            let session_id = attachment["session_id"].as_str().unwrap_or("");
            let directory = root.join(session_id);
            let mut path = None;
            if let Ok(entries) = std::fs::read_dir(&directory) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with(file_id) && !name.ends_with(".json") {
                        path = Some(entry.path());
                    }
                }
            }
            let Some(path) = path else { continue };
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if bytes.len() > 7 * 1024 * 1024 {
                continue;
            }
            use base64::{Engine, engine::general_purpose::STANDARD};
            images.push(yukii_agent_ports::model::ImageContent {
                media_type: media_type.into(),
                base64: STANDARD.encode(bytes),
                width: 0,
                height: 0,
            });
        }
        images
    }

    async fn usage(&self, usage: &Usage) -> Result<(), AgentError> {
        if self.parent_call_id.is_some() {
            if !self
                .budget
                .add_tokens(usage.input_tokens + usage.output_tokens)
            {
                return Err(AgentError::Budget);
            }
            return Ok(());
        }
        let usage = usage.clone();
        let context_window = self.context_window_tokens;
        self.io(move |store, id| {
            store.add_usage(
                id,
                usage.input_tokens,
                usage.output_tokens,
                usage.cache_read_tokens,
                usage.cache_write_tokens,
                context_window,
            )
        })
        .await
    }
    /// 调用 ID 与结果持久化：已完成调用复用结果，未知结果拒绝重新产生副作用。
    async fn tool(&self, call: &ToolCall) -> Result<ToolResult, AgentError> {
        let receipt = call.clone();
        let begin_receipt = receipt.clone();
        let parent_call_id = self.parent_call_id.clone();
        let depth = if parent_call_id.is_some() { 1 } else { 0 };
        let definition = self
            .gateway
            .definitions()
            .into_iter()
            .find(|tool| tool.name == call.name);
        let schema = definition.as_ref().map(|tool| tool.schema.to_string());
        if let Some(result) = self
            .io(move |store, id| {
                store.begin_tool(
                    id,
                    &begin_receipt,
                    schema.as_deref(),
                    parent_call_id.as_deref(),
                    depth,
                )
            })
            .await?
        {
            return Ok(result);
        }
        let known = definition.is_some();
        let label = if known {
            call.name.as_str()
        } else {
            "未授权工具"
        };
        self.progress(&format!("调用工具：{label}")).await?;
        let context = match self.target {
            yukii_contracts::ExecutionTarget::CloudOffice => {
                ToolContext::office(&self.id, &call.id)
            }
            yukii_contracts::ExecutionTarget::LocalComputer => {
                ToolContext::desktop(&self.id, &call.id)
            }
        };
        if call.name == "send_message" {
            if !self.allow_subagent {
                return Err(AgentError::Host);
            }
            let agent_id = call
                .arguments
                .get("agent_id")
                .and_then(|value| value.as_str())
                .ok_or(AgentError::Host)?
                .to_owned();
            let message = call
                .arguments
                .get("message")
                .and_then(|value| value.as_str())
                .ok_or(AgentError::Host)?
                .to_owned();
            if subagent_inbox::send(&agent_id, message.clone()) {
                let completion = ToolCompletion::Result(ToolResult {
                    call_id: call.id.clone(),
                    content: serde_json::json!({
                        "accepted": true,
                        "agent_id": agent_id,
                        "status": subagent_inbox::status(&agent_id).unwrap_or("idle"),
                    })
                    .to_string(),
                    is_error: false,
                    images: Vec::new(),
                });
                return self
                    .io(move |store, id| store.complete_tool(id, &receipt, completion))
                    .await
                    .map_err(|_| AgentError::Host);
            }
            let run = self
                .io({
                    let agent_id = agent_id.clone();
                    move |store, _| store.subagent_run(&agent_id)
                })
                .await?
                .ok_or(AgentError::Host)?;
            // send_message 属于同一 Session 的新一轮 task；不能要求 parent_task_id 等于当前 task。
            let session_id = run["child_session_id"]
                .as_str()
                .ok_or(AgentError::Host)?
                .to_owned();
            let child_task = self
                .io({
                    let session_id = session_id.clone();
                    let message = message.clone();
                    let target = self.target;
                    move |store, _| store.create_child_turn(&session_id, &message, target)
                })
                .await?;
            let history = self
                .io({
                    let session_id = session_id.clone();
                    let before = child_task.id.clone();
                    move |store, _| store.session_history(&session_id, &before)
                })
                .await?;
            let run_id = self
                .io({
                    let message = message.clone();
                    let parent_id = self.id.clone();
                    let parent_call = call.id.clone();
                    let session_id = session_id.clone();
                    let task_id = child_task.id.clone();
                    move |store, _| {
                        store.begin_subagent(
                            &parent_id,
                            &parent_call,
                            &message,
                            &session_id,
                            &task_id,
                        )
                    }
                })
                .await?;
            let result = self
                .run_child_task(child_task, message, history, run_id.clone())
                .await;
            subagent_registry::unregister(&run_id);
            let completion = ToolCompletion::Result(ToolResult {
                call_id: call.id.clone(),
                content: match result {
                    Ok(text) => serde_json::json!({
                        "delivered": true,
                        "agent_id": run_id,
                        "session_id": session_id,
                        "response": text,
                    })
                    .to_string(),
                    Err(error) => serde_json::json!({
                        "delivered": false,
                        "agent_id": run_id,
                        "error": error.to_string(),
                    })
                    .to_string(),
                },
                is_error: false,
                images: Vec::new(),
            });
            return self
                .io(move |store, id| store.complete_tool(id, &receipt, completion))
                .await
                .map_err(|_| AgentError::Host);
        }
        if call.name == "list_agents" {
            if !self.allow_subagent {
                return Err(AgentError::Host);
            }
            let session_id = self
                .io(|store, id| store.get(id).map(|task| task.conversation_id))
                .await?;
            let mut rows = self
                .io(move |store, _| store.subagents_for_session(&session_id))
                .await?;
            for row in &mut rows {
                if let Some(agent_id) = row["id"].as_str() {
                    row["live_status"] =
                        serde_json::json!(subagent_inbox::status(agent_id).unwrap_or("ended"));
                }
            }
            let completion = ToolCompletion::Result(ToolResult {
                call_id: call.id.clone(),
                content: serde_json::json!({"agents": rows}).to_string(),
                is_error: false,
                images: Vec::new(),
            });
            return self
                .io(move |store, id| store.complete_tool(id, &receipt, completion))
                .await
                .map_err(|_| AgentError::Host);
        }
        if call.name == "interrupt_agent" {
            if !self.allow_subagent {
                return Err(AgentError::Host);
            }
            let agent_id = call
                .arguments
                .get("agent_id")
                .and_then(|value| value.as_str())
                .ok_or(AgentError::Host)?;
            let live = subagent_inbox::status(agent_id);
            let running = live == Some("running");
            let cancelled = if running {
                subagent_registry::cancel(agent_id)
            } else {
                false
            };
            let completion = ToolCompletion::Result(ToolResult {
                call_id: call.id.clone(),
                content: serde_json::json!({
                    "accepted": true,
                    "agent_id": agent_id,
                    "live_status": live.unwrap_or("ended"),
                    "cancelled_current_turn": cancelled,
                })
                .to_string(),
                is_error: false,
                images: Vec::new(),
            });
            return self
                .io(move |store, id| store.complete_tool(id, &receipt, completion))
                .await
                .map_err(|_| AgentError::Host);
        }
        if call.name == "spawn_subagents" {
            if !self.allow_subagent {
                return Err(AgentError::Host);
            }
            let prompts: Vec<String> = call
                .arguments
                .get("prompts")
                .and_then(|value| value.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                        .take(4)
                        .collect()
                })
                .unwrap_or_default();
            if prompts.is_empty() || prompts.len() > 4 {
                return Err(AgentError::Host);
            }
            let parent_session = self
                .io(|store, id| store.get(id).map(|task| task.conversation_id))
                .await?;
            let mut started = Vec::new();
            for prompt in prompts {
                let child_task = self
                    .io({
                        let prompt = prompt.clone();
                        let target = self.target;
                        move |store, _| store.create_child_task(&prompt, target)
                    })
                    .await?;
                let _ = inherit_workspace(self, &child_task.conversation_id).await;
                let run_id = self
                    .io({
                        let prompt = prompt.clone();
                        let parent_id = self.id.clone();
                        let parent_call = call.id.clone();
                        let session_id = child_task.conversation_id.clone();
                        let task_id = child_task.id.clone();
                        move |store, _| {
                            store.begin_subagent(
                                &parent_id,
                                &parent_call,
                                &prompt,
                                &session_id,
                                &task_id,
                            )
                        }
                    })
                    .await?;
                let _ = self
                    .io({
                        let parent_id = self.id.clone();
                        let run_id = run_id.clone();
                        let session_id = child_task.conversation_id.clone();
                        let prompt = prompt.clone();
                        move |store, _| {
                            store.record_subagent_catalog(&parent_id, &run_id, &session_id, &prompt)
                        }
                    })
                    .await;
                let mut child_host = TaskHost {
                    store: self.store.clone(),
                    id: child_task.id.clone(),
                    gateway: self.gateway.clone(),
                    target: self.target,
                    model: self.model.clone(),
                    limits: self.limits.clone(),
                    system: self.system.clone(),
                    cancel: self.cancel.clone(),
                    allow_subagent: false,
                    parent_call_id: None,
                    budget: self.budget.clone(),
                    context_window_tokens: self.context_window_tokens,
                    steer: None,
                };
                let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
                let inbox = subagent_inbox::ChildInbox::new(receiver);
                child_host.steer = Some(inbox.clone());
                subagent_inbox::register(&run_id, sender);
                let child_session = child_task.conversation_id.clone();
                tokio::spawn(run_child_loop(
                    child_host,
                    child_task,
                    prompt,
                    run_id.clone(),
                    parent_session.clone(),
                    inbox,
                ));
                started.push(serde_json::json!({
                    "agent_id": run_id,
                    "session_id": child_session,
                }));
            }
            let completion = ToolCompletion::Result(ToolResult {
                call_id: call.id.clone(),
                content: serde_json::json!({"started": started}).to_string(),
                is_error: false,
                images: Vec::new(),
            });
            return self
                .io(move |store, id| store.complete_tool(id, &receipt, completion))
                .await
                .map_err(|_| AgentError::Host);
        }
        if call.name == "spawn_subagent" {
            if !self.allow_subagent {
                return Err(AgentError::Host);
            }
            let prompt = call
                .arguments
                .get("prompt")
                .and_then(|value| value.as_str())
                .ok_or(AgentError::Host)?
                .to_owned();
            let parent_session = self
                .io(|store, id| store.get(id).map(|task| task.conversation_id))
                .await?;
            let child_task = self
                .io({
                    let prompt = prompt.clone();
                    let target = self.target;
                    move |store, _| store.create_child_task(&prompt, target)
                })
                .await?;
            let _ = inherit_workspace(self, &child_task.conversation_id).await;
            let run_id = self
                .io({
                    let prompt = prompt.clone();
                    let parent_id = self.id.clone();
                    let parent_call = call.id.clone();
                    let session_id = child_task.conversation_id.clone();
                    let task_id = child_task.id.clone();
                    move |store, _| {
                        store.begin_subagent(
                            &parent_id,
                            &parent_call,
                            &prompt,
                            &session_id,
                            &task_id,
                        )
                    }
                })
                .await?;
            let _ = self
                .io({
                    let parent_id = self.id.clone();
                    let run_id = run_id.clone();
                    let session_id = child_task.conversation_id.clone();
                    let prompt = prompt.clone();
                    move |store, _| {
                        store.record_subagent_catalog(&parent_id, &run_id, &session_id, &prompt)
                    }
                })
                .await;
            let mut child_host = TaskHost {
                store: self.store.clone(),
                id: child_task.id.clone(),
                gateway: self.gateway.clone(),
                target: self.target,
                model: self.model.clone(),
                limits: self.limits.clone(),
                system: self.system.clone(),
                cancel: self.cancel.clone(),
                allow_subagent: false,
                parent_call_id: None,
                budget: self.budget.clone(),
                context_window_tokens: self.context_window_tokens,
                steer: None,
            };
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            let inbox = subagent_inbox::ChildInbox::new(receiver);
            child_host.steer = Some(inbox.clone());
            subagent_inbox::register(&run_id, sender);
            let agent_id = run_id.clone();
            let child_session = child_task.conversation_id.clone();
            tokio::spawn(run_child_loop(
                child_host,
                child_task,
                prompt,
                agent_id.clone(),
                parent_session,
                inbox,
            ));
            let completion = ToolCompletion::Result(ToolResult {
                call_id: call.id.clone(),
                content: serde_json::json!({
                    "started": true,
                    "agent_id": agent_id,
                    "session_id": child_session,
                })
                .to_string(),
                is_error: false,
                images: Vec::new(),
            });
            return self
                .io(move |store, id| store.complete_tool(id, &receipt, completion))
                .await
                .map_err(|_| AgentError::Host);
        }
        if call.name == "invoke_tool" {
            let child_name = call
                .arguments
                .get("name")
                .and_then(|value| value.as_str())
                .ok_or(AgentError::Host)?;
            if child_name == "invoke_tool" {
                return Err(AgentError::Host);
            }
            let child_arguments = call
                .arguments
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let child_call = ToolCall {
                id: format!("{}:sub:{}", call.id, uuid::Uuid::new_v4()),
                name: child_name.to_owned(),
                arguments: child_arguments,
            };
            let child_schema = self
                .gateway
                .definitions()
                .into_iter()
                .find(|tool| tool.name == child_call.name)
                .map(|tool| tool.schema.to_string());
            let child_receipt = child_call.clone();
            let parent_id = call.id.clone();
            let child_existing = self
                .io({
                    let child_schema = child_schema.clone();
                    let parent_id = parent_id.clone();
                    move |store, id| {
                        store.begin_tool(
                            id,
                            &child_receipt,
                            child_schema.as_deref(),
                            Some(&parent_id),
                            1,
                        )
                    }
                })
                .await?;
            let child_result = if let Some(result) = child_existing {
                result
            } else {
                let child_context = ToolContext::office(&self.id, &child_call.id);
                let child_completion = match self
                    .gateway
                    .execute(
                        &child_context,
                        &child_call.name,
                        child_call.arguments.clone(),
                    )
                    .await
                {
                    Ok(ToolEffect::Result(value)) => ToolCompletion::Result(ToolResult {
                        call_id: child_call.id.clone(),
                        content: value.to_string(),
                        is_error: false,
                        images: Vec::new(),
                    }),
                    Ok(ToolEffect::ResultWithImages { value, images }) => {
                        ToolCompletion::Result(ToolResult {
                            call_id: child_call.id.clone(),
                            content: value.to_string(),
                            is_error: false,
                            images,
                        })
                    }
                    Ok(_) => ToolCompletion::Result(ToolResult {
                        call_id: child_call.id.clone(),
                        content: "子工具返回了非结果产物".into(),
                        is_error: true,
                        images: Vec::new(),
                    }),
                    Err(error) => ToolCompletion::Result(ToolResult {
                        call_id: child_call.id.clone(),
                        content: error,
                        is_error: true,
                        images: Vec::new(),
                    }),
                };
                let receipt = child_call.clone();
                self.io(move |store, id| store.complete_tool(id, &receipt, child_completion))
                    .await?
            };
            let parent_completion = ToolCompletion::Result(ToolResult {
                call_id: call.id.clone(),
                content: child_result.content.clone(),
                is_error: child_result.is_error,
                images: child_result.images.clone(),
            });
            let receipt = call.clone();
            return self
                .io(move |store, id| store.complete_tool(id, &receipt, parent_completion))
                .await
                .map_err(|_| AgentError::Host);
        }
        let completion = match self
            .gateway
            .execute(&context, &call.name, call.arguments.clone())
            .await
        {
            Ok(ToolEffect::Result(value)) => ToolCompletion::Result(ToolResult {
                call_id: call.id.clone(),
                content: value.to_string(),
                is_error: false,
                images: Vec::new(),
            }),
            Ok(ToolEffect::ResultWithImages { value, images }) => {
                ToolCompletion::Result(ToolResult {
                    call_id: call.id.clone(),
                    content: value.to_string(),
                    is_error: false,
                    images,
                })
            }
            Ok(ToolEffect::Plan(steps)) => ToolCompletion::Plan { steps },
            Ok(ToolEffect::Document { title, content }) => {
                ToolCompletion::Document { title, content }
            }
            Ok(ToolEffect::Artifact {
                title,
                filename,
                media_type,
                base64,
            }) => ToolCompletion::Artifact {
                title,
                filename,
                media_type,
                base64,
            },
            Err(message) => ToolCompletion::Result(ToolResult {
                call_id: call.id.clone(),
                content: message,
                is_error: true,
                images: Vec::new(),
            }),
        };
        let receipt = call.clone();
        self.io(move |store, id| store.complete_tool(id, &receipt, completion))
            .await
    }
}
