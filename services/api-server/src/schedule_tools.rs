//! 模型可调用的本地计划任务工具；权限与 target 仍由 Runtime/ToolGateway 约束。
use async_trait::async_trait;
use chrono::Utc;
use cron::Schedule as CronSchedule;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{str::FromStr, sync::Arc};
use yukii_agent_ports::{
    model::ToolDefinition,
    repository::{RepositoryError, TaskRepository},
    tools::{ExecutionTarget, Tool, ToolCatalog, ToolContext, ToolEffect},
};

pub struct ScheduleTools {
    store: Arc<dyn TaskRepository>,
}

impl ScheduleTools {
    pub fn new(store: Arc<dyn TaskRepository>) -> Self {
        Self { store }
    }
}

#[derive(Deserialize)]
struct CreateInput {
    prompt: String,
    #[serde(default)]
    delay_seconds: i64,
    #[serde(default)]
    repeat_seconds: i64,
    cron: Option<String>,
}

#[derive(Deserialize)]
struct CancelInput {
    id: String,
}

struct ScheduleTool {
    store: Arc<dyn TaskRepository>,
    kind: Kind,
}

#[derive(Clone, Copy)]
enum Kind {
    Create,
    List,
    Cancel,
}

#[async_trait]
impl ToolCatalog for ScheduleTools {
    async fn tools(&self) -> Result<Vec<Arc<dyn Tool>>, RepositoryError> {
        Ok([Kind::Create, Kind::List, Kind::Cancel]
            .into_iter()
            .map(|kind| {
                Arc::new(ScheduleTool {
                    store: self.store.clone(),
                    kind,
                }) as Arc<dyn Tool>
            })
            .collect())
    }
}

#[async_trait]
impl Tool for ScheduleTool {
    fn definition(&self) -> ToolDefinition {
        let (name, description, schema) = match self.kind {
            Kind::Create => (
                "schedule_create",
                "创建一个本机定时任务；可用 delay_seconds、repeat_seconds 或 cron。",
                json!({
                    "type":"object",
                    "properties":{
                        "prompt":{"type":"string","maxLength":8000},
                        "delay_seconds":{"type":"integer","minimum":0},
                        "repeat_seconds":{"type":"integer","minimum":0},
                        "cron":{"type":"string","maxLength":120}
                    },
                    "required":["prompt"],
                    "additionalProperties":false
                }),
            ),
            Kind::List => (
                "schedule_list",
                "列出当前 Session 的定时任务及其状态。",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            Kind::Cancel => (
                "schedule_cancel",
                "按 id 取消当前 Session 的待执行定时任务。",
                json!({
                    "type":"object",
                    "properties":{"id":{"type":"string"}},
                    "required":["id"],
                    "additionalProperties":false
                }),
            ),
        };
        ToolDefinition {
            name: name.into(),
            description: description.into(),
            schema,
        }
    }

    fn execution_target(&self) -> ExecutionTarget {
        ExecutionTarget::CloudOffice
    }

    async fn execute(&self, context: &ToolContext, arguments: Value) -> Result<ToolEffect, String> {
        let task = self
            .store
            .get(&context.task_id)
            .map_err(|e| e.to_string())?;
        let session = task.conversation_id;
        match self.kind {
            Kind::Create => {
                let input: CreateInput =
                    serde_json::from_value(arguments).map_err(|_| "计划任务参数无效")?;
                if input.prompt.trim().is_empty() || input.prompt.len() > 8000 {
                    return Err("计划任务 prompt 需要 1–8000 字".into());
                }
                if input.cron.is_some() && input.repeat_seconds != 0 {
                    return Err("cron 与 repeat_seconds 不能同时设置".into());
                }
                if input.repeat_seconds != 0 && input.repeat_seconds < 60 {
                    return Err("repeat_seconds 必须为 0 或至少 60".into());
                }
                let now = Utc::now().timestamp();
                let run_at = if let Some(expression) = input.cron.as_deref() {
                    let schedule =
                        CronSchedule::from_str(expression).map_err(|_| "cron 表达式无效")?;
                    schedule
                        .upcoming(Utc)
                        .next()
                        .ok_or("cron 没有下一次执行时间")?
                        .timestamp()
                } else {
                    now + input.delay_seconds.max(1)
                };
                let created = self
                    .store
                    .create_schedule(
                        Some(&session),
                        input.prompt.trim(),
                        "cloud_office",
                        run_at,
                        input.repeat_seconds,
                        3,
                    )
                    .map_err(|e| e.to_string())?;
                if let (Some(expr), Some(id)) = (input.cron.as_deref(), created["id"].as_str()) {
                    self.store
                        .set_schedule_cron(id, expr)
                        .map_err(|e| e.to_string())?;
                }
                Ok(ToolEffect::Result(json!({
                    "created": true,
                    "schedule": created
                })))
            }
            Kind::List => {
                let schedules = self
                    .store
                    .schedules()
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .filter(|item| item["conversation_id"].as_str() == Some(session.as_str()))
                    .collect::<Vec<_>>();
                Ok(ToolEffect::Result(json!({"schedules": schedules})))
            }
            Kind::Cancel => {
                let input: CancelInput =
                    serde_json::from_value(arguments).map_err(|_| "需要 id")?;
                let owned = self
                    .store
                    .schedules()
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .any(|item| {
                        item["id"].as_str() == Some(input.id.as_str())
                            && item["conversation_id"].as_str() == Some(session.as_str())
                    });
                if !owned {
                    return Err("计划任务不存在或不属于当前 Session".into());
                }
                self.store
                    .cancel_schedule(&input.id)
                    .map_err(|e| e.to_string())?;
                Ok(ToolEffect::Result(json!({"cancelled": input.id})))
            }
        }
    }
}
