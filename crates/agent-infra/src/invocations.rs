//! 工具调用账本：登记先于执行，结果、数据库产物与事件在同一事务提交。
use crate::{
    StoreError, TaskStore,
    sqlite::{append_event, ensure_active, now, now_millis, read_task},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use yukii_agent_domain::TaskError;
use yukii_agent_ports::{
    model::{ToolCall, ToolResult},
    repository::ToolCompletion,
};
use yukii_contracts::{Artifact, invocation::ToolInvocation};

impl TaskStore {
    /// 相同调用只复用已提交结果；正在执行或结果未知的调用不得再次执行。
    pub fn begin_tool(
        &self,
        id: &str,
        call: &ToolCall,
        schema: Option<&str>,
        parent_call_id: Option<&str>,
        depth: u32,
    ) -> Result<Option<ToolResult>, StoreError> {
        let serialized_arguments = call.arguments.to_string();
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let task = read_task(&transaction, id)?;
        ensure_active(&task)?;
        let existing: Option<(String, String, Option<String>)> = transaction
            .query_row(
                "SELECT name,arguments,result FROM tool_calls WHERE task_id=?1 AND call_id=?2",
                params![id, call.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((name, arguments, result)) = existing {
            if name != call.name || arguments != serialized_arguments {
                return Err(StoreError::IdempotencyConflict);
            }
            return match result {
                Some(value) => Ok(Some(serde_json::from_str(&value)?)),
                None => Err(TaskError::InvalidTransition.into()),
            };
        }
        transaction.execute(
            "INSERT INTO tool_calls(task_id,call_id,name,arguments,execution_id,execution_state,started_at,schema,parent_call_id,depth)
             VALUES(?1,?2,?3,?4,?5,'running',?6,?7,?8,?9)",
            params![
                id,
                call.id,
                call.name,
                serialized_arguments,
                uuid::Uuid::new_v4().to_string(),
                now_millis(),
                schema,
                parent_call_id,
                depth
            ],
        )?;
        append_event(&transaction, &task, "工具调用已登记，等待执行结果。")?;
        Self::enqueue_session_event(
            &transaction,
            &task.conversation_id,
            &format!("{}:call:{}", task.id, call.id),
            "tool/call",
            serde_json::json!({
                "task_id": task.id,
                "call_id": call.id,
                "name": call.name,
                "arguments": call.arguments.to_string(),
                "parent_call_id": parent_call_id,
                "depth": depth,
            }),
        )?;
        transaction.commit()?;
        drop(connection);
        self.drain_session_outbox()?;
        Ok(None)
    }

    pub fn invocation(&self, id: &str, call_id: &str) -> Result<ToolInvocation, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let (execution_id, tool_name, state): (String, String, String) = connection
            .query_row(
                "SELECT execution_id,name,execution_state FROM tool_calls WHERE task_id=?1 AND call_id=?2",
                params![id, call_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or(StoreError::NotFound)?;
        Ok(ToolInvocation {
            execution_id,
            task_id: id.into(),
            call_id: call_id.into(),
            tool_name,
            state: serde_json::from_value(serde_json::Value::String(state))?,
        })
    }

    pub fn finish_tool(
        &self,
        id: &str,
        call: &ToolCall,
        result: &ToolResult,
    ) -> Result<(), StoreError> {
        self.complete_tool(id, call, ToolCompletion::Result(result.clone()))?;
        Ok(())
    }

    /// 校验登记身份后才写产物；任意 SQL/序列化/事件失败都会回滚整个完成事务。
    /// 该保证只覆盖同一 SQLite，不能延伸为本机文件或外部命令的恰好一次执行。
    pub fn complete_tool(
        &self,
        id: &str,
        call: &ToolCall,
        completion: ToolCompletion,
    ) -> Result<ToolResult, StoreError> {
        let serialized_arguments = call.arguments.to_string();
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let task = read_task(&transaction, id)?;
        ensure_active(&task)?;
        let registered: Option<(String, String, String, Option<String>)> = transaction
            .query_row(
                "SELECT name,arguments,execution_state,result FROM tool_calls WHERE task_id=?1 AND call_id=?2",
                params![id, call.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((name, arguments, state, previous_result)) = registered else {
            return Err(TaskError::InvalidTransition.into());
        };
        if name != call.name || arguments != serialized_arguments {
            return Err(StoreError::IdempotencyConflict);
        }
        if state != "running" || previous_result.is_some() {
            return Err(TaskError::InvalidTransition.into());
        }
        let (result, message, workspace_copy) = match completion {
            ToolCompletion::Result(result) => {
                if result.call_id != call.id {
                    return Err(StoreError::IdempotencyConflict);
                }
                let message = if result.is_error {
                    "工具执行返回错误，回执已保存。"
                } else {
                    "工具执行完成，回执已保存。"
                };
                (result, message.to_owned(), None)
            }
            ToolCompletion::Document { title, content } => {
                let workspace_copy = ("document.md".to_owned(), content.as_bytes().to_vec());
                let artifact = Artifact {
                    id: uuid::Uuid::new_v4().to_string(),
                    task_id: id.into(),
                    title,
                    media_type: "text/markdown".into(),
                    filename: "document.md".into(),
                    encoding: "utf8".into(),
                    created_at: now(),
                };
                transaction.execute(
                    "INSERT INTO artifacts(id,task_id,metadata,content) VALUES(?1,?2,?3,?4)",
                    params![artifact.id, id, serde_json::to_string(&artifact)?, content],
                )?;
                let result = ToolResult {
                    call_id: call.id.clone(),
                    content: serde_json::json!({
                        "artifact_id":artifact.id, "title":artifact.title,
                        "format":"markdown",
                        "download_url":format!("/api/artifacts/{}/download",artifact.id)
                    })
                    .to_string(),
                    is_error: false,
                    images: Vec::new(),
                };
                (
                    result,
                    format!("已保存 Markdown 文档及工具回执：{}", artifact.title),
                    Some(workspace_copy),
                )
            }
            ToolCompletion::Artifact {
                title,
                filename,
                media_type,
                base64,
            } => {
                let workspace_copy = STANDARD
                    .decode(base64.as_bytes())
                    .ok()
                    .map(|bytes| (filename.clone(), bytes));
                let artifact = Artifact {
                    id: uuid::Uuid::new_v4().to_string(),
                    task_id: id.into(),
                    title,
                    media_type,
                    filename,
                    encoding: "base64".into(),
                    created_at: now(),
                };
                self.write_artifact_file(&artifact.id, &base64)?;
                transaction.execute(
                    "INSERT INTO artifacts(id,task_id,metadata,content) VALUES(?1,?2,?3,?4)",
                    params![artifact.id, id, serde_json::to_string(&artifact)?, base64],
                )?;
                let result = ToolResult {
                    call_id: call.id.clone(),
                    content: serde_json::json!({
                        "artifact_id":artifact.id, "title":artifact.title,
                        "format":artifact.media_type,
                        "filename":artifact.filename,
                        "download_url":format!("/api/artifacts/{}/download",artifact.id)
                    })
                    .to_string(),
                    is_error: false,
                    images: Vec::new(),
                };
                (
                    result,
                    format!("已保存二进制产物及工具回执：{}", artifact.title),
                    workspace_copy,
                )
            }
            ToolCompletion::Plan { steps } => (
                ToolResult {
                    call_id: call.id.clone(),
                    content: "计划已保存".into(),
                    is_error: false,
                    images: Vec::new(),
                },
                format!(
                    "任务计划：\n{}",
                    steps
                        .iter()
                        .enumerate()
                        .map(|(index, step)| format!("{}. {step}", index + 1))
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
                None,
            ),
        };
        for (ordinal, image) in result.images.iter().enumerate() {
            transaction.execute(
                "INSERT OR REPLACE INTO tool_call_images(task_id,call_id,ordinal,media_type,base64,width,height) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![
                    id,
                    call.id,
                    ordinal as i64,
                    image.media_type,
                    image.base64,
                    image.width as i64,
                    image.height as i64
                ],
            )?;
        }
        let state = if result.is_error {
            "failed"
        } else {
            "succeeded"
        };
        let changed = transaction.execute(
            "UPDATE tool_calls SET result=?1,execution_state=?2,finished_at=?3
             WHERE task_id=?4 AND call_id=?5 AND execution_state='running' AND result IS NULL",
            params![
                serde_json::to_string(&result)?,
                state,
                now_millis(),
                id,
                call.id
            ],
        )?;
        if changed != 1 {
            return Err(TaskError::InvalidTransition.into());
        }
        append_event(&transaction, &task, &message)?;
        let image_refs = self.session_log.save_images(
            &task.conversation_id,
            &task.id,
            &call.id,
            &result.images,
        )?;
        Self::enqueue_session_event(
            &transaction,
            &task.conversation_id,
            &format!("{}:result:{}", task.id, call.id),
            "tool/result",
            serde_json::json!({
                "task_id": task.id,
                "call_id": call.id,
                "content": result.content,
                "is_error": result.is_error,
                "images": image_refs,
            }),
        )?;
        transaction.commit()?;
        drop(connection);
        self.drain_session_outbox()?;
        if let Some((filename, bytes)) = workspace_copy {
            self.save_artifact_to_workspace(&task, &filename, &bytes);
        }
        Ok(result)
    }

    /// 产物仍以 artifact 为事实源；同时尽力复制到会话工作区，方便用户直接取用。
    fn save_artifact_to_workspace(
        &self,
        task: &yukii_contracts::Task,
        filename: &str,
        bytes: &[u8],
    ) {
        let Ok(Some(workspace)) = self.session_workspace(&task.conversation_id) else {
            return;
        };
        let Some(path) = workspace["path"].as_str() else {
            return;
        };
        let root = std::path::Path::new(path);
        if !root.is_dir() {
            return;
        }
        let name = std::path::Path::new(filename)
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("artifact.bin");
        let _ = std::fs::write(root.join(name), bytes);
    }
}
