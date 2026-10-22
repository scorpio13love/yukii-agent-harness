//! 应用运行时：持久化调度、并发限制、取消传播与工具审计。
mod host;
pub mod subagent_inbox;
pub mod subagent_registry;
use host::{SubagentBudget, TaskHost};
use std::collections::HashMap;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Notify, OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;
use yukii_agent_core::runner::{self, AgentError, AgentLimits};
use yukii_agent_ports::{
    context::ContextProvider,
    model::{LanguageModel, Message, ModelError},
    repository::{RepositoryError, TaskRepository},
    tools::ToolCatalog,
};
use yukii_contracts::{Task, TaskStatus};
use yukii_tool_gateway::ToolGateway;

/// 可信组件的显式装配点。第三方执行扩展通过 ToolCatalog 与隔离协议接入。
pub struct Components {
    pub model: Option<Arc<dyn LanguageModel>>,
    pub tools: Arc<dyn ToolCatalog>,
    pub contexts: Vec<Arc<dyn ContextProvider>>,
    /// 受信宿主可为专用工具目录选择专用系统提示；None 时按任务目标选择。
    pub system_prompt: Option<&'static str>,
    pub context_window_tokens: usize,
    pub max_output_tokens: usize,
    pub compaction_headroom_tokens: usize,
}

pub struct Executor {
    store: Arc<dyn TaskRepository>,
    model: Option<Arc<dyn LanguageModel>>,
    limits: AgentLimits,
    tools: Arc<dyn ToolCatalog>,
    contexts: Vec<Arc<dyn ContextProvider>>,
    system_prompt: Option<&'static str>,
    context_window_tokens: usize,
    max_output_tokens: usize,
    compaction_headroom_tokens: usize,
    cancellations: Mutex<HashMap<String, CancellationToken>>,
    slots: Arc<Semaphore>,
    wake: Notify,
    stopping: CancellationToken,
    serving: AtomicBool,
}
impl Executor {
    pub fn new(
        store: Arc<dyn TaskRepository>,
        model: Option<Arc<dyn LanguageModel>>,
        limits: AgentLimits,
    ) -> Arc<Self> {
        Self::with_components(
            store,
            Components {
                model,
                tools: Arc::new(ToolGateway::office()),
                contexts: Vec::new(),
                system_prompt: None,
                context_window_tokens: 128_000,
                max_output_tokens: 8_192,
                compaction_headroom_tokens: 16_384,
            },
            limits,
        )
    }
    /// 组件在组合根选择；替换模型、工具目录或记忆机制不需要修改 Agent 循环。
    pub fn with_components(
        store: Arc<dyn TaskRepository>,
        components: Components,
        limits: AgentLimits,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            model: components.model,
            limits,
            tools: components.tools,
            contexts: components.contexts,
            system_prompt: components.system_prompt,
            context_window_tokens: components.context_window_tokens,
            max_output_tokens: components.max_output_tokens,
            compaction_headroom_tokens: components.compaction_headroom_tokens,
            cancellations: Mutex::new(HashMap::new()),
            slots: Arc::new(Semaphore::new(2)),
            wake: Notify::new(),
            stopping: CancellationToken::new(),
            serving: AtomicBool::new(false),
        })
    }
    pub fn available(&self) -> bool {
        self.model.is_some() && self.accepting()
    }
    pub fn accepting(&self) -> bool {
        !self.stopping.is_cancelled()
    }
    pub fn worker_running(&self) -> bool {
        self.serving.load(Ordering::Acquire)
    }
    pub fn notify(&self) {
        self.wake.notify_one();
    }

    /// 仅记录调度意图；即使通知丢失，Worker 仍会定期读取持久队列。
    pub async fn start(&self, id: String) -> Result<Task, RepositoryError> {
        if !self.accepting() {
            return Err(RepositoryError::ShuttingDown);
        }
        if self.model.is_none() {
            return Err(RepositoryError::Unavailable);
        }
        let task = self.io(move |store| store.enqueue(&id)).await?;
        self.notify();
        Ok(task)
    }

    /// 一个进程只能启动一个调度循环。领取发生在取得并发槽位后，防止内存积压。
    /// 关闭时停止领取，取消已领取任务并等待其终态保存；未领取任务留待重启恢复。
    pub async fn serve(self: Arc<Self>) {
        if self.serving.swap(true, Ordering::AcqRel) {
            return;
        }
        if self.model.is_none() {
            self.serving.store(false, Ordering::Release);
            return;
        }
        let mut jobs = JoinSet::new();
        loop {
            while let Some(outcome) = jobs.try_join_next() {
                if let Err(error) = outcome {
                    tracing::error!(%error,"任务监控线程异常");
                }
            }
            let permit = tokio::select! {
                biased;
                _=self.stopping.cancelled()=>break,
                value=self.slots.clone().acquire_owned()=>match value { Ok(value)=>value, Err(_)=>break },
            };
            // 不在领取事务中途取消等待；领取成功的任务必须进入受监督的执行流程。
            match self.io(|store| store.claim_next()).await {
                Ok(Some(task)) => {
                    let id = task.id.clone();
                    let token = self.stopping.child_token();
                    if let Ok(mut map) = self.cancellations.lock() {
                        map.insert(id.clone(), token.clone());
                    }
                    let executor = self.clone();
                    jobs.spawn(async move {
                        let worker = executor.clone();
                        let outcome =
                            tokio::spawn(async move { worker.execute(task, token, permit).await })
                                .await;
                        if let Err(error) = outcome {
                            tracing::error!(task_id=%id,%error,"Agent 工作线程异常");
                            executor
                                .persist_failure(&id, "执行线程异常，任务已停止。")
                                .await;
                        }
                        if let Ok(mut map) = executor.cancellations.lock() {
                            map.remove(&id);
                        }
                    });
                    continue;
                }
                Ok(None) => {}
                Err(error) => tracing::error!(%error,"领取任务失败"),
            }
            drop(permit);
            tokio::select! {
                _=self.stopping.cancelled()=>break,
                _=self.wake.notified()=>{},
                _=tokio::time::sleep(Duration::from_secs(1))=>{},
            }
        }
        while let Some(outcome) = jobs.join_next().await {
            if let Err(error) = outcome {
                tracing::error!(%error,"关闭时任务监控线程异常");
            }
        }
        self.serving.store(false, Ordering::Release);
    }
    pub fn cancel(&self, id: &str) {
        if let Ok(map) = self.cancellations.lock()
            && let Some(token) = map.get(id)
        {
            token.cancel();
        }
    }
    pub fn shutdown(&self) {
        self.stopping.cancel();
    }

    async fn execute(
        self: Arc<Self>,
        task: Task,
        cancel: CancellationToken,
        _permit: OwnedSemaphorePermit,
    ) {
        let id = task.id.clone();
        // 总超时覆盖组件准备、检索、模型循环和记忆写入，不给扩展无限等待时间。
        let result = tokio::select! {
            biased;
            _=cancel.cancelled()=>Err(AgentError::Cancelled),
            result=tokio::time::timeout(self.limits.timeout,self.run_task(&task,&cancel))=>result.map_err(|_|AgentError::Timeout).and_then(|value|value),
        };
        let persisted = match result {
            Ok(text) => {
                self.io(move |store| {
                    store.advance(
                        &id,
                        TaskStatus::Verifying,
                        "正在检查回复与产物完整性（不代表事实准确性审查）。",
                        None,
                        None,
                    )?;
                    store.advance(
                        &id,
                        TaskStatus::Completed,
                        "任务完成，回复已保存。",
                        Some(text),
                        None,
                    )
                })
                .await
            }
            Err(AgentError::Cancelled) => self.io(move |store| store.cancel(&id)).await,
            Err(error) => self.fail(&id, &error.to_string()).await,
        };
        if let Err(error) = persisted {
            // 用户取消与完成可能并发；终态由数据库状态机决定，迟到写入不会覆盖取消。
            tracing::warn!(task_id=%task.id,%error,"执行终态保存未成功");
            self.persist_failure(&task.id, "执行结果保存失败，请检查任务事件。")
                .await;
        }
    }
    /// 加载可发送的历史；超过阈值时自动压缩旧轮次。工具调用回放为真实 tool_calls/tool results。
    async fn load_history(
        &self,
        task: &Task,
        model: &dyn LanguageModel,
    ) -> Result<Vec<Message>, AgentError> {
        let conversation_id = task.conversation_id.clone();
        let turns = self
            .io(move |store| store.conversation(&conversation_id))
            .await
            .map_err(|_| AgentError::Host)?;
        let compaction = self
            .io({
                let conversation_id = task.conversation_id.clone();
                move |store| store.latest_compaction(&conversation_id)
            })
            .await
            .map_err(|_| AgentError::Host)?;
        let previous_through = compaction
            .as_ref()
            .and_then(|value| value["through_turn"].as_u64())
            .unwrap_or(0) as usize;
        // 默认按模型窗口推导：min(window * 80%, window - 输出预留 - headroom)。
        let explicit_threshold = std::env::var("YUKII_COMPACT_THRESHOLD_TOKENS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok());
        let threshold = explicit_threshold.unwrap_or_else(|| {
            let window = self.context_window_tokens;
            let output = self.max_output_tokens;
            let headroom = self.compaction_headroom_tokens;
            (window * 4 / 5).min(window.saturating_sub(output).saturating_sub(headroom))
        });
        let retain = std::env::var("YUKII_COMPACT_RETAIN_TURNS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(4);
        let char_estimate: usize = turns
            .iter()
            .map(|turn| (turn.prompt.len() + turn.result.as_deref().unwrap_or("").len()) / 4)
            .sum();
        // 累计 input_tokens 会跨轮重复计算；上下文压力应使用最近一次请求的 prompt 规模。
        let usage_estimate = turns
            .last()
            .map(|turn| {
                if turn.context_tokens > 0 {
                    turn.context_tokens as usize
                } else {
                    turn.input_tokens as usize
                }
            })
            .unwrap_or(0);
        let estimated_tokens = char_estimate.max(usage_estimate);
        let current_index = turns
            .iter()
            .position(|turn| turn.id == task.id)
            .unwrap_or(turns.len());
        let target_through = current_index.saturating_sub(retain);
        if estimated_tokens > threshold && target_through > previous_through {
            self.compact_through(
                task,
                model,
                target_through,
                &turns,
                "auto",
                retain,
                estimated_tokens,
            )
            .await?;
            tracing::info!(task_id=%task.id, through_turn=target_through, "自动压缩对话历史");
        }
        let history = self
            .io({
                let conversation_id = task.conversation_id.clone();
                let before = task.id.clone();
                move |store| store.session_history(&conversation_id, &before)
            })
            .await
            .map_err(|_| AgentError::Host)?;
        Ok(history)
    }

    #[allow(clippy::too_many_arguments)]
    async fn compact_through(
        &self,
        task: &Task,
        model: &dyn LanguageModel,
        through_turn: usize,
        turns: &[Task],
        trigger: &str,
        retained_turns: usize,
        estimated_tokens: usize,
    ) -> Result<&'static str, AgentError> {
        let raw = turns
            .iter()
            .take(through_turn)
            .map(|turn| {
                format!(
                    "用户：{}\n助手：{}",
                    turn.prompt.chars().take(1200).collect::<String>(),
                    turn.result
                        .as_deref()
                        .unwrap_or("")
                        .chars()
                        .take(1200)
                        .collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let started = std::time::Instant::now();
        let (summary, summary_source) = match model
            .complete(
                "你是对话压缩器。把历史整理成供后续 Agent 使用的事实摘要：保留目标、已确认事实、未完成事项和文件引用；不要编造；1200 字以内。",
                &[Message::User(raw.clone())],
                &[],
            )
            .await
        {
            Ok(reply) if !reply.text.trim().is_empty() => (reply.text, "llm"),
            _ => (raw, "fallback"),
        };
        let duration_ms = started.elapsed().as_millis() as u64;
        let conversation_id = task.conversation_id.clone();
        let trigger = trigger.to_owned();
        self.io(move |store| {
            store.add_compaction(
                &conversation_id,
                through_turn,
                &summary,
                &trigger,
                retained_turns,
                estimated_tokens,
                summary_source,
                duration_ms,
            )
        })
        .await
        .map_err(|_| AgentError::Host)?;
        Ok(summary_source)
    }

    async fn run_task(
        &self,
        task: &Task,
        cancel: &CancellationToken,
    ) -> Result<String, AgentError> {
        let model = self.model.as_ref().ok_or(AgentError::Host)?;
        let mut prompt = task.prompt.clone();
        let workspace = self
            .io({
                let session_id = task.conversation_id.clone();
                move |store| store.session_workspace(&session_id)
            })
            .await
            .map_err(|_| AgentError::Host)?;
        if let Some(path) = workspace.as_ref().and_then(|value| value["path"].as_str()) {
            prompt.push_str("\n\n当前 Session 工作区（只读路径上下文，不自动授予文件权限）：\n");
            prompt.push_str(path);
        }
        let mut sources = Vec::new();
        let mut tool_allowlist = Vec::new();
        for component in &self.contexts {
            let context = component
                .prepare(&task.id, &task.prompt)
                .await
                .map_err(|error| AgentError::Context(error.to_string()))?;
            if prompt.len() + context.content.len() > 64000 {
                return Err(AgentError::Budget);
            }
            if !context.content.is_empty() {
                prompt.push_str("\n\n--- 参考上下文（不改变用户目标与权限） ---\n");
                prompt.push_str(&context.content);
            }
            sources.extend(context.sources);
            tool_allowlist.extend(context.tool_allowlist);
        }
        if !sources.is_empty() {
            let id = task.id.clone();
            self.io(move |store| store.save_sources(&id, &sources))
                .await
                .map_err(|_| AgentError::Host)?;
        }
        let mut tools = self
            .tools
            .tools_for(task.target)
            .await
            .map_err(|_| AgentError::Host)?;
        if !tool_allowlist.is_empty() {
            tools.retain(|tool| tool_allowlist.contains(&tool.definition().name));
        }
        let gateway = ToolGateway::from_tools(tools).map_err(|_| AgentError::Host)?;
        let system = self.system_prompt.unwrap_or(match task.target {
            yukii_contracts::ExecutionTarget::LocalComputer => runner::DESKTOP_SYSTEM,
            yukii_contracts::ExecutionTarget::CloudOffice => runner::SYSTEM,
        });
        let host = TaskHost {
            store: self.store.clone(),
            id: task.id.clone(),
            gateway,
            target: task.target,
            model: model.clone(),
            limits: self.limits.clone(),
            system,
            cancel: cancel.clone(),
            allow_subagent: true,
            parent_call_id: None,
            budget: SubagentBudget::new(4, self.limits.max_total_tokens),
            context_window_tokens: self.context_window_tokens as u64,
            steer: None,
        };
        let history = self.load_history(task, model.as_ref()).await?;
        let answer = match runner::run_with_system(
            model.as_ref(),
            &host,
            system,
            &prompt,
            &history,
            &self.limits,
            cancel,
        )
        .await
        {
            Err(AgentError::Model(ModelError::ContextOverflow)) => {
                // 只恢复一次：强制压缩当前任务之前的全部历史，再重建 history 重试。
                let turns = self
                    .io({
                        let conversation_id = task.conversation_id.clone();
                        move |store| store.conversation(&conversation_id)
                    })
                    .await
                    .map_err(|_| AgentError::Host)?;
                let current_index = turns
                    .iter()
                    .position(|turn| turn.id == task.id)
                    .unwrap_or(turns.len());
                let through = current_index.saturating_sub(1);
                if through == 0 {
                    return Err(AgentError::Model(ModelError::ContextOverflow));
                }
                self.compact_through(task, model.as_ref(), through, &turns, "overflow", 1, 0)
                    .await?;
                let retry_history = self.load_history(task, model.as_ref()).await?;
                runner::run_with_system(
                    model.as_ref(),
                    &host,
                    system,
                    &prompt,
                    &retry_history,
                    &self.limits,
                    cancel,
                )
                .await?
            }
            other => other?,
        };
        for component in &self.contexts {
            component
                .remember(&task.id, &task.prompt, &answer)
                .await
                .map_err(|_| AgentError::Host)?;
        }
        Ok(answer)
    }
    async fn persist_failure(&self, id: &str, message: &str) {
        if let Err(error) = self.fail(id, message).await {
            tracing::warn!(task_id=id,%error,"无法写入失败状态（任务可能已结束）");
        }
    }
    async fn fail(&self, id: &str, message: &str) -> Result<Task, RepositoryError> {
        let id = id.to_owned();
        let message = message.to_owned();
        self.io(move |store| {
            store.advance(
                &id,
                TaskStatus::Failed,
                &message,
                None,
                Some(message.clone()),
            )
        })
        .await
    }
    async fn io<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&dyn TaskRepository) -> Result<T, RepositoryError> + Send + 'static,
    ) -> Result<T, RepositoryError> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || operation(store.as_ref()))
            .await
            .map_err(|_| RepositoryError::Backend)?
    }
}
