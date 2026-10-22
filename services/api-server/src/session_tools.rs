//! 模型可调用的 Session 历史查询：FTS 搜索与 JSONL 时间线。
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{sync::Arc, time::Instant};
use yukii_agent_ports::{
    model::ToolDefinition,
    repository::{RepositoryError, TaskRepository},
    tools::{ExecutionTarget, Tool, ToolCatalog, ToolContext, ToolEffect},
};

pub struct SessionTools {
    store: Arc<dyn TaskRepository>,
}

impl SessionTools {
    pub fn new(store: Arc<dyn TaskRepository>) -> Self {
        Self { store }
    }
}

#[derive(Deserialize)]
struct SearchInput {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
}

#[derive(Deserialize)]
struct TimelineInput {
    session_id: String,
    #[serde(default)]
    limit: Option<usize>,
}

struct SessionTool {
    store: Arc<dyn TaskRepository>,
    kind: Kind,
}

#[derive(Clone, Copy)]
enum Kind {
    Search,
    Timeline,
}

#[async_trait]
impl ToolCatalog for SessionTools {
    async fn tools(&self) -> Result<Vec<Arc<dyn Tool>>, RepositoryError> {
        Ok([Kind::Search, Kind::Timeline]
            .into_iter()
            .map(|kind| {
                Arc::new(SessionTool {
                    store: self.store.clone(),
                    kind,
                }) as Arc<dyn Tool>
            })
            .collect())
    }
}

#[async_trait]
impl Tool for SessionTool {
    fn definition(&self) -> ToolDefinition {
        let (name, description, schema) = match self.kind {
            Kind::Search => (
                "session_search",
                "搜索历史 Session 的 prompt/result（FTS5 BM25，短词回退 LIKE）；用于找回过去做过的工作。",
                json!({"type":"object","properties":{"query":{"type":"string","maxLength":200},"limit":{"type":"integer","minimum":1,"maximum":50},"offset":{"type":"integer","minimum":0}},"required":["query"],"additionalProperties":false}),
            ),
            Kind::Timeline => (
                "session_timeline",
                "读取指定 Session 的 JSONL 事件时间线（最近 N 条），用于复盘工具调用与状态。",
                json!({"type":"object","properties":{"session_id":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100}},"required":["session_id"],"additionalProperties":false}),
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
        match self.kind {
            Kind::Search => {
                let input: SearchInput =
                    serde_json::from_value(arguments).map_err(|_| "搜索参数无效")?;
                let query = input.query.trim();
                if query.is_empty() || query.chars().count() > 200 {
                    return Err("搜索词需要 1–200 字".into());
                }
                let started = Instant::now();
                let current_session = self
                    .store
                    .get(&context.task_id)
                    .map(|task| task.conversation_id)
                    .unwrap_or_default();
                let limit = input.limit.unwrap_or(20).clamp(1, 50);
                let offset = input.offset.unwrap_or(0).min(10_000);
                // 先排除当前 Session 再分页，避免过滤后页码错位；最多取前 50 条索引结果。
                let fetch = offset.saturating_add(limit).saturating_add(1).min(50);
                let (sessions, _) = self
                    .store
                    .search_sessions(query, fetch, 0)
                    .map_err(|error| error.to_string())?;
                let filtered = sessions
                    .into_iter()
                    .filter(|session| {
                        session["session_id"].as_str() != Some(current_session.as_str())
                    })
                    .collect::<Vec<_>>();
                let has_more = filtered.len() > offset.saturating_add(limit);
                let sessions = filtered
                    .into_iter()
                    .skip(offset)
                    .take(limit)
                    .collect::<Vec<_>>();
                Ok(ToolEffect::Result(json!({
                    "sessions": sessions,
                    "has_more": has_more,
                    "next_offset": has_more.then_some(offset + sessions.len()),
                    "elapsed_ms": started.elapsed().as_millis(),
                })))
            }
            Kind::Timeline => {
                let input: TimelineInput =
                    serde_json::from_value(arguments).map_err(|_| "时间线参数无效")?;
                if uuid::Uuid::parse_str(&input.session_id).is_err() {
                    return Err("session_id 必须是 UUID".into());
                }
                let mut events = self
                    .store
                    .session_events(&input.session_id)
                    .map_err(|error| error.to_string())?;
                let limit = input.limit.unwrap_or(50).clamp(1, 100);
                if events.len() > limit {
                    events = events.split_off(events.len() - limit);
                }
                Ok(ToolEffect::Result(json!({
                    "session_id": input.session_id,
                    "events": events,
                })))
            }
        }
    }
}
