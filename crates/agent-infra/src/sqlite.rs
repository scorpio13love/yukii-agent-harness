//! SQLite 单实例存储。任务状态与事件使用同一事务提交，避免双写不一致。
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use uuid::Uuid;
use yukii_agent_domain::{TaskError, transition, validate_desktop_task, validate_task};
use yukii_agent_ports::repository::CreatedTask;
use yukii_contracts::{Artifact, CreateTask, Task, TaskEvent, TaskStatus};

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("当前会话还有未结束的执行，请等待完成或取消后再发送")]
    ConversationBusy,
    #[error("数据库版本高于当前程序，请使用匹配版本启动")]
    SchemaTooNew,
    #[error("Session 日志格式不受支持：{0}")]
    SessionFormat(String),
    #[error("同一幂等键已用于不同请求")]
    IdempotencyConflict,
    #[error("任务队列已满")]
    QueueFull,
    #[error("模型尚未配置，请填写 .env 中的 API 格式、密钥和模型名，再重启服务")]
    Unavailable,
    #[error(transparent)]
    Validation(#[from] TaskError),
    #[error("任务不存在")]
    NotFound,
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("任务存储不可用")]
    Lock,
    #[error("桌面控制未激活、已暂停或已结束")]
    ControlUnavailable,
    #[error("桌面观察已过期、已使用或不属于当前控制会话")]
    ObservationUnavailable,
}

pub struct TaskStore {
    pub(crate) connection: Mutex<Connection>,
    artifacts: std::path::PathBuf,
    pub(crate) session_log: super::session_log::SessionLog,
}

impl TaskStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        let session_root = super::session_log::root_for_db(&path);
        Self::open_with_session_root(path, session_root)
    }

    pub(crate) fn open_with_session_root(
        path: impl AsRef<Path>,
        session_root: impl AsRef<Path>,
    ) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        let mut connection = Connection::open(&path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;",
        )?;
        super::migrations::apply(&mut connection)?;
        let artifacts = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("artifacts");
        std::fs::create_dir_all(&artifacts)?;
        let session_log = super::session_log::SessionLog::new(session_root.as_ref().to_path_buf());
        let store = Self {
            connection: Mutex::new(connection),
            artifacts,
            session_log,
        };
        store.backfill_session_log()?;
        store.drain_session_outbox()?;
        store.session_log.repair_all()?;
        let retain_days = std::env::var("YUKII_OUTBOX_RETAIN_DAYS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(7_i64);
        store.prune_session_outbox(retain_days)?;
        Ok(store)
    }

    /// 在业务事务内登记事件；JSONL 投递在提交后进行，崩溃后由 outbox 补偿。
    pub(crate) fn enqueue_session_event(
        connection: &Connection,
        session_id: &str,
        event_id: &str,
        event_type: &str,
        data: serde_json::Value,
    ) -> Result<(), StoreError> {
        connection.execute(
            "INSERT OR IGNORE INTO session_outbox(event_id,session_id,event_type,payload,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![event_id, session_id, event_type, serde_json::to_string(&data)?, now()],
        )?;
        Ok(())
    }

    /// 投递未完成事件；session_log 以 event_id 幂等，重复投递不会产生重复行。
    pub(crate) fn drain_session_outbox(&self) -> Result<(), StoreError> {
        loop {
            let pending = {
                let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
                let mut statement = connection.prepare(
                    "SELECT id,event_id,session_id,event_type,payload FROM session_outbox WHERE delivered=0 ORDER BY id LIMIT 200",
                )?;
                statement
                    .query_map([], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    })?
                    .collect::<Result<Vec<_>, _>>()?
            };
            if pending.is_empty() {
                return Ok(());
            }
            for (id, event_id, session_id, event_type, payload) in pending {
                let data: serde_json::Value = serde_json::from_str(&payload)?;
                self.session_log
                    .append(&session_id, &event_id, &event_type, data)?;
                let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
                connection.execute("UPDATE session_outbox SET delivered=1 WHERE id=?1", [id])?;
            }
        }
    }

    /// 清理已投递且超过保留期的 outbox 行；未投递行永不删除。
    pub(crate) fn prune_session_outbox(&self, retain_days: i64) -> Result<usize, StoreError> {
        let cutoff = now().saturating_sub(retain_days.max(1).saturating_mul(86_400));
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let removed = connection.execute(
            "DELETE FROM session_outbox WHERE delivered=1 AND created_at < ?1",
            [cutoff],
        )?;
        Ok(removed)
    }

    /// 把已有 SQLite 任务/工具/压缩镜像回 JSONL；事件 id 幂等。
    fn backfill_session_log(&self) -> Result<(), StoreError> {
        let (tasks, calls, compactions) = {
            let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
            let mut task_statement =
                connection.prepare("SELECT payload FROM tasks ORDER BY rowid")?;
            let tasks = task_statement
                .query_map([], |row| row.get::<_, String>(0))?
                .map(|row| Ok(serde_json::from_str::<Task>(&row?)?))
                .collect::<Result<Vec<_>, StoreError>>()?;
            let mut call_statement = connection.prepare(
                "SELECT task_id,call_id,name,arguments,result,execution_state FROM tool_calls ORDER BY rowid",
            )?;
            let calls = call_statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut compaction_statement = connection.prepare(
                "SELECT conversation_id,through_turn,summary,trigger FROM conversation_compactions ORDER BY rowid",
            )?;
            let compactions = compaction_statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            (tasks, calls, compactions)
        };
        for task in &tasks {
            self.session_log.append(
                &task.conversation_id,
                &format!("{}:user", task.id),
                "user/message",
                serde_json::json!({
                    "task_id": task.id,
                    "prompt": task.prompt,
                    "target": task.target,
                    "created_at": task.created_at,
                }),
            )?;
            for (task_id, call_id, name, arguments, result, state) in &calls {
                if task_id != &task.id {
                    continue;
                }
                self.session_log.append(
                    &task.conversation_id,
                    &format!("{task_id}:call:{call_id}"),
                    "tool/call",
                    serde_json::json!({
                        "task_id": task_id,
                        "call_id": call_id,
                        "name": name,
                        "arguments": arguments,
                        "state": state,
                    }),
                )?;
                if let Some(raw) = result {
                    let parsed = serde_json::from_str::<serde_json::Value>(raw).ok();
                    let content = parsed
                        .as_ref()
                        .and_then(|value| value["content"].as_str())
                        .unwrap_or(raw);
                    let is_error = parsed
                        .as_ref()
                        .and_then(|value| value["is_error"].as_bool())
                        .unwrap_or(false);
                    self.session_log.append(
                        &task.conversation_id,
                        &format!("{task_id}:result:{call_id}"),
                        "tool/result",
                        serde_json::json!({
                            "task_id": task_id,
                            "call_id": call_id,
                            "content": content,
                            "is_error": is_error,
                            "images": parsed
                                .as_ref()
                                .and_then(|value| value["images"].as_array())
                                .map(|images| images.len())
                                .unwrap_or(0),
                        }),
                    )?;
                }
            }
            if matches!(
                task.status,
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
            ) {
                self.session_log.append(
                    &task.conversation_id,
                    &format!("{}:assistant", task.id),
                    "assistant/message",
                    serde_json::json!({
                        "task_id": task.id,
                        "content": task.result.clone().unwrap_or_else(|| task.error.clone().unwrap_or_default()),
                        "status": task.status,
                        "usage": {
                            "input_tokens": task.input_tokens,
                            "output_tokens": task.output_tokens,
                            "cache_read_tokens": task.cache_read_tokens,
                            "cache_write_tokens": task.cache_write_tokens,
                        }
                    }),
                )?;
            }
        }
        for (session_id, through_turn, summary, trigger) in compactions {
            self.session_log.append(
                &session_id,
                &format!("compaction:{through_turn}"),
                "compaction",
                serde_json::json!({
                    "summary": summary,
                    "through_turn": through_turn,
                    "trigger": trigger,
                }),
            )?;
        }
        Ok(())
    }

    /// 兼容内部测试。HTTP 创建必须使用持久化幂等键。
    pub fn create(&self, input: CreateTask) -> Result<Task, StoreError> {
        Ok(self
            .create_once(input, &Uuid::new_v4().to_string(), false)?
            .task)
    }

    /// 请求去重、任务、事件和调度意图一次提交，避免提交成功但执行意图丢失。
    pub fn create_once(
        &self,
        input: CreateTask,
        key: &str,
        enqueue: bool,
    ) -> Result<CreatedTask, StoreError> {
        self.create_for_target(input, key, enqueue, false)
    }

    /// 受信桌面宿主的创建入口；本机目标不能通过网页依赖的 `create_once` 创建。
    /// 真正的 GUI 控制仍需 Executor 的会话同意、窗口绑定和逐动作审批。
    pub fn create_local_once(
        &self,
        input: CreateTask,
        key: &str,
        enqueue: bool,
    ) -> Result<CreatedTask, StoreError> {
        self.create_for_target(input, key, enqueue, true)
    }

    fn create_for_target(
        &self,
        input: CreateTask,
        key: &str,
        enqueue: bool,
        local: bool,
    ) -> Result<CreatedTask, StoreError> {
        let prompt = if local {
            validate_desktop_task(&input.prompt, input.target)?
        } else {
            validate_task(&input.prompt, input.target)?
        };
        let mut request = serde_json::json!({"prompt":prompt,"target":input.target});
        if let Some(id) = &input.conversation_id {
            request["session_id"] = id.clone().into();
        }
        let payload = request.to_string();
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing: Option<(String, String)> = transaction
            .query_row(
                "SELECT request_payload,task_id FROM idempotency_keys WHERE request_key=?1",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((previous, id)) = existing {
            if previous != payload {
                return Err(StoreError::IdempotencyConflict);
            }
            return Ok(CreatedTask {
                task: read_task(&transaction, &id)?,
                replayed: true,
            });
        }
        // IMMEDIATE 事务同时串行化续聊检查与插入，禁止两轮并发打乱上下文。
        let (conversation_id, conversation_title) = if let Some(id) = &input.conversation_id {
            let previous = read_conversation(&transaction, id)?;
            if previous.iter().any(|task| {
                !matches!(
                    task.status,
                    TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
                )
            }) {
                return Err(StoreError::ConversationBusy);
            }
            (
                id.clone(),
                previous
                    .first()
                    .map(|task| task.conversation_title.clone())
                    .unwrap_or_else(|| prompt.clone()),
            )
        } else {
            (Uuid::new_v4().to_string(), prompt.clone())
        };
        let mut task = Task {
            conversation_id,
            conversation_title,
            id: Uuid::new_v4().to_string(),
            prompt,
            target: input.target,
            status: TaskStatus::Queued,
            created_at: now(),
            result: None,
            error: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            context_tokens: 0,
            context_window_tokens: 0,
            sources: Vec::new(),
        };
        transaction.execute(
            "INSERT INTO tasks(id,payload,created_at) VALUES(?1,?2,?3)",
            params![task.id, serde_json::to_string(&task)?, task.created_at],
        )?;
        transaction.execute(
            "INSERT INTO idempotency_keys(request_key,request_payload,task_id) VALUES(?1,?2,?3)",
            params![key, payload, task.id],
        )?;
        append_event(&transaction, &task, "任务已保存，等待执行。")?;
        if enqueue {
            enqueue_task(&transaction, &mut task)?;
        }
        Self::enqueue_session_event(
            &transaction,
            &task.conversation_id,
            &format!("{}:user", task.id),
            "user/message",
            serde_json::json!({
                "task_id": task.id,
                "prompt": task.prompt,
                "target": task.target,
                "created_at": task.created_at,
            }),
        )?;
        transaction.commit()?;
        drop(connection);
        self.drain_session_outbox()?;
        Ok(CreatedTask {
            task,
            replayed: false,
        })
    }

    pub fn enqueue(&self, id: &str) -> Result<Task, StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut task = read_task(&transaction, id)?;
        if task.status == TaskStatus::Planning {
            return Ok(task);
        }
        enqueue_task(&transaction, &mut task)?;
        transaction.commit()?;
        Ok(task)
    }

    /// 领取与 Running 状态变更原子提交；同一队列条目只有一个领取者。
    pub fn claim_next(&self) -> Result<Option<Task>, StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let id:Option<String>=transaction.query_row("SELECT task_id FROM execution_queue WHERE state='pending' ORDER BY requested_at,rowid LIMIT 1",[],|row|row.get(0)).optional()?;
        let Some(id) = id else { return Ok(None) };
        let mut task = read_task(&transaction, &id)?;
        transition(task.status, TaskStatus::Running)?;
        task.status = TaskStatus::Running;
        save_task(&transaction, &task)?;
        transaction.execute(
            "UPDATE execution_queue SET state='claimed' WHERE task_id=?1",
            [&id],
        )?;
        append_event(&transaction, &task, "Worker 已领取任务并开始执行。")?;
        transaction.commit()?;
        Ok(Some(task))
    }

    pub fn list(&self) -> Result<Vec<Task>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let mut statement = connection
            .prepare("SELECT payload FROM tasks ORDER BY created_at DESC, rowid DESC LIMIT 100")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn conversations(&self) -> Result<Vec<Task>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let mut statement = connection.prepare("SELECT payload FROM tasks WHERE rowid IN (SELECT MAX(rowid) FROM tasks GROUP BY json_extract(payload,'$.session_id')) ORDER BY rowid DESC LIMIT 500")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn conversation(&self, id: &str) -> Result<Vec<Task>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        read_conversation(&connection, id)
    }

    pub fn get(&self, id: &str) -> Result<Task, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        read_task(&connection, id)
    }

    pub fn events(&self, id: &str) -> Result<Vec<TaskEvent>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        read_task(&connection, id)?;
        let mut statement = connection
            .prepare("SELECT sequence, payload FROM events WHERE task_id = ?1 ORDER BY sequence")?;
        let rows = statement.query_map([id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (sequence, payload) = row?;
            let mut event: TaskEvent = serde_json::from_str(&payload)?;
            event.sequence = sequence;
            Ok(event)
        })
        .collect()
    }

    /// 状态、结果与事件原子写入；取消已提交后，迟到的模型结果无法覆盖终态。
    pub fn advance(
        &self,
        id: &str,
        next: TaskStatus,
        message: &str,
        result: Option<String>,
        error: Option<String>,
    ) -> Result<Task, StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut task = read_task(&transaction, id)?;
        transition(task.status, next)?;
        task.status = next;
        task.result = result;
        task.error = error;
        save_task(&transaction, &task)?;
        if matches!(
            next,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
        ) {
            mark_unfinished_tools(&transaction, id)?;
            transaction.execute(
                "UPDATE execution_queue SET state='done' WHERE task_id=?1",
                [id],
            )?;
        }
        append_event(&transaction, &task, message)?;
        if matches!(
            next,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
        ) {
            let reasoning = reasoning_for_task(&transaction, id)?;
            Self::enqueue_session_event(
                &transaction,
                &task.conversation_id,
                &format!("{}:assistant", task.id),
                "assistant/message",
                serde_json::json!({
                    "task_id": task.id,
                    "content": task.result.clone().unwrap_or_else(|| task.error.clone().unwrap_or_default()),
                    "status": task.status,
                    "reasoning": reasoning,
                }),
            )?;
        }
        transaction.commit()?;
        drop(connection);
        self.drain_session_outbox()?;
        Ok(task)
    }

    /// 在模型请求前保存引用快照；引用、任务与事件同一事务提交。
    pub fn save_sources(
        &self,
        id: &str,
        sources: &[yukii_contracts::knowledge::KnowledgeHit],
    ) -> Result<(), StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut task = read_task(&transaction, id)?;
        ensure_active(&task)?;
        task.sources = sources.to_vec();
        save_task(&transaction, &task)?;
        append_event(
            &transaction,
            &task,
            &format!("已检索并保存 {} 条知识库引用。", sources.len()),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn record(&self, id: &str, message: &str) -> Result<(), StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let task = read_task(&transaction, id)?;
        ensure_active(&task)?;
        append_event(&transaction, &task, message)?;
        transaction.commit()?;
        Ok(())
    }
    pub fn record_delta(&self, id: &str, channel: &str, delta: &str) -> Result<(), StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let task = read_task(&transaction, id)?;
        ensure_active(&task)?;
        let event = TaskEvent {
            sequence: 0,
            task_id: task.id.clone(),
            status: task.status,
            message: String::new(),
            channel: Some(channel.into()),
            delta: Some(delta.into()),
            created_at: now(),
            created_at_ms: now_millis(),
        };
        transaction.execute(
            "INSERT INTO events (task_id, payload) VALUES (?1, ?2)",
            params![task.id, serde_json::to_string(&event)?],
        )?;
        if channel == "retry" {
            Self::enqueue_session_event(
                &transaction,
                &task.conversation_id,
                &format!("{}:retry:{}", task.id, uuid::Uuid::new_v4()),
                "assistant/retry",
                serde_json::json!({"task_id": task.id, "message": delta}),
            )?;
        }
        transaction.commit()?;
        drop(connection);
        if channel == "retry" {
            self.drain_session_outbox()?;
        }
        Ok(())
    }

    pub fn add_usage(
        &self,
        id: &str,
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
        context_window: u64,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut task = read_task(&transaction, id)?;
        ensure_active(&task)?;
        task.input_tokens = task.input_tokens.saturating_add(input);
        task.output_tokens = task.output_tokens.saturating_add(output);
        task.cache_read_tokens = task.cache_read_tokens.saturating_add(cache_read);
        task.cache_write_tokens = task.cache_write_tokens.saturating_add(cache_write);
        task.context_tokens = input.saturating_add(cache_read).saturating_add(cache_write);
        task.context_window_tokens = context_window;
        save_task(&transaction, &task)?;
        Self::enqueue_session_event(
            &transaction,
            &task.conversation_id,
            &format!("{}:attempt:{}", task.id, uuid::Uuid::new_v4()),
            "assistant/attempt",
            serde_json::json!({
                "task_id": task.id,
                "input_tokens": input,
                "output_tokens": output,
                "cache_read_tokens": cache_read,
                "cache_write_tokens": cache_write,
                "context_tokens": input.saturating_add(cache_read).saturating_add(cache_write),
                "context_window_tokens": context_window,
            }),
        )?;
        Self::enqueue_session_event(
            &transaction,
            &task.conversation_id,
            &format!("{}:usage:{}", task.id, uuid::Uuid::new_v4()),
            "usage",
            serde_json::json!({
                "task_id": task.id,
                "input_tokens": task.input_tokens,
                "output_tokens": task.output_tokens,
                "cache_read_tokens": task.cache_read_tokens,
                "cache_write_tokens": task.cache_write_tokens,
                "context_tokens": task.context_tokens,
                "context_window_tokens": task.context_window_tokens,
            }),
        )?;
        transaction.commit()?;
        drop(connection);
        self.drain_session_outbox()?;
        Ok(())
    }

    /// 从已有 Session 的已完成 turn 派生新 Session；事件/工具索引复制后由 JSONL 回填。
    pub fn fork_session(&self, source: &str) -> Result<String, StoreError> {
        let target = uuid::Uuid::new_v4().to_string();
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut statement = transaction.prepare(
            "SELECT payload FROM tasks WHERE json_extract(payload,'$.session_id')=?1 ORDER BY rowid",
        )?;
        let tasks = statement
            .query_map([source], |row| row.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str::<Task>(&row?)?))
            .collect::<Result<Vec<_>, StoreError>>()?;
        drop(statement);
        let mut parent_title = None;
        let mut copied = 0usize;
        for mut task in tasks {
            if !matches!(
                task.status,
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
            ) {
                continue;
            }
            let old_id = task.id.clone();
            parent_title.get_or_insert_with(|| task.conversation_title.clone());
            task.id = uuid::Uuid::new_v4().to_string();
            task.conversation_id = target.clone();
            task.conversation_title =
                format!("{} (fork)", parent_title.as_deref().unwrap_or("Session"));
            transaction.execute(
                "INSERT INTO tasks(id,payload,created_at) VALUES(?1,?2,?3)",
                params![task.id, serde_json::to_string(&task)?, task.created_at],
            )?;
            // 复制事件索引，轨迹面板仍可读取 fork 后的时间线。
            let mut event_statement = transaction
                .prepare("SELECT payload FROM events WHERE task_id=?1 ORDER BY rowid")?;
            let events = event_statement
                .query_map([&old_id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            drop(event_statement);
            for payload in events {
                let mut event: TaskEvent = serde_json::from_str(&payload)?;
                event.task_id = task.id.clone();
                event.sequence = 0;
                transaction.execute(
                    "INSERT INTO events(task_id,payload) VALUES(?1,?2)",
                    params![task.id, serde_json::to_string(&event)?],
                )?;
            }
            let mut call_statement = transaction.prepare(
                "SELECT call_id,name,arguments,result,execution_state,started_at,finished_at,schema,parent_call_id,depth FROM tool_calls WHERE task_id=?1 ORDER BY rowid",
            )?;
            let calls = call_statement
                .query_map([&old_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, i64>(9)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(call_statement);
            for (
                call_id,
                name,
                arguments,
                result,
                state,
                started,
                finished,
                schema,
                parent,
                depth,
            ) in calls
            {
                transaction.execute(
                    "INSERT INTO tool_calls(task_id,call_id,name,arguments,execution_id,execution_state,started_at,finished_at,schema,parent_call_id,depth,result)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                    params![task.id, call_id, name, arguments, uuid::Uuid::new_v4().to_string(), state, started, finished, schema, parent, depth, result],
                )?;
            }
            // 工具截图
            let mut image_statement = transaction.prepare(
                "SELECT call_id,ordinal,media_type,base64,width,height FROM tool_call_images WHERE task_id=?1",
            )?;
            let images = image_statement
                .query_map([&old_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(image_statement);
            for (call_id, ordinal, media_type, base64, width, height) in images {
                transaction.execute(
                    "INSERT INTO tool_call_images(task_id,call_id,ordinal,media_type,base64,width,height) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    params![task.id, call_id, ordinal, media_type, base64, width, height],
                )?;
            }
            // 子 Agent 审计
            let mut subagent_statement = transaction.prepare(
                "SELECT parent_call_id,prompt,status,result,error,started_at,finished_at FROM subagent_runs WHERE parent_task_id=?1",
            )?;
            let subagents = subagent_statement
                .query_map([&old_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(subagent_statement);
            for (parent_call, prompt, status, result, error, started, finished) in subagents {
                transaction.execute(
                    "INSERT INTO subagent_runs(id,parent_task_id,parent_call_id,prompt,status,result,error,started_at,finished_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    params![uuid::Uuid::new_v4().to_string(), task.id, parent_call, prompt, status, result, error, started, finished],
                )?;
            }
            // 产物：复制数据库内容，并复制存在的二进制文件
            let mut artifact_statement = transaction
                .prepare("SELECT id,metadata,content FROM artifacts WHERE task_id=?1")?;
            let artifacts = artifact_statement
                .query_map([&old_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(artifact_statement);
            for (old_artifact, metadata, content) in artifacts {
                let new_artifact = uuid::Uuid::new_v4().to_string();
                let mut value: Artifact = serde_json::from_str(&metadata)?;
                value.id = new_artifact.clone();
                value.task_id = task.id.clone();
                transaction.execute(
                    "INSERT INTO artifacts(id,task_id,metadata,content) VALUES(?1,?2,?3,?4)",
                    params![
                        new_artifact,
                        task.id,
                        serde_json::to_string(&value)?,
                        content
                    ],
                )?;
                let old_file = self.artifacts.join(&old_artifact);
                if old_file.is_file() {
                    let _ = std::fs::copy(old_file, self.artifacts.join(&new_artifact));
                }
            }
            // 附件元数据（文件仍由 Session 上传目录管理）
            let mut attach_statement = transaction.prepare(
                "SELECT file_id,metadata,created_at FROM task_attachments WHERE task_id=?1",
            )?;
            let attachments = attach_statement
                .query_map([&old_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(attach_statement);
            for (file_id, metadata, created_at) in attachments {
                let mut value: serde_json::Value = serde_json::from_str(&metadata)?;
                value["task_id"] = serde_json::json!(task.id);
                value["session_id"] = serde_json::json!(target);
                transaction.execute(
                    "INSERT INTO task_attachments(task_id,file_id,metadata,created_at) VALUES(?1,?2,?3,?4)",
                    params![task.id, file_id, serde_json::to_string(&value)?, created_at],
                )?;
            }
            copied += 1;
        }
        // 工作区绑定
        let workspace: Option<(String, String)> = transaction
            .query_row(
                "SELECT path,name FROM session_workspaces WHERE session_id=?1",
                [source],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((path, name)) = workspace {
            transaction.execute(
                "INSERT OR REPLACE INTO session_workspaces(session_id,path,name,updated_at) VALUES(?1,?2,?3,?4)",
                params![target, path, name, now()],
            )?;
        }
        if copied == 0 {
            return Err(StoreError::NotFound);
        }
        transaction.commit()?;
        drop(connection);
        self.backfill_session_log()?;
        Ok(target)
    }

    pub fn session_history(
        &self,
        session_id: &str,
        before_task: &str,
    ) -> Result<Vec<yukii_agent_ports::model::Message>, StoreError> {
        self.session_log.history(session_id, before_task)
    }

    /// SQLite 索引上的 Session 字面搜索；按 Session 分组，返回命中数与摘要。
    pub fn search_sessions(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<serde_json::Value>, bool), StoreError> {
        let limit = limit.clamp(1, 50);
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        // trigram 至少需要 3 个字符；更短词和 FTS 无命中时回退字面 LIKE。
        if query.chars().count() >= 3 {
            let phrase = format!("\"{}\"", query.replace('"', "\"\""));
            let mut statement = connection.prepare(
                "SELECT session_id,title,snippet(session_search_fts,4,'[',']','…',24),rank
                 FROM session_search_fts
                 WHERE session_search_fts MATCH ?1
                 ORDER BY rank
                 LIMIT 200",
            )?;
            let rows = statement
                .query_map([phrase], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, f64>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            if !rows.is_empty() {
                use std::collections::BTreeMap;
                let mut grouped: BTreeMap<String, (String, i64, String, f64)> = BTreeMap::new();
                for (session_id, title, snippet, rank) in rows {
                    let entry =
                        grouped
                            .entry(session_id)
                            .or_insert((title, 0, snippet.clone(), rank));
                    entry.1 += 1;
                    if rank < entry.3 {
                        entry.2 = snippet;
                        entry.3 = rank;
                    }
                }
                let mut hits: Vec<_> = grouped.into_iter().collect();
                hits.sort_by(|left, right| left.1.3.total_cmp(&right.1.3));
                let has_more = hits.len() > offset.saturating_add(limit);
                let rows = hits
                    .into_iter()
                    .skip(offset)
                    .take(limit)
                    .map(|(session_id, (title, count, snippet, _))| {
                        serde_json::json!({
                            "session_id": session_id,
                            "session_title": title,
                            "latest_at": 0,
                            "hits": count,
                            "snippet": snippet,
                        })
                    })
                    .collect();
                return Ok((rows, has_more));
            }
        }
        let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        let mut statement = connection.prepare(
            "SELECT json_extract(payload,'$.session_id') AS sid,
                    max(json_extract(payload,'$.session_title')) AS title,
                    max(created_at) AS latest,
                    count(*) AS hits,
                    group_concat(substr(json_extract(payload,'$.prompt'),1,160),' | ') AS prompts
             FROM tasks
             WHERE json_extract(payload,'$.prompt') LIKE ?1 ESCAPE '\\'
                OR coalesce(json_extract(payload,'$.result'),'') LIKE ?1 ESCAPE '\\'
             GROUP BY sid
             ORDER BY latest DESC
             LIMIT ?2 OFFSET ?3",
        )?;
        let rows = statement
            .query_map(params![pattern, limit as i64 + 1, offset as i64], |row| {
                Ok(serde_json::json!({
                    "session_id": row.get::<_, String>(0)?,
                    "session_title": row.get::<_, String>(1)?,
                    "latest_at": row.get::<_, i64>(2)?,
                    "hits": row.get::<_, i64>(3)?,
                    "snippet": row.get::<_, String>(4)?,
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = rows.len() > limit;
        let rows = rows.into_iter().take(limit).collect();
        Ok((rows, has_more))
    }

    /// pending 首次出现时写 approval/asked；event_id 幂等吸收重复轮询。
    pub fn record_approval_asked(
        &self,
        approval_id: &str,
        task_id: &str,
        app_id: &str,
        application: &str,
    ) -> Result<(), StoreError> {
        let task = {
            let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
            read_task(&connection, task_id)?
        };
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        Self::enqueue_session_event(
            &transaction,
            &task.conversation_id,
            &format!("{}:approval-asked:{}", task.id, approval_id),
            "approval/asked",
            serde_json::json!({
                "task_id": task.id,
                "approval_id": approval_id,
                "app_id": app_id,
                "application": application,
            }),
        )?;
        transaction.commit()?;
        drop(connection);
        self.drain_session_outbox()?;
        Ok(())
    }

    /// 用户审批决定写入 Session 事件；审计失败不阻止审批本身。
    pub fn record_approval(
        &self,
        task_id: &str,
        app_id: &str,
        application: &str,
        allowed: bool,
    ) -> Result<(), StoreError> {
        let task = {
            let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
            read_task(&connection, task_id)?
        };
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        Self::enqueue_session_event(
            &transaction,
            &task.conversation_id,
            &format!("{}:approval:{}", task.id, uuid::Uuid::new_v4()),
            "approval/decision",
            serde_json::json!({
                "task_id": task.id,
                "app_id": app_id,
                "application": application,
                "allowed": allowed,
            }),
        )?;
        transaction.commit()?;
        drop(connection);
        self.drain_session_outbox()?;
        Ok(())
    }

    pub fn session_events(&self, session_id: &str) -> Result<Vec<serde_json::Value>, StoreError> {
        self.session_log.read(session_id)
    }

    /// 文档是数据库内的任务产物，不接受模型指定宿主路径。
    pub fn document(&self, id: &str, title: &str, content: &str) -> Result<Artifact, StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let task = read_task(&transaction, id)?;
        ensure_active(&task)?;
        let artifact = Artifact {
            id: uuid::Uuid::new_v4().to_string(),
            task_id: id.into(),
            title: title.into(),
            media_type: "text/markdown".into(),
            filename: "document.md".into(),
            encoding: "utf8".into(),
            created_at: now(),
        };
        transaction.execute(
            "INSERT INTO artifacts (id,task_id,metadata,content) VALUES (?1,?2,?3,?4)",
            params![artifact.id, id, serde_json::to_string(&artifact)?, content],
        )?;
        append_event(
            &transaction,
            &task,
            &format!("已生成 Markdown 文档：{title}（非空与大小检查通过）"),
        )?;
        transaction.commit()?;
        Ok(artifact)
    }

    pub fn artifacts(&self, id: &str) -> Result<Vec<Artifact>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        read_task(&connection, id)?;
        let mut statement =
            connection.prepare("SELECT metadata FROM artifacts WHERE task_id=?1 ORDER BY rowid")?;
        let rows = statement.query_map([id], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub(crate) fn write_artifact_file(&self, id: &str, base64: &str) -> Result<(), StoreError> {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let bytes = STANDARD.decode(base64.as_bytes()).map_err(|error| {
            StoreError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })?;
        std::fs::write(self.artifacts.join(id), bytes)?;
        Ok(())
    }

    pub fn record_task_attachments(
        &self,
        task_id: &str,
        attachments: &[serde_json::Value],
    ) -> Result<(), StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for attachment in attachments {
            transaction.execute(
                "INSERT OR IGNORE INTO task_attachments(task_id,file_id,metadata,created_at) VALUES(?1,?2,?3,?4)",
                params![task_id, attachment["file_id"].as_str().unwrap_or(""), serde_json::to_string(attachment)?, now()],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_compaction(
        &self,
        conversation_id: &str,
        through_turn: usize,
        summary: &str,
        trigger: &str,
        retained_turns: usize,
        estimated_tokens: usize,
        summary_source: &str,
        duration_ms: u64,
    ) -> Result<serde_json::Value, StoreError> {
        let id = uuid::Uuid::new_v4().to_string();
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        connection.execute(
            "INSERT INTO conversation_compactions(id,conversation_id,through_turn,summary,created_at,trigger,retained_turns,estimated_tokens,summary_source,duration_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![id, conversation_id, through_turn as i64, summary, now(), trigger, retained_turns as i64, estimated_tokens as i64, summary_source, duration_ms as i64],
        )?;
        Self::enqueue_session_event(
            &connection,
            conversation_id,
            &format!("compaction:{through_turn}"),
            "compaction",
            serde_json::json!({
                "summary": summary,
                "through_turn": through_turn,
                "trigger": trigger,
            }),
        )?;
        drop(connection);
        self.drain_session_outbox()?;
        self.session_log
            .replace_turns(conversation_id, through_turn)?;
        Ok(serde_json::json!({
            "id": id, "conversation_id": conversation_id,
            "through_turn": through_turn, "summary": summary, "created_at": now(),
            "trigger": trigger, "retained_turns": retained_turns,
            "estimated_tokens": estimated_tokens, "summary_source": summary_source,
            "duration_ms": duration_ms
        }))
    }

    #[allow(clippy::type_complexity)]
    pub fn latest_compaction(
        &self,
        conversation_id: &str,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let value: Option<(String, i64, String, i64, String, i64, i64, String, i64)> = connection
            .query_row(
                "SELECT id,through_turn,summary,created_at,trigger,retained_turns,estimated_tokens,summary_source,duration_ms FROM conversation_compactions WHERE conversation_id=?1 ORDER BY created_at DESC LIMIT 1",
                [conversation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?)),
            )
            .optional()?;
        Ok(value.map(
            |(
                id,
                through_turn,
                summary,
                created_at,
                trigger,
                retained_turns,
                estimated_tokens,
                summary_source,
                duration_ms,
            )| {
                serde_json::json!({
                    "id": id, "conversation_id": conversation_id,
                    "through_turn": through_turn, "summary": summary, "created_at": created_at,
                    "trigger": trigger, "retained_turns": retained_turns,
                    "estimated_tokens": estimated_tokens, "summary_source": summary_source,
                    "duration_ms": duration_ms
                })
            },
        ))
    }

    pub fn compactions(&self, conversation_id: &str) -> Result<Vec<serde_json::Value>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT id,through_turn,summary,created_at,trigger,retained_turns,estimated_tokens,summary_source,duration_ms FROM conversation_compactions WHERE conversation_id=?1 ORDER BY created_at",
        )?;
        let rows = statement.query_map([conversation_id], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "conversation_id": conversation_id,
                "through_turn": row.get::<_, i64>(1)?,
                "summary": row.get::<_, String>(2)?,
                "created_at": row.get::<_, i64>(3)?,
                "trigger": row.get::<_, String>(4)?,
                "retained_turns": row.get::<_, i64>(5)?,
                "estimated_tokens": row.get::<_, i64>(6)?,
                "summary_source": row.get::<_, String>(7)?,
                "duration_ms": row.get::<_, i64>(8)?,
            }))
        })?;
        rows.map(|row| Ok(row?)).collect()
    }

    pub fn delete_session(&self, session_id: &str) -> Result<usize, StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let ids: Vec<String> = {
            let mut statement = transaction
                .prepare("SELECT id FROM tasks WHERE json_extract(payload,'$.session_id')=?1")?;
            statement
                .query_map([session_id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut child_sessions: Vec<String> = Vec::new();
        for task_id in &ids {
            let mut statement = transaction.prepare(
                "SELECT child_session_id FROM subagent_runs WHERE parent_task_id=?1 AND child_session_id IS NOT NULL",
            )?;
            for row in statement.query_map([task_id], |row| row.get::<_, String>(0))? {
                let session = row?;
                if session != session_id && !child_sessions.contains(&session) {
                    child_sessions.push(session);
                }
            }
        }
        for task_id in &ids {
            transaction.execute("DELETE FROM artifacts WHERE task_id=?1", [task_id])?;
            transaction.execute("DELETE FROM events WHERE task_id=?1", [task_id])?;
            transaction.execute("DELETE FROM tool_call_images WHERE task_id=?1", [task_id])?;
            transaction.execute("DELETE FROM desktop_receipts WHERE permission_id IN (SELECT id FROM permissions WHERE task_id=?1)", [task_id])?;
            transaction.execute("DELETE FROM permission_events WHERE permission_id IN (SELECT id FROM permissions WHERE task_id=?1)", [task_id])?;
            transaction.execute("DELETE FROM permissions WHERE task_id=?1", [task_id])?;
            transaction.execute("DELETE FROM computer_actions WHERE control_id IN (SELECT id FROM computer_sessions WHERE task_id=?1)", [task_id])?;
            transaction.execute("DELETE FROM computer_observations WHERE control_id IN (SELECT id FROM computer_sessions WHERE task_id=?1)", [task_id])?;
            transaction.execute("DELETE FROM computer_sessions WHERE task_id=?1", [task_id])?;
            transaction.execute("DELETE FROM execution_queue WHERE task_id=?1", [task_id])?;
            transaction.execute("DELETE FROM idempotency_keys WHERE task_id=?1", [task_id])?;
            transaction.execute("DELETE FROM tool_calls WHERE task_id=?1", [task_id])?;
            transaction.execute(
                "DELETE FROM subagent_runs WHERE parent_task_id=?1",
                [task_id],
            )?;
            transaction.execute("DELETE FROM task_attachments WHERE task_id=?1", [task_id])?;
            transaction.execute("DELETE FROM tasks WHERE id=?1", [task_id])?;
        }
        let schedule_ids: Vec<String> = {
            let mut statement =
                transaction.prepare("SELECT id FROM scheduled_tasks WHERE conversation_id=?1")?;
            statement
                .query_map([session_id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for schedule_id in &schedule_ids {
            transaction.execute(
                "DELETE FROM scheduled_cron WHERE schedule_id=?1",
                [schedule_id],
            )?;
        }
        transaction.execute(
            "DELETE FROM scheduled_tasks WHERE conversation_id=?1",
            [session_id],
        )?;
        transaction.execute(
            "DELETE FROM conversation_compactions WHERE conversation_id=?1",
            [session_id],
        )?;
        transaction.execute(
            "DELETE FROM session_workspaces WHERE session_id=?1",
            [session_id],
        )?;
        transaction.execute(
            "DELETE FROM session_outbox WHERE session_id=?1",
            [session_id],
        )?;
        transaction.commit()?;
        drop(connection);
        for child_session in child_sessions {
            let _ = self.delete_session(&child_session);
        }
        self.session_log.delete_session(session_id)?;
        Ok(ids.len())
    }

    pub fn session_workspace(
        &self,
        session_id: &str,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        Ok(connection
            .query_row(
                "SELECT session_id,path,name,updated_at,kind FROM session_workspaces WHERE session_id=?1",
                [session_id],
                |row| Ok(serde_json::json!({
                    "session_id": row.get::<_, String>(0)?,
                    "path": row.get::<_, String>(1)?,
                    "name": row.get::<_, String>(2)?,
                    "updated_at": row.get::<_, i64>(3)?,
                    "kind": row.get::<_, String>(4)?,
                })),
            )
            .optional()?)
    }

    pub fn session_workspaces(&self) -> Result<Vec<serde_json::Value>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT session_id,path,name,updated_at,kind FROM session_workspaces ORDER BY updated_at DESC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(serde_json::json!({
                "session_id": row.get::<_, String>(0)?,
                "path": row.get::<_, String>(1)?,
                "name": row.get::<_, String>(2)?,
                "updated_at": row.get::<_, i64>(3)?,
                "kind": row.get::<_, String>(4)?,
            }))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn set_session_workspace(
        &self,
        session_id: &str,
        path: &str,
        name: &str,
        kind: &str,
    ) -> Result<serde_json::Value, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        connection.execute(
            "INSERT INTO session_workspaces(session_id,path,name,updated_at,kind) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(session_id) DO UPDATE SET path=excluded.path,name=excluded.name,updated_at=excluded.updated_at,kind=excluded.kind",
            params![session_id, path, name, now(), kind],
        )?;
        Ok(serde_json::json!({"session_id":session_id,"path":path,"name":name,"kind":kind}))
    }

    pub fn clear_session_workspace(&self, session_id: &str) -> Result<(), StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        connection.execute(
            "DELETE FROM session_workspaces WHERE session_id=?1",
            [session_id],
        )?;
        Ok(())
    }

    pub fn session_mode(&self, session_id: &str) -> Result<Option<serde_json::Value>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        Ok(connection
            .query_row(
                "SELECT goal_objective,goal_phase,goal_rounds,goal_max_rounds,plan_active,updated_at,pending_plan FROM session_modes WHERE session_id=?1",
                [session_id],
                |row| {
                    Ok(serde_json::json!({
                        "session_id": session_id,
                        "goal": row.get::<_, Option<String>>(0)?.map(|objective| serde_json::json!({
                            "objective": objective,
                            "phase": row.get::<_, String>(1).unwrap_or_else(|_| "active".into()),
                            "rounds": row.get::<_, i64>(2).unwrap_or(0),
                            "max_rounds": row.get::<_, i64>(3).unwrap_or(6),
                        })),
                        "plan_active": row.get::<_, i64>(4).unwrap_or(0) != 0,
                        "updated_at": row.get::<_, i64>(5).unwrap_or(0),
                        "pending_plan": row.get::<_, Option<String>>(6).unwrap_or(None),
                    }))
                },
            )
            .optional()?)
    }

    pub fn set_goal(
        &self,
        session_id: &str,
        objective: &str,
        max_rounds: i64,
    ) -> Result<serde_json::Value, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        connection.execute(
            "INSERT INTO session_modes(session_id,goal_objective,goal_phase,goal_rounds,goal_max_rounds,plan_active,updated_at)
             VALUES(?1,?2,'active',0,?3,0,?4)
             ON CONFLICT(session_id) DO UPDATE SET goal_objective=excluded.goal_objective,goal_phase='active',goal_rounds=0,goal_max_rounds=excluded.goal_max_rounds,updated_at=excluded.updated_at",
            params![session_id, objective, max_rounds, now()],
        )?;
        drop(connection);
        self.session_mode(session_id)?.ok_or(StoreError::NotFound)
    }

    pub fn set_goal_phase(
        &self,
        session_id: &str,
        phase: &str,
    ) -> Result<serde_json::Value, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let changed = connection.execute(
            "UPDATE session_modes SET goal_phase=?2,updated_at=?3 WHERE session_id=?1 AND goal_objective IS NOT NULL",
            params![session_id, phase, now()],
        )?;
        drop(connection);
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        self.session_mode(session_id)?.ok_or(StoreError::NotFound)
    }

    pub fn clear_goal(&self, session_id: &str) -> Result<serde_json::Value, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        connection.execute(
            "UPDATE session_modes SET goal_objective=NULL,goal_phase='complete',goal_rounds=0,updated_at=?2 WHERE session_id=?1",
            params![session_id, now()],
        )?;
        drop(connection);
        Ok(self
            .session_mode(session_id)?
            .unwrap_or_else(|| serde_json::json!({"session_id": session_id, "goal": null, "plan_active": false, "updated_at": now()})))
    }

    pub fn set_plan_active(
        &self,
        session_id: &str,
        active: bool,
    ) -> Result<serde_json::Value, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        connection.execute(
            "INSERT INTO session_modes(session_id,goal_objective,goal_phase,goal_rounds,goal_max_rounds,plan_active,updated_at,pending_plan)
             VALUES(?1,NULL,'active',0,6,?2,?3,NULL)
             ON CONFLICT(session_id) DO UPDATE SET plan_active=excluded.plan_active,updated_at=excluded.updated_at,pending_plan=NULL",
            params![session_id, i64::from(active), now()],
        )?;
        drop(connection);
        self.session_mode(session_id)?.ok_or(StoreError::NotFound)
    }

    pub fn set_plan_pending(
        &self,
        session_id: &str,
        plan: &str,
    ) -> Result<serde_json::Value, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let changed = connection.execute(
            "UPDATE session_modes SET pending_plan=?2,plan_active=1,updated_at=?3 WHERE session_id=?1",
            params![session_id, plan, now()],
        )?;
        drop(connection);
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        self.session_mode(session_id)?.ok_or(StoreError::NotFound)
    }

    pub fn clear_plan_pending(&self, session_id: &str) -> Result<serde_json::Value, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        connection.execute(
            "UPDATE session_modes SET pending_plan=NULL,updated_at=?2 WHERE session_id=?1",
            params![session_id, now()],
        )?;
        drop(connection);
        Ok(self.session_mode(session_id)?.unwrap_or_else(|| {
            serde_json::json!({"session_id": session_id, "goal": null, "plan_active": false, "pending_plan": null, "updated_at": now()})
        }))
    }

    pub fn increment_goal_round(&self, session_id: &str) -> Result<serde_json::Value, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let changed = connection.execute(
            "UPDATE session_modes SET goal_rounds=goal_rounds+1,updated_at=?2 WHERE session_id=?1 AND goal_objective IS NOT NULL AND goal_phase='active'",
            params![session_id, now()],
        )?;
        drop(connection);
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        self.session_mode(session_id)?.ok_or(StoreError::NotFound)
    }

    pub fn active_goals(&self) -> Result<Vec<serde_json::Value>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT session_id,goal_objective,goal_rounds,goal_max_rounds FROM session_modes
             WHERE goal_objective IS NOT NULL AND goal_phase='active' AND goal_rounds < goal_max_rounds
             ORDER BY updated_at DESC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(serde_json::json!({
                "session_id": row.get::<_, String>(0)?,
                "objective": row.get::<_, String>(1)?,
                "rounds": row.get::<_, i64>(2)?,
                "max_rounds": row.get::<_, i64>(3)?,
            }))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn create_schedule(
        &self,
        conversation_id: Option<&str>,
        prompt: &str,
        target: &str,
        run_at: i64,
        repeat_seconds: i64,
        max_attempts: i64,
    ) -> Result<serde_json::Value, StoreError> {
        let id = uuid::Uuid::new_v4().to_string();
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        connection.execute(
            "INSERT INTO scheduled_tasks(id,conversation_id,prompt,target,run_at,status,created_at,repeat_seconds,max_attempts) VALUES(?1,?2,?3,?4,?5,'pending',?6,?7,?8)",
            params![id, conversation_id, prompt, target, run_at, now(), repeat_seconds, max_attempts],
        )?;
        Ok(
            serde_json::json!({"id":id,"conversation_id":conversation_id,"prompt":prompt,"target":target,"run_at":run_at,"repeat_seconds":repeat_seconds,"status":"pending","repeat_seconds":repeat_seconds,"attempts":0,"max_attempts":max_attempts}),
        )
    }

    pub fn schedules(&self) -> Result<Vec<serde_json::Value>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT id,conversation_id,prompt,target,run_at,status,task_id,error,created_at,repeat_seconds,attempts,max_attempts FROM scheduled_tasks ORDER BY run_at",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "conversation_id": row.get::<_, Option<String>>(1)?,
                "prompt": row.get::<_, String>(2)?,
                "target": row.get::<_, String>(3)?,
                "run_at": row.get::<_, i64>(4)?,
                "status": row.get::<_, String>(5)?,
                "task_id": row.get::<_, Option<String>>(6)?,
                "error": row.get::<_, Option<String>>(7)?,
                "created_at": row.get::<_, i64>(8)?,
                "repeat_seconds": row.get::<_, i64>(9)?,
                "attempts": row.get::<_, i64>(10)?,
                "max_attempts": row.get::<_, i64>(11)?,
            }))
        })?;
        rows.map(|row| Ok(row?)).collect()
    }

    pub fn cancel_schedule(&self, id: &str) -> Result<(), StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let changed = connection.execute(
            "UPDATE scheduled_tasks SET status='cancelled' WHERE id=?1 AND status='pending'",
            [id],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    /// 原子领取到期计划；claimed 之后不会再被另一个 worker 取走。
    #[allow(clippy::type_complexity)]
    pub fn claim_due_schedule(
        &self,
        now_seconds: i64,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let row: Option<(String, Option<String>, String, String, i64, i64, i64, i64)> = transaction
            .query_row(
                "SELECT id,conversation_id,prompt,target,run_at,repeat_seconds,attempts,max_attempts FROM scheduled_tasks WHERE status='pending' AND run_at<=?1 ORDER BY run_at LIMIT 1",
                [now_seconds],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?)),
            )
            .optional()?;
        let Some((
            id,
            conversation_id,
            prompt,
            target,
            run_at,
            repeat_seconds,
            attempts,
            max_attempts,
        )) = row
        else {
            return Ok(None);
        };
        transaction.execute(
            "UPDATE scheduled_tasks SET status='claimed' WHERE id=?1",
            [&id],
        )?;
        transaction.commit()?;
        Ok(Some(
            serde_json::json!({"id":id,"conversation_id":conversation_id,"prompt":prompt,"target":target,"run_at":run_at,"repeat_seconds":repeat_seconds,"attempts":attempts,"max_attempts":max_attempts}),
        ))
    }

    pub fn retry_schedule(&self, id: &str, next_at: i64) -> Result<(), StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        connection.execute(
            "UPDATE scheduled_tasks SET status='pending',run_at=?2,attempts=attempts+1,error=NULL WHERE id=?1",
            params![id, next_at],
        )?;
        Ok(())
    }

    /// 进程重启后把 claimed 行放回 pending；幂等键保证已创建任务不会重复。
    pub fn recover_schedules(&self) -> Result<usize, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        Ok(connection.execute(
            "UPDATE scheduled_tasks SET status='pending' WHERE status='claimed'",
            [],
        )?)
    }

    pub fn finish_schedule(&self, id: &str, task_id: &str) -> Result<(), StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        connection.execute(
            "UPDATE scheduled_tasks SET status='dispatched',task_id=?2 WHERE id=?1",
            params![id, task_id],
        )?;
        Ok(())
    }

    pub fn fail_schedule(&self, id: &str, error: &str) -> Result<(), StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        connection.execute(
            "UPDATE scheduled_tasks SET status='failed',error=?2 WHERE id=?1",
            params![id, error],
        )?;
        Ok(())
    }

    pub fn set_schedule_cron(&self, id: &str, expression: &str) -> Result<(), StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        connection.execute(
            "INSERT OR REPLACE INTO scheduled_cron(schedule_id,expression) VALUES(?1,?2)",
            params![id, expression],
        )?;
        Ok(())
    }

    pub fn schedule_cron(&self, id: &str) -> Result<Option<String>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        Ok(connection
            .query_row(
                "SELECT expression FROM scheduled_cron WHERE schedule_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// 为子 Agent 创建独立 Session/Task；不进入父任务执行队列。
    pub fn create_child_task(
        &self,
        prompt: &str,
        target: yukii_contracts::ExecutionTarget,
    ) -> Result<Task, StoreError> {
        self.create_child_task_in(&uuid::Uuid::new_v4().to_string(), prompt, target)
    }

    pub fn create_child_turn(
        &self,
        session_id: &str,
        prompt: &str,
        target: yukii_contracts::ExecutionTarget,
    ) -> Result<Task, StoreError> {
        self.create_child_task_in(session_id, prompt, target)
    }

    fn create_child_task_in(
        &self,
        session_id: &str,
        prompt: &str,
        target: yukii_contracts::ExecutionTarget,
    ) -> Result<Task, StoreError> {
        let task = Task {
            conversation_id: session_id.to_owned(),
            conversation_title: prompt.chars().take(80).collect(),
            id: uuid::Uuid::new_v4().to_string(),
            prompt: prompt.to_owned(),
            target,
            status: TaskStatus::Planning,
            created_at: now(),
            result: None,
            error: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            context_tokens: 0,
            context_window_tokens: 0,
            sources: Vec::new(),
        };
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO tasks(id,payload,created_at) VALUES(?1,?2,?3)",
            params![task.id, serde_json::to_string(&task)?, task.created_at],
        )?;
        Self::enqueue_session_event(
            &transaction,
            &task.conversation_id,
            &format!("{}:user", task.id),
            "user/message",
            serde_json::json!({
                "task_id": task.id,
                "prompt": task.prompt,
                "target": task.target,
                "created_at": task.created_at,
            }),
        )?;
        transaction.commit()?;
        drop(connection);
        self.drain_session_outbox()?;
        Ok(task)
    }

    pub fn record_subagent_catalog(
        &self,
        parent_task_id: &str,
        run_id: &str,
        child_session_id: &str,
        prompt: &str,
    ) -> Result<(), StoreError> {
        let task = {
            let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
            read_task(&connection, parent_task_id)?
        };
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        Self::enqueue_session_event(
            &transaction,
            &task.conversation_id,
            &format!("{}:catalog:{}", task.id, run_id),
            "subagent/catalog",
            serde_json::json!({
                "parent_task_id": task.id,
                "agent_id": run_id,
                "session_id": child_session_id,
                "prompt": prompt,
            }),
        )?;
        transaction.commit()?;
        drop(connection);
        self.drain_session_outbox()?;
        Ok(())
    }

    pub fn subagent_run(&self, run_id: &str) -> Result<Option<serde_json::Value>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        Ok(connection
            .query_row(
                "SELECT id,parent_task_id,child_session_id,child_task_id,prompt,status FROM subagent_runs WHERE id=?1",
                [run_id],
                |row| {
                    Ok(serde_json::json!({
                        "id": row.get::<_, String>(0)?,
                        "parent_task_id": row.get::<_, String>(1)?,
                        "child_session_id": row.get::<_, Option<String>>(2)?,
                        "child_task_id": row.get::<_, Option<String>>(3)?,
                        "prompt": row.get::<_, String>(4)?,
                        "status": row.get::<_, String>(5)?,
                    }))
                },
            )
            .optional()?)
    }

    pub fn begin_subagent(
        &self,
        parent_task_id: &str,
        parent_call_id: &str,
        prompt: &str,
        child_session_id: &str,
        child_task_id: &str,
    ) -> Result<String, StoreError> {
        let id = uuid::Uuid::new_v4().to_string();
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        connection.execute(
            "INSERT INTO subagent_runs(id,parent_task_id,parent_call_id,prompt,status,started_at,child_session_id,child_task_id) VALUES(?1,?2,?3,?4,'running',?5,?6,?7)",
            params![id, parent_task_id, parent_call_id, prompt, now(), child_session_id, child_task_id],
        )?;
        Ok(id)
    }

    pub fn finish_subagent(
        &self,
        id: &str,
        result: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        connection.execute(
            "UPDATE subagent_runs SET status=?2,result=?3,error=?4,finished_at=?5 WHERE id=?1",
            params![
                id,
                if error.is_some() {
                    "failed"
                } else {
                    "completed"
                },
                result,
                error,
                now()
            ],
        )?;
        Ok(())
    }

    pub fn subagents_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<serde_json::Value>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT r.id,r.parent_task_id,r.parent_call_id,r.prompt,r.status,r.result,r.error,r.started_at,r.finished_at,r.child_session_id
             FROM subagent_runs r JOIN tasks t ON t.id=r.parent_task_id
             WHERE json_extract(t.payload,'$.session_id')=?1 ORDER BY r.rowid",
        )?;
        let rows = statement
            .query_map([session_id], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "parent_task_id": row.get::<_, String>(1)?,
                    "parent_call_id": row.get::<_, String>(2)?,
                    "prompt": row.get::<_, String>(3)?,
                    "status": row.get::<_, String>(4)?,
                    "result": row.get::<_, Option<String>>(5)?,
                    "error": row.get::<_, Option<String>>(6)?,
                    "started_at": row.get::<_, i64>(7)?,
                    "finished_at": row.get::<_, Option<i64>>(8)?,
                    "child_session_id": row.get::<_, Option<String>>(9)?,
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn subagents(&self, task_id: &str) -> Result<Vec<serde_json::Value>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT id,parent_task_id,parent_call_id,prompt,status,result,error,started_at,finished_at FROM subagent_runs WHERE parent_task_id=?1 ORDER BY started_at",
        )?;
        let rows = statement.query_map([task_id], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "parent_task_id": row.get::<_, String>(1)?,
                "parent_call_id": row.get::<_, String>(2)?,
                "prompt": row.get::<_, String>(3)?,
                "status": row.get::<_, String>(4)?,
                "result": row.get::<_, Option<String>>(5)?,
                "error": row.get::<_, Option<String>>(6)?,
                "started_at": row.get::<_, i64>(7)?,
                "finished_at": row.get::<_, Option<i64>>(8)?,
            }))
        })?;
        rows.map(|row| Ok(row?)).collect()
    }

    pub fn tool_call_images(&self, task_id: &str) -> Result<Vec<serde_json::Value>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT call_id,ordinal,media_type,width,height FROM tool_call_images WHERE task_id=?1 ORDER BY call_id,ordinal",
        )?;
        let rows = statement.query_map([task_id], |row| {
            Ok(serde_json::json!({
                "call_id": row.get::<_, String>(0)?,
                "ordinal": row.get::<_, i64>(1)?,
                "media_type": row.get::<_, String>(2)?,
                "width": row.get::<_, i64>(3)?,
                "height": row.get::<_, i64>(4)?,
            }))
        })?;
        rows.map(|row| Ok(row?)).collect()
    }

    pub fn tool_image(
        &self,
        task_id: &str,
        call_id: &str,
        ordinal: i64,
    ) -> Result<Option<(String, String)>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        Ok(connection
            .query_row(
                "SELECT media_type,base64 FROM tool_call_images WHERE task_id=?1 AND call_id=?2 AND ordinal=?3",
                params![task_id, call_id, ordinal],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    pub fn task_tool_calls(&self, task_id: &str) -> Result<Vec<serde_json::Value>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT call_id,name,arguments,result,execution_state,started_at,finished_at,schema,parent_call_id,depth FROM tool_calls WHERE task_id = ?1 ORDER BY rowid",
        )?;
        let rows = statement.query_map([task_id], |row| {
            Ok(serde_json::json!({
                "call_id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "arguments": row.get::<_, String>(2)?,
                "result": row.get::<_, Option<String>>(3)?,
                "execution_state": row.get::<_, String>(4)?,
                "started_at": row.get::<_, i64>(5)?,
                "finished_at": row.get::<_, Option<i64>>(6)?,
                "schema": row.get::<_, Option<String>>(7)?,
                "parent_call_id": row.get::<_, Option<String>>(8)?,
                "depth": row.get::<_, i64>(9)?,
            }))
        })?;
        rows.map(|row| Ok(row?)).collect()
    }

    pub fn task_attachments(&self, task_id: &str) -> Result<Vec<serde_json::Value>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let mut statement = connection.prepare(
            "SELECT metadata FROM task_attachments WHERE task_id = ?1 ORDER BY created_at, file_id",
        )?;
        let rows = statement.query_map([task_id], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn artifact_file(&self, id: &str) -> Option<std::path::PathBuf> {
        let path = self.artifacts.join(id);
        path.is_file().then_some(path)
    }

    pub fn artifact(&self, id: &str) -> Result<Artifact, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        connection
            .query_row("SELECT metadata FROM artifacts WHERE id=?1", [id], |row| {
                row.get::<_, String>(0)
            })
            .optional()?
            .ok_or(StoreError::NotFound)
            .and_then(|text| serde_json::from_str(&text).map_err(StoreError::from))
    }

    pub fn artifact_content(&self, id: &str) -> Result<String, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        connection
            .query_row("SELECT content FROM artifacts WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()?
            .ok_or(StoreError::NotFound)
    }

    /// 未领取的持久队列保留；已领取任务转为失败，避免重复计费或重放副作用。
    pub fn recover_interrupted(&self) -> Result<usize, StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let payloads = {
            let mut statement = transaction.prepare("SELECT payload FROM tasks")?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut count = 0;
        for payload in payloads {
            let mut task: Task = serde_json::from_str(&payload)?;
            let pending: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM execution_queue WHERE task_id=?1 AND state='pending')",
                [&task.id],
                |row| row.get(0),
            )?;
            if task.status == TaskStatus::Planning && pending {
                continue;
            }
            if matches!(
                task.status,
                TaskStatus::Planning
                    | TaskStatus::Running
                    | TaskStatus::Verifying
                    | TaskStatus::WaitingForUser
            ) {
                task.status = TaskStatus::Failed;
                task.error = Some("服务已重启，上次执行中断；未自动重放，请新建任务。".into());
                save_task(&transaction, &task)?;
                append_event(&transaction, &task, "服务重启，上次执行已标记为中断。")?;
                transaction.execute(
                    "UPDATE execution_queue SET state='done' WHERE task_id=?1",
                    [&task.id],
                )?;
                count += 1;
            }
        }
        transaction.execute(
            "UPDATE tool_calls SET execution_state='outcome_unknown' WHERE execution_state='running' AND result IS NULL",
            [],
        )?;
        transaction.commit()?;
        Ok(count)
    }

    /// 取消操作具备幂等性；重复请求不会创建重复的取消事件。
    pub fn cancel(&self, id: &str) -> Result<Task, StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut task = read_task(&transaction, id)?;
        if task.status == TaskStatus::Cancelled {
            return Ok(task);
        }
        transition(task.status, TaskStatus::Cancelled)?;
        task.status = TaskStatus::Cancelled;
        transaction.execute(
            "UPDATE tasks SET payload = ?1 WHERE id = ?2",
            params![serde_json::to_string(&task)?, id],
        )?;
        transaction.execute(
            "UPDATE execution_queue SET state='done' WHERE task_id=?1",
            [id],
        )?;
        mark_unfinished_tools(&transaction, id)?;
        append_event(&transaction, &task, "任务已取消。")?;
        transaction.commit()?;
        Ok(task)
    }
}

/// 任务终态只关闭调度，不代表已确认外部副作用或进程退出。
fn mark_unfinished_tools(connection: &Connection, id: &str) -> Result<(), StoreError> {
    connection.execute(
        "UPDATE tool_calls SET execution_state='outcome_unknown' WHERE task_id=?1 AND execution_state='running' AND result IS NULL",
        [id],
    )?;
    Ok(())
}

fn enqueue_task(connection: &Connection, task: &mut Task) -> Result<(), StoreError> {
    transition(task.status, TaskStatus::Planning)?;
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM execution_queue WHERE state!='done'",
        [],
        |row| row.get(0),
    )?;
    if count >= 100 {
        return Err(StoreError::QueueFull);
    }
    task.status = TaskStatus::Planning;
    save_task(connection, task)?;
    connection.execute(
        "INSERT INTO execution_queue(task_id,state,requested_at) VALUES(?1,'pending',?2)",
        params![task.id, now()],
    )?;
    append_event(connection, task, "执行意图已持久化，等待 Worker 领取。")
}

fn save_task(connection: &Connection, task: &Task) -> Result<(), StoreError> {
    connection.execute(
        "UPDATE tasks SET payload=?1 WHERE id=?2",
        params![serde_json::to_string(task)?, task.id],
    )?;
    Ok(())
}
pub(crate) fn ensure_active(task: &Task) -> Result<(), StoreError> {
    if matches!(
        task.status,
        TaskStatus::Planning | TaskStatus::Running | TaskStatus::Verifying
    ) {
        Ok(())
    } else {
        Err(TaskError::InvalidTransition.into())
    }
}

pub(crate) fn read_task(connection: &Connection, id: &str) -> Result<Task, StoreError> {
    let payload: Option<String> = connection
        .query_row("SELECT payload FROM tasks WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .optional()?;
    Ok(serde_json::from_str(&payload.ok_or(StoreError::NotFound)?)?)
}

pub(crate) fn append_event(
    connection: &Connection,
    task: &Task,
    message: &str,
) -> Result<(), StoreError> {
    let event = TaskEvent {
        sequence: 0,
        task_id: task.id.clone(),
        status: task.status,
        message: message.into(),
        channel: None,
        delta: None,
        created_at: now(),
        created_at_ms: now_millis(),
    };
    connection.execute(
        "INSERT INTO events (task_id, payload) VALUES (?1, ?2)",
        params![task.id, serde_json::to_string(&event)?],
    )?;
    Ok(())
}

pub(crate) fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

/// 按插入顺序读取，避免秒级时间戳相同导致对话倒序。
fn reasoning_for_task(connection: &Connection, id: &str) -> Result<String, StoreError> {
    let mut statement =
        connection.prepare("SELECT payload FROM events WHERE task_id=?1 ORDER BY rowid")?;
    let rows = statement.query_map([id], |row| row.get::<_, String>(0))?;
    let mut value = String::new();
    for row in rows {
        let event: TaskEvent = serde_json::from_str(&row?)?;
        if event.channel.as_deref() == Some("reasoning")
            && let Some(delta) = event.delta
        {
            value.push_str(&delta);
            if value.chars().count() > 20_000 {
                break;
            }
        }
    }
    Ok(value)
}

fn read_conversation(connection: &Connection, id: &str) -> Result<Vec<Task>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT payload FROM tasks WHERE json_extract(payload,'$.session_id')=?1 ORDER BY rowid",
    )?;
    let rows = statement.query_map([id], |row| row.get::<_, String>(0))?;
    let tasks: Vec<Task> = rows
        .map(|row| Ok(serde_json::from_str(&row?)?))
        .collect::<Result<_, StoreError>>()?;
    if tasks.is_empty() {
        return Err(StoreError::NotFound);
    }
    Ok(tasks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use yukii_contracts::ExecutionTarget;

    #[test]
    fn persists_reasoning_into_jsonl() {
        let base = std::env::temp_dir().join(format!("yukii-reasoning-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let path = base.join("tasks.sqlite");
        let root = base.join("sessions");
        let store = TaskStore::open_with_session_root(&path, &root).unwrap();
        let task = store
            .create(CreateTask {
                conversation_id: None,
                prompt: "reasoning".into(),
                target: ExecutionTarget::CloudOffice,
            })
            .unwrap();
        store
            .advance(&task.id, TaskStatus::Planning, "planning", None, None)
            .unwrap();
        store
            .advance(&task.id, TaskStatus::Running, "running", None, None)
            .unwrap();
        store
            .record_delta(&task.id, "reasoning", "weigh options")
            .unwrap();
        store.add_usage(&task.id, 10, 2, 3, 0, 1000).unwrap();
        store.add_usage(&task.id, 20, 4, 1, 0, 1000).unwrap();
        store
            .advance(&task.id, TaskStatus::Verifying, "verifying", None, None)
            .unwrap();
        store
            .advance(
                &task.id,
                TaskStatus::Completed,
                "done",
                Some("ok".into()),
                None,
            )
            .unwrap();
        let events = store.session_log.read(&task.conversation_id).unwrap();
        let assistant = events
            .iter()
            .find(|event| event["type"] == "assistant/message")
            .unwrap();
        assert_eq!(assistant["data"]["reasoning"], "weigh options");
        assert_eq!(
            events
                .iter()
                .filter(|event| event["type"] == "assistant/attempt")
                .count(),
            2
        );
        drop(store);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn deleting_parent_removes_child_sessions() {
        let base = std::env::temp_dir().join(format!("yukii-subagent-delete-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let store =
            TaskStore::open_with_session_root(base.join("tasks.sqlite"), base.join("sessions"))
                .unwrap();
        let parent = store
            .create(CreateTask {
                conversation_id: None,
                prompt: "parent".into(),
                target: ExecutionTarget::CloudOffice,
            })
            .unwrap();
        let child = store
            .create_child_task("child", ExecutionTarget::CloudOffice)
            .unwrap();
        store
            .begin_subagent(
                &parent.id,
                "call",
                "child",
                &child.conversation_id,
                &child.id,
            )
            .unwrap();
        assert!(base.join("sessions").join(&child.conversation_id).exists());
        store.delete_session(&parent.conversation_id).unwrap();
        assert!(store.get(&parent.id).is_err());
        assert!(store.get(&child.id).is_err());
        assert!(!base.join("sessions").join(&child.conversation_id).exists());
        drop(store);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn prunes_only_old_delivered_outbox() {
        let base = std::env::temp_dir().join(format!("yukii-prune-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let store =
            TaskStore::open_with_session_root(base.join("tasks.sqlite"), base.join("sessions"))
                .unwrap();
        {
            let connection = store.connection.lock().unwrap();
            connection
                .execute(
                    "INSERT INTO session_outbox(event_id,session_id,event_type,payload,created_at,delivered) VALUES('old','s','x','{}',0,1)",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO session_outbox(event_id,session_id,event_type,payload,created_at,delivered) VALUES('new','s','x','{}',?1,1)",
                    [now()],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO session_outbox(event_id,session_id,event_type,payload,created_at,delivered) VALUES('pending','s','x','{}',0,0)",
                    [],
                )
                .unwrap();
        }
        assert_eq!(store.prune_session_outbox(7).unwrap(), 1);
        let connection = store.connection.lock().unwrap();
        let remaining: Vec<String> = connection
            .prepare("SELECT event_id FROM session_outbox ORDER BY event_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(remaining, vec!["new".to_string(), "pending".to_string()]);
        drop(connection);
        drop(store);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn replays_pending_outbox_on_reopen() {
        let base = std::env::temp_dir().join(format!("yukii-outbox-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let path = base.join("tasks.sqlite");
        let root = base.join("sessions");
        let store = TaskStore::open_with_session_root(&path, &root).unwrap();
        let task = store
            .create(CreateTask {
                conversation_id: None,
                prompt: "outbox recovery".into(),
                target: ExecutionTarget::CloudOffice,
            })
            .unwrap();
        {
            let connection = store.connection.lock().unwrap();
            connection
                .execute(
                    "INSERT INTO session_outbox(event_id,session_id,event_type,payload,created_at) VALUES(?1,?2,?3,?4,?5)",
                    params![
                        "pending:test",
                        task.conversation_id,
                        "assistant/message",
                        serde_json::json!({"task_id": task.id, "content": "RECOVERED", "status": "completed"}).to_string(),
                        now()
                    ],
                )
                .unwrap();
        }
        drop(store);
        let reopened = TaskStore::open_with_session_root(&path, &root).unwrap();
        let events = reopened.session_log.read(&task.conversation_id).unwrap();
        assert!(events.iter().any(|event| event["id"] == "pending:test"));
        let delivered: i64 = reopened
            .connection
            .lock()
            .unwrap()
            .query_row(
                "SELECT delivered FROM session_outbox WHERE event_id='pending:test'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(delivered, 1);
        drop(reopened);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn persists_tasks_and_cancellation_is_idempotent() {
        let path = std::env::temp_dir().join(format!("yukii-{}.sqlite", Uuid::new_v4()));
        let store = TaskStore::open(&path).unwrap();
        let task = store
            .create(CreateTask {
                conversation_id: None,
                prompt: "分析月报".into(),
                target: ExecutionTarget::CloudOffice,
            })
            .unwrap();
        assert_eq!(store.search_sessions("分析月报", 10, 0).unwrap().0.len(), 1);
        store.cancel(&task.id).unwrap();
        store.cancel(&task.id).unwrap();
        assert_eq!(store.events(&task.id).unwrap().len(), 2);
        drop(store);
        let reopened = TaskStore::open(&path).unwrap();
        assert_eq!(
            reopened.get(&task.id).unwrap().status,
            TaskStatus::Cancelled
        );
        drop(reopened);
        std::fs::remove_file(path).unwrap();
    }
}
