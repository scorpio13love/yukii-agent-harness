pub mod browser_tools;
pub mod cua_approvals;
pub mod dev_tools;
pub mod executor;
pub mod gateway;
pub mod mode_tools;
pub mod office;
pub mod schedule_tools;
pub mod session_tools;
pub mod store;
pub mod web_search;
pub mod workspaces;
use executor::Executor;
use yukii_contracts::Artifact;

use axum::body::Bytes;
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::Utc;
use cron::Schedule as CronSchedule;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::str::FromStr;
use std::sync::Arc;
use std::{convert::Infallible, time::Duration};
use store::TaskStore;
use tower_http::services::{ServeDir, ServeFile};
use yukii_agent_ports::{
    model::{LanguageModel, Message},
    repository::{RepositoryError, TaskRepository},
};
use yukii_contracts::{
    ApiError, CHROMIUM_MAJOR, Capabilities, CreateTask, ExecutionTarget, PROTOCOL_VERSION, Task,
    TaskEvent,
};
use yukii_mcp_client::{McpToolCatalog, McpToolView};

/// HTTP 应用层只依赖存储端口；具体驱动在 main 组合。
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<dyn TaskRepository>,
    pub executor: Arc<Executor>,
    pub office: Option<Arc<office::OfficeServices>>,
    pub mcp: Option<Arc<McpToolCatalog>>,
    pub uploads: std::path::PathBuf,
    pub model: Option<Arc<dyn LanguageModel>>,
    pub web_dist: Option<std::path::PathBuf>,
    pub memory: Option<Arc<yukii_agent_memory::MemoryService>>,
    pub tools: Option<Arc<dyn yukii_agent_ports::tools::ToolCatalog>>,
    pub workspace_root: std::path::PathBuf,
}

pub fn router(store: Arc<TaskStore>) -> Router {
    let executor = Executor::new(store.clone(), None, Default::default());
    configured_router(AppState {
        store,
        executor,
        office: None,
        mcp: None,
        uploads: std::path::PathBuf::from(".data/uploads"),
        model: None,
        web_dist: None,
        memory: None,
        tools: None,
        workspace_root: std::path::PathBuf::from(".data/chats"),
    })
}

pub fn configured_router(state: AppState) -> Router {
    let web_dist = state.web_dist.clone();
    let router = Router::new()
        .merge(office::routes())
        .route(
            "/api/health",
            get(|| async { Json(serde_json::json!({ "status": "ok" })) }),
        )
        .route("/api/ready", get(readiness))
        .route("/api/capabilities", get(capabilities))
        .route("/api/mcp", get(mcp_tools))
        .route("/api/mcp/tools/{name}/enabled", post(set_mcp_tool))
        .route("/api/sessions", get(list_conversations))
        .route("/api/workspaces", get(list_workspaces))
        .route("/api/sessions/search", get(search_sessions_handler))
        .route(
            "/api/sessions/{id}",
            get(conversation).delete(delete_session),
        )
        .route("/api/sessions/{id}/fork", post(fork_session))
        .route("/api/sessions/{id}/timeline", get(session_timeline))
        .route(
            "/api/sessions/{id}/workspace",
            get(get_session_workspace)
                .put(set_session_workspace)
                .delete(clear_session_workspace),
        )
        .route("/api/sessions/{id}/mode", get(get_session_mode))
        .route("/api/sessions/{id}/goal", post(set_session_goal))
        .route("/api/sessions/{id}/plan", post(set_session_plan))
        .route(
            "/api/sessions/{id}/plan/decision",
            post(decide_session_plan),
        )
        .route(
            "/api/sessions/{id}/trajectory",
            get(conversation_trajectory),
        )
        .route("/api/sessions/{id}/compact", post(compact_conversation))
        .route("/api/cua/approvals", get(cua_approvals::list))
        .route("/api/cua/approvals/{id}", post(cua_approvals::decide))
        .route("/api/tools", get(tool_inventory))
        .route("/api/memories", get(list_memories))
        .route("/api/memories/{id}", axum::routing::delete(delete_memory))
        .route("/api/schedules", get(list_schedules).post(create_schedule))
        .route(
            "/api/schedules/{id}",
            axum::routing::delete(cancel_schedule),
        )
        .route("/api/tasks", get(list_tasks).post(create_task))
        .route("/api/tasks/{id}", get(get_task))
        .route("/api/tasks/{id}/events", get(task_events))
        .route("/api/tasks/{id}/stream", get(stream_task))
        .route(
            "/api/files",
            post(upload_file).layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route(
            "/api/uploads",
            post(upload_staging).layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route(
            "/api/uploads/{file_id}",
            axum::routing::delete(delete_staging),
        )
        .route("/api/uploads/{file_id}/content", get(download_staging))
        .route(
            "/api/sessions/{session}/attachments",
            post(upload_attachment).layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route("/api/sessions/{session}/attachments", get(list_attachments))
        .route(
            "/api/sessions/{session}/attachments/{file_id}",
            get(get_attachment).delete(delete_attachment),
        )
        .route(
            "/api/sessions/{session}/attachments/{file_id}/content",
            get(download_attachment),
        )
        .route(
            "/api/sessions/{session}/attachments/{file_id}/attach",
            post(attach_file),
        )
        .route("/api/tasks/{id}/cancel", post(cancel_task))
        .route("/api/tasks/{id}/run", post(run_task))
        .route("/api/tasks/{id}/artifacts", get(task_artifacts))
        .route("/api/tasks/{id}/attachments", get(task_attachments))
        .route("/api/tasks/{id}/tool-calls", get(task_tool_calls))
        .route(
            "/api/tasks/{id}/tool-calls/{call_id}/images/{ordinal}",
            get(task_tool_image),
        )
        .route("/api/tasks/{id}/subagents", get(task_subagents))
        .route(
            "/api/tasks/{id}/subagents/{run_id}/cancel",
            post(cancel_subagent),
        )
        .route("/api/artifacts/{id}/download", get(download_artifact))
        .layer(DefaultBodyLimit::max(512 * 1024))
        .layer(axum::middleware::from_fn(gateway::request_boundary))
        .with_state(state);
    if let Some(path) = web_dist {
        router.fallback_service(
            ServeDir::new(&path).not_found_service(ServeFile::new(path.join("index.html"))),
        )
    } else {
        router
    }
}

async fn mcp_tools(State(state): State<AppState>) -> Result<Json<Vec<McpToolView>>, HttpError> {
    let mcp = state
        .mcp
        .as_ref()
        .ok_or(HttpError(RepositoryError::InvalidRequest("MCP 未配置")))?;
    Ok(Json(mcp.inventory().await))
}

#[derive(Deserialize)]
struct McpToggle {
    enabled: bool,
}

async fn set_mcp_tool(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(input): Json<McpToggle>,
) -> Result<Json<McpToolView>, HttpError> {
    let mcp = state
        .mcp
        .as_ref()
        .ok_or(HttpError(RepositoryError::InvalidRequest("MCP 未配置")))?;
    mcp.set_enabled(&name, input.enabled)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%name, %error, "更新 MCP 工具状态失败");
            HttpError(RepositoryError::InvalidRequest("无法更新 MCP 工具状态"))
        })
}

/// 就绪检查验证数据库可读与 Worker 存活，不向模型供应商发送探活请求。
async fn readiness(State(state): State<AppState>) -> Result<Json<serde_json::Value>, HttpError> {
    if !state.executor.available() || !state.executor.worker_running() {
        return Err(HttpError(RepositoryError::Unavailable));
    }
    database(move || state.store.list()).await?;
    Ok(Json(serde_json::json!({"status":"ready"})))
}

async fn capabilities(State(state): State<AppState>) -> Json<Capabilities> {
    Json(Capabilities {
        protocol_version: PROTOCOL_VERSION,
        chromium_major: CHROMIUM_MAJOR,
        task_creation: state.executor.accepting(),
        agent_execution: state.executor.available(),
        local_computer: false,
        cloud_computer: false,
    })
}

// SQLite 是同步 API，放入阻塞线程，避免阻塞 Tokio 的异步工作线程。
async fn database<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, RepositoryError> + Send + 'static,
) -> Result<T, HttpError> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| {
            tracing::error!(%error, "数据库任务异常");
            HttpError(RepositoryError::Backend)
        })?
        .map_err(HttpError)
}

/// 网页创建任务的扩展输入：workspace_path 表示用户显式选择的项目目录。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateTaskRequest {
    #[serde(rename = "session_id")]
    conversation_id: Option<String>,
    prompt: String,
    target: ExecutionTarget,
    #[serde(default)]
    workspace_path: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    goal_objective: Option<String>,
    #[serde(default)]
    goal_max_rounds: Option<i64>,
    #[serde(default)]
    command: Option<String>,
}

async fn create_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateTaskRequest>,
) -> Result<(StatusCode, Json<Task>), HttpError> {
    let mode = request.mode.clone();
    let goal_objective = request.goal_objective.clone();
    let goal_max_rounds = request.goal_max_rounds;
    let command = request.command.clone();
    let initial_prompt = request.prompt.clone();
    if let Some(command) = command.as_deref()
        && !matches!(command, "/goal" | "/plan")
    {
        return Err(HttpError(RepositoryError::InvalidRequest(
            "command 仅支持 /goal 或 /plan",
        )));
    }
    if let Some(mode) = mode.as_deref()
        && !matches!(mode, "goal" | "plan")
    {
        return Err(HttpError(RepositoryError::InvalidRequest(
            "mode 仅支持 goal 或 plan",
        )));
    }
    let mut input = CreateTask {
        conversation_id: request.conversation_id,
        prompt: request.prompt,
        target: request.target,
    };
    let project_workspace = match request.workspace_path {
        Some(path) if !path.trim().is_empty() => {
            let canonical = std::fs::canonicalize(path.trim())
                .map_err(|_| HttpError(RepositoryError::InvalidRequest("项目工作区路径不存在")))?;
            if !canonical.is_dir() {
                return Err(HttpError(RepositoryError::InvalidRequest(
                    "项目工作区必须是目录",
                )));
            }
            Some(canonical.to_string_lossy().to_string())
        }
        _ => None,
    };
    let attachment_ids: Vec<String> = headers
        .get("x-attachment-ids")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if attachment_ids.len() > 16 {
        return Err(HttpError(RepositoryError::InvalidRequest("最多 16 个附件")));
    }
    let mut attachment_lines = Vec::new();
    let mut attachment_records = Vec::new();
    let mut has_image_attachment = false;
    for file_id in &attachment_ids {
        uuid::Uuid::parse_str(file_id)
            .map_err(|_| HttpError(RepositoryError::InvalidRequest("附件 file_id 必须是 UUID")))?;
        let mut found = None;
        if let Ok(sessions) = std::fs::read_dir(&state.uploads) {
            for session in sessions.flatten() {
                let candidate = session.path().join(format!("{file_id}.json"));
                if candidate.is_file() {
                    found = Some(candidate);
                    break;
                }
            }
        }
        let path = found.ok_or(HttpError(RepositoryError::InvalidRequest("附件不存在")))?;
        let mut metadata: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&path).map_err(|_| HttpError(RepositoryError::Backend))?,
        )
        .map_err(|_| HttpError(RepositoryError::Backend))?;
        metadata["staged"] = serde_json::Value::Bool(false);
        metadata["attached_at"] = serde_json::Value::from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_secs())
                .unwrap_or(0),
        );
        let filename = metadata["filename"].as_str().unwrap_or("附件");
        let media_type = metadata["media_type"]
            .as_str()
            .unwrap_or("application/octet-stream");
        has_image_attachment |= matches!(media_type, "image/png" | "image/jpeg");
        let attachment_session = metadata["session_id"].as_str().unwrap_or("");
        let size = metadata["size"].as_u64().unwrap_or(0);
        let digest = metadata["sha256"].as_str().unwrap_or("");
        let digest_prefix: String = digest.chars().take(12).collect();
        attachment_lines.push(format!(
            "- {filename} ({media_type}, {size} bytes, sha256={digest_prefix}, session_id={attachment_session}, file_id={file_id}, read_only=true)"
        ));
        attachment_records.push(metadata.clone());
        std::fs::write(
            &path,
            serde_json::to_vec(&metadata).map_err(|_| HttpError(RepositoryError::Backend))?,
        )
        .map_err(|_| HttpError(RepositoryError::Backend))?;
    }
    if has_image_attachment && std::env::var("YUKII_VISION_INPUT").as_deref() != Ok("true") {
        return Err(HttpError(RepositoryError::InvalidRequest(
            "图片附件需要 YUKII_VISION_INPUT=true 且模型支持图像输入",
        )));
    }
    if !attachment_lines.is_empty() {
        // 只把附件元数据交给模型；UI 里的 inline token 标签不会进入 prompt。
        input
            .prompt
            .push_str("\n\n当前消息附件（元数据，不是正文）：\n");
        input.prompt.push_str(&attachment_lines.join("\n"));
        input
            .prompt
            .push_str("\n需要内容时调用 read_pdf/read_docx/read_xlsx/read_pptx/read_image。");
    }
    if !state.executor.accepting() {
        return Err(HttpError(RepositoryError::ShuttingDown));
    }
    let key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .filter(|key| {
            !key.is_empty()
                && key.len() <= 128
                && key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
        })
        .ok_or(HttpError(RepositoryError::InvalidRequest(
            "需要 1–128 位字母、数字或 -_.: 组成的 Idempotency-Key",
        )))?
        .to_owned();
    let store = state.store.clone();
    let mut created = database(move || store.create_once(input, &key, false)).await?;
    if let Some(command) = command {
        let store = state.store.clone();
        let task_id = created.task.id.clone();
        created.task = database(move || store.set_task_command(&task_id, &command)).await?;
    }
    if let Some(mode) = mode.as_deref() {
        let store = state.store.clone();
        let session_id = created.task.conversation_id.clone();
        let objective = goal_objective
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| initial_prompt.clone());
        let max_rounds = goal_max_rounds.unwrap_or(6).clamp(1, 20);
        let mode = mode.to_owned();
        database(move || match mode.as_str() {
            "goal" => store.set_goal(&session_id, objective.trim(), max_rounds),
            "plan" => store.set_plan_active(&session_id, true),
            _ => Err(RepositoryError::InvalidRequest("mode 无效")),
        })
        .await?;
    }
    if matches!(created.task.target, ExecutionTarget::CloudOffice) {
        let store = state.store.clone();
        let session_id = created.task.conversation_id.clone();
        if let Some(path) = project_workspace.as_ref() {
            let name = std::path::Path::new(path)
                .file_name()
                .map(|value| value.to_string_lossy().to_string())
                .unwrap_or_else(|| "项目".into());
            let path = path.clone();
            database(move || store.set_session_workspace(&session_id, &path, &name, "project"))
                .await?;
        } else {
            let root = state.workspace_root.clone();
            let title = created.task.conversation_title.clone();
            database(move || {
                workspaces::ensure_default_workspace(
                    store.as_ref(),
                    &root,
                    &session_id,
                    &title,
                    false,
                )
            })
            .await?;
        }
    }
    if !attachment_records.is_empty() {
        let conversation_id = created.task.conversation_id.clone();
        for record in &mut attachment_records {
            if record["session_id"].as_str() != Some("_staging") {
                continue;
            }
            let file_id = record["file_id"].as_str().unwrap_or("");
            let staging = state.uploads.join("_staging");
            let mut source = None;
            if let Ok(entries) = std::fs::read_dir(&staging) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with(file_id) && !name.ends_with(".json") {
                        source = Some(entry.path());
                    }
                }
            }
            if let Some(source) = source {
                let destination = state.uploads.join(&conversation_id);
                std::fs::create_dir_all(&destination)
                    .map_err(|_| HttpError(RepositoryError::Backend))?;
                let target = destination.join(source.file_name().unwrap_or_default());
                std::fs::rename(&source, &target)
                    .map_err(|_| HttpError(RepositoryError::Backend))?;
                let _ = std::fs::remove_file(staging.join(format!("{file_id}.json")));
            }
            record["session_id"] = serde_json::Value::String(conversation_id.clone());
            record["staged"] = serde_json::Value::Bool(false);
            record["attached_at"] = serde_json::Value::from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|value| value.as_secs())
                    .unwrap_or(0),
            );
        }
        let task_id = created.task.id.clone();
        let store = state.store.clone();
        database(move || store.record_task_attachments(&task_id, &attachment_records)).await?;
    }
    if !created.replayed && state.executor.available() {
        state
            .executor
            .start(created.task.id.clone())
            .await
            .map_err(HttpError)?;
    }
    state.executor.notify();
    Ok((
        if created.replayed {
            StatusCode::OK
        } else {
            StatusCode::CREATED
        },
        Json(created.task),
    ))
}
async fn list_conversations(State(state): State<AppState>) -> Result<Json<Vec<Task>>, HttpError> {
    Ok(Json(database(move || state.store.conversations()).await?))
}
#[derive(Deserialize)]
struct TrajectoryQuery {
    limit: Option<usize>,
    before_turn: Option<usize>,
}

#[derive(Deserialize)]
struct ScheduleInput {
    prompt: String,
    #[serde(default)]
    run_at: i64,
    #[serde(alias = "conversation_id")]
    session_id: Option<String>,
    target: Option<String>,
    #[serde(default)]
    repeat_seconds: i64,
    #[serde(default = "default_schedule_attempts")]
    max_attempts: i64,
    cron: Option<String>,
}

fn default_schedule_attempts() -> i64 {
    3
}

async fn tool_inventory(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let catalog = state
        .tools
        .as_ref()
        .ok_or(HttpError(RepositoryError::Unavailable))?;
    let tools = catalog.tools().await.map_err(HttpError)?;
    Ok(Json(serde_json::json!({
        "tools": tools.iter().map(|tool| {
            let definition = tool.definition();
            serde_json::json!({
                "name": definition.name,
                "description": definition.description,
                "target": match tool.execution_target() {
                    yukii_contracts::ExecutionTarget::CloudOffice => "cloud_office",
                    yukii_contracts::ExecutionTarget::LocalComputer => "local_computer",
                }
            })
        }).collect::<Vec<_>>()
    })))
}

async fn list_memories(
    State(state): State<AppState>,
    Query(query): Query<MemoryQuery>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let memory = state
        .memory
        .as_ref()
        .ok_or(HttpError(RepositoryError::Unavailable))?;
    let entries = memory
        .entries(
            query.q.as_deref().unwrap_or(""),
            query.scope.as_deref().unwrap_or(""),
        )
        .map_err(|_| HttpError(RepositoryError::Backend))?;
    Ok(Json(serde_json::json!({ "memories": entries })))
}

async fn delete_memory(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let memory = state
        .memory
        .as_ref()
        .ok_or(HttpError(RepositoryError::Unavailable))?;
    memory
        .delete(&id)
        .map_err(|_| HttpError(RepositoryError::NotFound))?;
    Ok(Json(serde_json::json!({ "deleted": id })))
}

#[derive(Deserialize)]
struct MemoryQuery {
    q: Option<String>,
    scope: Option<String>,
}

async fn list_schedules(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, HttpError> {
    Ok(Json(serde_json::json!({
        "schedules": database(move || state.store.schedules()).await?
    })))
}

async fn create_schedule(
    State(state): State<AppState>,
    Json(input): Json<ScheduleInput>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let prompt = input.prompt.trim();
    if prompt.is_empty() || prompt.len() > 8000 {
        return Err(HttpError(RepositoryError::InvalidRequest(
            "计划任务 prompt 无效",
        )));
    }
    let target = input.target.as_deref().unwrap_or("cloud_office");
    if target != "cloud_office" {
        return Err(HttpError(RepositoryError::InvalidRequest(
            "网页计划任务只支持 cloud_office",
        )));
    }
    if input.repeat_seconds != 0 && input.repeat_seconds < 60 {
        return Err(HttpError(RepositoryError::InvalidRequest(
            "repeat_seconds 必须为 0 或至少 60",
        )));
    }
    let (run_at, cron) = if let Some(expression) = input.cron.as_deref() {
        if input.repeat_seconds != 0 {
            return Err(HttpError(RepositoryError::InvalidRequest(
                "cron 与 repeat_seconds 不能同时设置",
            )));
        }
        let schedule = CronSchedule::from_str(expression)
            .map_err(|_| HttpError(RepositoryError::InvalidRequest("cron 表达式无效")))?;
        let next =
            schedule
                .upcoming(Utc)
                .next()
                .ok_or(HttpError(RepositoryError::InvalidRequest(
                    "cron 没有下一次执行时间",
                )))?;
        (next.timestamp(), Some(expression.to_owned()))
    } else {
        if input.run_at <= 0 {
            return Err(HttpError(RepositoryError::InvalidRequest(
                "run_at 必须是时间戳",
            )));
        }
        (input.run_at, None)
    };
    if let Some(id) = &input.session_id
        && uuid::Uuid::parse_str(id).is_err()
    {
        return Err(HttpError(RepositoryError::InvalidRequest(
            "session_id 必须是 UUID",
        )));
    }
    let prompt = prompt.to_owned();
    let conversation_id = input
        .session_id
        .or_else(|| Some(AUTOMATION_SESSION_ID.to_string()));
    let max_attempts = input.max_attempts.clamp(1, 10);
    let repeat_seconds = input.repeat_seconds;
    Ok(Json(
        database(move || {
            let created = state.store.create_schedule(
                conversation_id.as_deref(),
                &prompt,
                "cloud_office",
                run_at,
                repeat_seconds,
                max_attempts,
            )?;
            if let Some(expression) = cron.as_deref()
                && let Some(id) = created["id"].as_str()
            {
                state.store.set_schedule_cron(id, expression)?;
            }
            Ok(created)
        })
        .await?,
    ))
}

async fn cancel_schedule(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, HttpError> {
    database(move || {
        state.store.cancel_schedule(&id)?;
        Ok(serde_json::json!({"cancelled": id}))
    })
    .await
    .map(Json)
}

async fn task_tool_image(
    State(state): State<AppState>,
    Path((id, call_id, ordinal)): Path<(String, String, i64)>,
) -> Result<Response, HttpError> {
    let image = database({
        let store = state.store.clone();
        move || store.tool_image(&id, &call_id, ordinal)
    })
    .await?
    .ok_or(HttpError(RepositoryError::NotFound))?;
    let bytes = STANDARD
        .decode(image.1)
        .map_err(|_| HttpError(RepositoryError::Backend))?;
    Ok(([(axum::http::header::CONTENT_TYPE, image.0)], bytes).into_response())
}

async fn task_subagents(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, HttpError> {
    Ok(Json(serde_json::json!({
        "subagents": database(move || state.store.subagents(&id)).await?
    })))
}

async fn cancel_subagent(
    State(state): State<AppState>,
    Path((id, run_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let runs = database({
        let store = state.store.clone();
        let id = id.clone();
        move || store.subagents(&id)
    })
    .await?;
    if !runs
        .iter()
        .any(|run| run["id"].as_str() == Some(run_id.as_str()))
    {
        return Err(HttpError(RepositoryError::NotFound));
    }
    if !yukii_agent_runtime::subagent_registry::cancel(&run_id) {
        return Err(HttpError(RepositoryError::InvalidRequest(
            "子 Agent 已结束或无法取消",
        )));
    }
    Ok(Json(serde_json::json!({"cancelled": run_id})))
}

const AUTOMATION_SESSION_ID: &str = "00000000-0000-0000-0000-000000000001";

async fn delete_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let deleted = id.clone();
    let removed = database(move || state.store.delete_session(&id))
        .await
        .inspect_err(|_| tracing::error!("删除 Session 失败"))?;
    Ok(Json(
        serde_json::json!({"deleted": deleted, "tasks_removed": removed}),
    ))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
}

async fn search_sessions_handler(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let q = query.q.trim();
    if q.is_empty() || q.chars().count() > 200 {
        return Err(HttpError(RepositoryError::InvalidRequest(
            "搜索词需要 1–200 字",
        )));
    }
    let q = q.to_owned();
    let limit = query.limit.unwrap_or(20).clamp(1, 50);
    let offset = query.offset.unwrap_or(0).min(10_000);
    let (sessions, has_more) =
        database(move || state.store.search_sessions(&q, limit, offset)).await?;
    let next_offset = has_more.then_some(offset + sessions.len());
    Ok(Json(serde_json::json!({
        "sessions": sessions,
        "has_more": has_more,
        "next_offset": next_offset
    })))
}

async fn session_timeline(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let events = database(move || state.store.session_events(&id)).await?;
    Ok(Json(serde_json::json!({"events": events})))
}

async fn fork_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let source = id.clone();
    let session_id = database(move || state.store.fork_session(&source)).await?;
    Ok(Json(
        serde_json::json!({"session_id": session_id, "forked_from": id}),
    ))
}

async fn list_workspaces(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let store = state.store.clone();
    let root = state.workspace_root.clone();
    let workspaces = database(move || {
        workspaces::backfill_default_workspaces(store.as_ref(), &root)?;
        store.session_workspaces()
    })
    .await?;
    Ok(Json(serde_json::json!({"workspaces": workspaces})))
}

async fn get_session_workspace(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let store = state.store.clone();
    let root = state.workspace_root.clone();
    let workspace = database(move || {
        if let Some(workspace) = store.session_workspace(&id)? {
            return Ok(workspace);
        }
        let tasks = store.conversation(&id)?;
        let title = tasks
            .first()
            .map(|task| task.conversation_title.as_str())
            .unwrap_or("新对话");
        workspaces::ensure_default_workspace(store.as_ref(), &root, &id, title, false)
    })
    .await?;
    Ok(Json(serde_json::json!({"workspace": workspace})))
}

#[derive(Deserialize)]
struct WorkspaceInput {
    path: String,
}

async fn set_session_workspace(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<WorkspaceInput>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let canonical = std::fs::canonicalize(input.path.trim())
        .map_err(|_| HttpError(RepositoryError::InvalidRequest("工作区路径不存在")))?;
    if !canonical.is_dir() {
        return Err(HttpError(RepositoryError::InvalidRequest(
            "工作区必须是目录",
        )));
    }
    let path = canonical.to_string_lossy().to_string();
    if path.len() > 1024 {
        return Err(HttpError(RepositoryError::InvalidRequest("工作区路径过长")));
    }
    let name = canonical
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| "工作区".into());
    let workspace = database(move || {
        state
            .store
            .set_session_workspace(&id, &path, &name, "project")
    })
    .await?;
    Ok(Json(serde_json::json!({"workspace": workspace})))
}

async fn clear_session_workspace(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let store = state.store.clone();
    let root = state.workspace_root.clone();
    let workspace = database(move || {
        let tasks = store.conversation(&id)?;
        let title = tasks
            .first()
            .map(|task| task.conversation_title.as_str())
            .unwrap_or("新对话");
        workspaces::ensure_default_workspace(store.as_ref(), &root, &id, title, true)
    })
    .await?;
    Ok(Json(
        serde_json::json!({"cleared": true, "workspace": workspace}),
    ))
}

async fn get_session_mode(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let mode = database(move || state.store.session_mode(&id)).await?;
    Ok(Json(serde_json::json!({"mode": mode})))
}

#[derive(Deserialize)]
struct GoalInput {
    #[serde(default)]
    objective: Option<String>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    max_goal_rounds: Option<i64>,
}

async fn set_session_goal(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<GoalInput>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let store = state.store.clone();
    let mode = database(move || match input.action.as_deref() {
        Some("clear") => store.clear_goal(&id),
        Some("pause") => store.set_goal_phase(&id, "paused"),
        Some("resume") => store.set_goal_phase(&id, "active"),
        Some("complete") => store.set_goal_phase(&id, "complete"),
        Some("edit") | None => {
            let objective = input
                .objective
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or(RepositoryError::InvalidRequest("goal objective 不能为空"))?;
            let rounds = input.max_goal_rounds.unwrap_or(6).clamp(1, 20);
            store.set_goal(&id, objective, rounds)
        }
        Some(_) => Err(RepositoryError::InvalidRequest("goal action 无效")),
    })
    .await?;
    Ok(Json(serde_json::json!({"mode": mode})))
}

#[derive(Deserialize)]
struct PlanModeInput {
    active: bool,
}

async fn set_session_plan(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<PlanModeInput>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let mode = database(move || {
        state.store.set_plan_active(&id, input.active)?;
        if !input.active {
            state.store.clear_plan_steps(&id)?;
        }
        state
            .store
            .session_mode(&id)?
            .ok_or(RepositoryError::NotFound)
    })
    .await?;
    Ok(Json(serde_json::json!({"mode": mode})))
}

#[derive(Deserialize)]
struct PlanDecisionInput {
    approved: bool,
}

async fn decide_session_plan(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<PlanDecisionInput>,
) -> Result<Json<serde_json::Value>, HttpError> {
    if !input.approved {
        let store = state.store.clone();
        let session_id = id.clone();
        let mode = database(move || store.clear_plan_pending(&session_id)).await?;
        return Ok(Json(serde_json::json!({"approved": false, "mode": mode})));
    }
    let store = state.store.clone();
    let session_id = id.clone();
    let pending = database(move || store.session_mode(&session_id)).await?;
    let _pending_plan = pending
        .as_ref()
        .and_then(|mode| mode["pending_plan"].as_str())
        .filter(|plan| !plan.trim().is_empty())
        .ok_or(HttpError(RepositoryError::InvalidRequest(
            "当前没有待评审的计划",
        )))?;
    let key = format!(
        "plan-approved:{id}:{}",
        pending
            .as_ref()
            .and_then(|mode| mode["updated_at"].as_i64())
            .unwrap_or(0)
    );
    let store = state.store.clone();
    let session_id = id.clone();
    let mode = database(move || {
        store.set_plan_active(&session_id, false)?;
        store
            .session_mode(&session_id)?
            .ok_or(RepositoryError::NotFound)
    })
    .await?;
    let store = state.store.clone();
    let session_id = id.clone();
    let created = database(move || {
        store.create_once(
            CreateTask {
                conversation_id: Some(session_id),
                prompt: "用户已批准你提交的计划。请开始执行该计划：先检查当前工作区与相关文件，再逐步完成并验证结果。每完成一步后调用 update_plan_steps 更新完整步骤状态；全部完成后把所有步骤标记为 completed。".into(),
                target: ExecutionTarget::CloudOffice,
            },
            &key,
            false,
        )
    })
    .await?;
    let store = state.store.clone();
    let root = state.workspace_root.clone();
    let session_id = created.task.conversation_id.clone();
    let title = created.task.conversation_title.clone();
    database(move || {
        workspaces::ensure_default_workspace(store.as_ref(), &root, &session_id, &title, false)
    })
    .await?;
    if !created.replayed && state.executor.available() {
        state
            .executor
            .start(created.task.id.clone())
            .await
            .map_err(HttpError)?;
    }
    state.executor.notify();
    Ok(Json(serde_json::json!({
        "approved": true,
        "mode": mode,
        "task": created.task,
    })))
}

async fn compact_conversation(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, HttpError> {
    if uuid::Uuid::parse_str(&id).is_err() {
        return Err(HttpError(RepositoryError::InvalidRequest(
            "conversation id 必须是 UUID",
        )));
    }
    let conversation_id = id.clone();
    let store = state.store.clone();
    let tasks = database({
        let store = state.store.clone();
        let conversation_id = conversation_id.clone();
        move || store.conversation(&conversation_id)
    })
    .await?;
    if tasks.len() <= 2 {
        return Err(HttpError(RepositoryError::InvalidRequest(
            "至少需要 3 轮对话才能压缩",
        )));
    }
    let through_turn = tasks.len() - 2;
    let raw = tasks
        .iter()
        .take(through_turn)
        .map(|task| {
            let prompt = task.prompt.chars().take(1200).collect::<String>();
            let answer = task
                .result
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(1200)
                .collect::<String>();
            format!("用户：{prompt}\n助手：{answer}")
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let compaction_started = std::time::Instant::now();
    let (summary, summary_source) = if let Some(model) = state.model.as_ref() {
        let system = "你是对话压缩器。把历史整理成供后续 Agent 使用的事实摘要：保留用户目标、已确认事实、未完成事项和关键文件引用；不要编造，不要输出 Markdown 标题，控制在 1200 字以内。";
        match model
            .complete(system, &[Message::User(raw.clone())], &[])
            .await
        {
            Ok(reply) if !reply.text.trim().is_empty() => (reply.text, "llm"),
            Ok(_) => (raw, "fallback"),
            Err(error) => {
                tracing::warn!(%error, "LLM 压缩摘要失败，回退到确定性摘要");
                (raw, "fallback")
            }
        }
    } else {
        (raw, "fallback")
    };
    let summary_source_value = summary_source;
    let duration_ms = compaction_started.elapsed().as_millis() as u64;
    let compaction = database(move || {
        store.add_compaction(
            &conversation_id,
            through_turn,
            &summary,
            "manual",
            2,
            0,
            summary_source_value,
            duration_ms,
        )
    })
    .await?;
    let mut result = compaction;
    result["summary_source"] = serde_json::Value::String(summary_source_value.into());
    Ok(Json(result))
}

async fn conversation_trajectory(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<TrajectoryQuery>,
) -> Result<Json<serde_json::Value>, HttpError> {
    if uuid::Uuid::parse_str(&id).is_err() {
        return Err(HttpError(RepositoryError::InvalidRequest(
            "conversation id 必须是 UUID",
        )));
    }
    let conversation_id = id.clone();
    let store = state.store.clone();
    let tasks = database(move || store.conversation(&conversation_id)).await?;
    let limit = query.limit.unwrap_or(200).clamp(1, 500);
    let before = query.before_turn.unwrap_or(usize::MAX);
    let selected: Vec<(usize, &Task)> = tasks
        .iter()
        .enumerate()
        .filter(|(index, _)| index + 1 < before)
        .collect();
    let start = selected.len().saturating_sub(limit);
    let next_cursor = (start > 0).then_some(start);
    let compactions = {
        let store = state.store.clone();
        let conversation_id = id.clone();
        database(move || store.compactions(&conversation_id)).await?
    };
    let mut records = Vec::new();
    for compaction in compactions {
        records.push(serde_json::json!({
            "id": format!("compaction:{}", compaction["id"].as_str().unwrap_or("")),
            "task_id": serde_json::Value::Null,
            "turn": compaction["through_turn"],
            "step": 0,
            "kind": "compaction",
            "status": "completed",
            "started_at": compaction["created_at"],
            "finished_at": compaction["created_at"],
            "output": compaction["summary"],
            "trigger": compaction["trigger"],
            "retained_turns": compaction["retained_turns"],
            "estimated_tokens": compaction["estimated_tokens"],
            "summary_source": compaction["summary_source"],
            "duration_ms": compaction["duration_ms"],
        }));
    }
    for (turn, task) in selected[start..].iter().copied() {
        let events = database({
            let task_id = task.id.clone();
            let store = state.store.clone();
            move || store.events(&task_id)
        })
        .await?;
        let tools = database({
            let task_id = task.id.clone();
            let store = state.store.clone();
            move || store.task_tool_calls(&task_id)
        })
        .await?;
        let attachments = database({
            let task_id = task.id.clone();
            let store = state.store.clone();
            move || store.task_attachments(&task_id)
        })
        .await?;
        let tool_images = database({
            let task_id = task.id.clone();
            let store = state.store.clone();
            move || store.tool_call_images(&task_id)
        })
        .await?;
        let last_at = events
            .last()
            .map(|event| event.created_at)
            .unwrap_or(task.created_at);
        let first_token = events
            .iter()
            .find(|event| matches!(event.channel.as_deref(), Some("text") | Some("reasoning")))
            .map(|event| {
                if event.created_at_ms > 0 {
                    event.created_at_ms
                } else {
                    event.created_at * 1000
                }
            });
        let ttft_ms = first_token.map(|at| (at - task.created_at * 1000).max(0));
        let retries: Vec<_> = events
            .iter()
            .filter(|event| event.channel.as_deref() == Some("retry"))
            .filter_map(|event| event.delta.clone())
            .collect();
        let reasoning = events
            .iter()
            .filter(|event| event.channel.as_deref() == Some("reasoning"))
            .filter_map(|event| event.delta.clone())
            .collect::<String>();
        records.push(serde_json::json!({
            "id": format!("{}:user", task.id),
            "task_id": task.id,
            "turn": turn + 1,
            "step": 1,
            "kind": "user",
            "status": task.status,
            "started_at": task.created_at,
            "finished_at": last_at,
            "input": task.prompt,
            "attachments": attachments,
        }));
        records.push(serde_json::json!({
            "id": format!("{}:assistant", task.id),
            "task_id": task.id,
            "turn": turn + 1,
            "step": 1,
            "kind": "assistant",
            "status": task.status,
            "started_at": task.created_at,
            "finished_at": last_at,
            "thinking": reasoning,
            "retries": retries,
            "retry_count": retries.len(),
            "timing": { "ttft_ms": ttft_ms },
            "output": task.result,
            "error": task.error,
            "usage": {
                "input_tokens": task.input_tokens,
                "output_tokens": task.output_tokens,
            },
        }));
        for call in tools {
            let started_at = call["started_at"].as_i64().unwrap_or(0);
            let finished_at = call["finished_at"].as_i64();
            records.push(serde_json::json!({
                "id": format!("{}:tool:{}", task.id, call["call_id"].as_str().unwrap_or("")),
                "task_id": task.id,
                "turn": turn + 1,
                "step": 2,
                "kind": "tool",
                "status": call["execution_state"],
                "started_at": started_at,
                "finished_at": finished_at,
                "duration_ms": finished_at.map(|end| (end - started_at).max(0)),
                "tool": {
                    "call_id": call["call_id"],
                    "name": call["name"],
                    "arguments": call["arguments"],
                    "result": call["result"],
                    "schema": call["schema"],
                    "parent_call_id": call["parent_call_id"],
                    "depth": call["depth"],
                    "images": tool_images
                        .iter()
                        .filter(|image| image["call_id"] == call["call_id"])
                        .map(|image| serde_json::json!({
                            "ordinal": image["ordinal"],
                            "media_type": image["media_type"],
                            "url": format!(
                                "/api/tasks/{}/tool-calls/{}/images/{}",
                                task.id, call["call_id"].as_str().unwrap_or(""), image["ordinal"]
                            )
                        }))
                        .collect::<Vec<_>>(),
                },
            }));
        }
        for event in events {
            if event.channel.is_some() || event.message.trim().is_empty() {
                continue;
            }
            records.push(serde_json::json!({
                "id": format!("{}:event:{}", task.id, event.sequence),
                "task_id": task.id,
                "turn": turn + 1,
                "step": 2,
                "kind": "context",
                "status": event.status,
                "started_at": event.created_at,
                "finished_at": event.created_at,
                "output": event.message,
            }));
        }
    }
    let records: Vec<_> = records
        .into_iter()
        .enumerate()
        .map(|(index, mut record)| {
            record["record_index"] = serde_json::json!(index + 1);
            record
        })
        .collect();
    Ok(Json(
        serde_json::json!({ "records": records, "next_cursor": next_cursor }),
    ))
}

async fn conversation(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<Task>>, HttpError> {
    Ok(Json(database(move || state.store.conversation(&id)).await?))
}
async fn list_tasks(State(state): State<AppState>) -> Result<Json<Vec<Task>>, HttpError> {
    Ok(Json(database(move || state.store.list()).await?))
}
async fn get_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Task>, HttpError> {
    Ok(Json(database(move || state.store.get(&id)).await?))
}
async fn task_events(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<TaskEvent>>, HttpError> {
    Ok(Json(database(move || state.store.events(&id)).await?))
}
#[derive(Serialize)]
struct UploadedFile {
    id: String,
    filename: String,
    media_type: String,
    size: usize,
}

#[derive(Serialize)]
struct UploadedAttachment {
    file_id: String,
    session_id: String,
    filename: String,
    media_type: String,
    size: usize,
    sha256: String,
    staged: bool,
}

async fn download_attachment(
    State(state): State<AppState>,
    Path((session, file_id)): Path<(String, String)>,
) -> Result<Response, HttpError> {
    uuid::Uuid::parse_str(&session)
        .map_err(|_| HttpError(RepositoryError::InvalidRequest("session 必须是 UUID")))?;
    uuid::Uuid::parse_str(&file_id)
        .map_err(|_| HttpError(RepositoryError::InvalidRequest("file_id 必须是 UUID")))?;
    let directory = state.uploads.join(&session);
    let metadata_path = directory.join(format!("{file_id}.json"));
    let metadata: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&metadata_path)
            .map_err(|_| HttpError(RepositoryError::InvalidRequest("附件不存在")))?,
    )
    .map_err(|_| HttpError(RepositoryError::Backend))?;
    let mut found = None;
    if let Ok(entries) = std::fs::read_dir(&directory) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&file_id) && !name.ends_with(".json") {
                found = Some(entry.path());
                break;
            }
        }
    }
    let path = found.ok_or(HttpError(RepositoryError::InvalidRequest("附件内容不存在")))?;
    let bytes = std::fs::read(path).map_err(|_| HttpError(RepositoryError::Backend))?;
    let filename = metadata["filename"].as_str().unwrap_or("attachment.bin");
    let media_type = metadata["media_type"]
        .as_str()
        .unwrap_or("application/octet-stream");
    Response::builder()
        .header("content-type", media_type)
        .header("content-disposition", content_disposition(filename))
        .header("x-content-type-options", "nosniff")
        .body(Body::from(bytes))
        .map_err(|_| HttpError(RepositoryError::Backend))
}

async fn delete_attachment(
    State(state): State<AppState>,
    Path((session, file_id)): Path<(String, String)>,
) -> Result<StatusCode, HttpError> {
    uuid::Uuid::parse_str(&session)
        .map_err(|_| HttpError(RepositoryError::InvalidRequest("session 必须是 UUID")))?;
    uuid::Uuid::parse_str(&file_id)
        .map_err(|_| HttpError(RepositoryError::InvalidRequest("file_id 必须是 UUID")))?;
    let directory = state.uploads.join(&session);
    let metadata_path = directory.join(format!("{file_id}.json"));
    let sha = std::fs::read(&metadata_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value["sha256"].as_str().map(str::to_owned));
    let mut removed = false;
    if let Ok(entries) = std::fs::read_dir(&directory) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with(&file_id) {
                std::fs::remove_file(entry.path())
                    .map_err(|_| HttpError(RepositoryError::Backend))?;
                removed = true;
            }
        }
    }
    if !removed {
        return Err(HttpError(RepositoryError::NotFound));
    }
    if let Some(sha) = sha {
        let mut referenced = false;
        if let Ok(sessions) = std::fs::read_dir(&state.uploads) {
            for session in sessions.flatten() {
                if session.file_name() == "_blobs" || !session.path().is_dir() {
                    continue;
                }
                if let Ok(files) = std::fs::read_dir(session.path()) {
                    for file in files.flatten() {
                        if file.path().extension().and_then(|value| value.to_str()) != Some("json")
                        {
                            continue;
                        }
                        if let Ok(bytes) = std::fs::read(file.path())
                            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                            && value["sha256"].as_str() == Some(sha.as_str())
                        {
                            referenced = true;
                            break;
                        }
                    }
                }
                if referenced {
                    break;
                }
            }
        }
        if !referenced {
            let _ = std::fs::remove_file(state.uploads.join("_blobs").join(sha));
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn get_attachment(
    State(state): State<AppState>,
    Path((session, file_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, HttpError> {
    uuid::Uuid::parse_str(&session)
        .map_err(|_| HttpError(RepositoryError::InvalidRequest("session 必须是 UUID")))?;
    uuid::Uuid::parse_str(&file_id)
        .map_err(|_| HttpError(RepositoryError::InvalidRequest("file_id 必须是 UUID")))?;
    let path = state.uploads.join(&session).join(format!("{file_id}.json"));
    let bytes = std::fs::read(&path)
        .map_err(|_| HttpError(RepositoryError::InvalidRequest("附件不存在")))?;
    let metadata: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| HttpError(RepositoryError::Backend))?;
    Ok(Json(metadata))
}

async fn list_attachments(
    State(state): State<AppState>,
    Path(session): Path<String>,
) -> Result<Json<Vec<serde_json::Value>>, HttpError> {
    uuid::Uuid::parse_str(&session)
        .map_err(|_| HttpError(RepositoryError::InvalidRequest("session 必须是 UUID")))?;
    let directory = state.uploads.join(&session);
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&directory) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&path)
                && let Ok(metadata) = serde_json::from_slice::<serde_json::Value>(&bytes)
            {
                files.push(metadata);
            }
        }
    }
    files.sort_by_key(|item| item["filename"].as_str().unwrap_or("").to_owned());
    Ok(Json(files))
}

async fn attach_file(
    State(state): State<AppState>,
    Path((session, file_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, HttpError> {
    uuid::Uuid::parse_str(&session)
        .map_err(|_| HttpError(RepositoryError::InvalidRequest("session 必须是 UUID")))?;
    uuid::Uuid::parse_str(&file_id)
        .map_err(|_| HttpError(RepositoryError::InvalidRequest("file_id 必须是 UUID")))?;
    let path = state.uploads.join(&session).join(format!("{file_id}.json"));
    let bytes = std::fs::read(&path)
        .map_err(|_| HttpError(RepositoryError::InvalidRequest("附件不存在")))?;
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| HttpError(RepositoryError::Backend))?;
    metadata["staged"] = serde_json::Value::Bool(false);
    metadata["attached_at"] = serde_json::Value::from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_secs())
            .unwrap_or(0),
    );
    std::fs::write(
        &path,
        serde_json::to_vec(&metadata).map_err(|_| HttpError(RepositoryError::Backend))?,
    )
    .map_err(|_| HttpError(RepositoryError::Backend))?;
    Ok(Json(metadata))
}

/// 浏览器不能把非 ASCII 文件名放进 XHR header，因此前端传 percent-encoded；
/// 这里解码后只保留最后一段，避免路径分隔符进入元数据。
fn upload_filename(headers: &HeaderMap) -> String {
    let raw = headers
        .get("x-filename")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let decoded = percent_encoding::percent_decode_str(raw).decode_utf8_lossy();
    decoded.rsplit('/').next().unwrap_or("").to_owned()
}

/// Content-Disposition 的 filename 只能是 ISO-8859-1；中文走 RFC 5987 的 filename*。
fn content_disposition(filename: &str) -> String {
    let fallback: String = filename
        .chars()
        .map(|character| {
            if character.is_ascii_graphic() && character != '"' && character != '\\' {
                character
            } else {
                '_'
            }
        })
        .collect();
    let encoded =
        percent_encoding::utf8_percent_encode(filename, percent_encoding::NON_ALPHANUMERIC);
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

/// workspace staging：附件先进入固定 staging bucket，不需要 conversation。
async fn upload_staging(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<UploadedAttachment>, HttpError> {
    const STAGING_SESSION: &str = "_staging";
    let filename = upload_filename(&headers);
    let extension = filename
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_lowercase())
        .filter(|ext| {
            matches!(
                ext.as_str(),
                "docx" | "xlsx" | "pptx" | "pdf" | "png" | "jpg" | "jpeg"
            )
        })
        .ok_or(HttpError(RepositoryError::InvalidRequest(
            "仅支持 docx/xlsx/pptx/pdf/png/jpg/jpeg",
        )))?;
    if body.is_empty() || body.len() > 8 * 1024 * 1024 {
        return Err(HttpError(RepositoryError::InvalidRequest(
            "文件为空或超过 8 MiB",
        )));
    }
    let media_type = match extension.as_str() {
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        _ => "application/pdf",
    };
    let file_id = uuid::Uuid::new_v4().to_string();
    let sha256 = format!("{:x}", Sha256::digest(&body));
    let directory = state.uploads.join(STAGING_SESSION);
    std::fs::create_dir_all(&directory).map_err(|_| HttpError(RepositoryError::Backend))?;
    let blobs = state.uploads.join("_blobs");
    std::fs::create_dir_all(&blobs).map_err(|_| HttpError(RepositoryError::Backend))?;
    let blob = blobs.join(&sha256);
    if !blob.is_file() {
        std::fs::write(&blob, &body).map_err(|_| HttpError(RepositoryError::Backend))?;
    }
    std::fs::write(directory.join(format!("{file_id}.{extension}")), &body)
        .map_err(|_| HttpError(RepositoryError::Backend))?;
    let metadata = serde_json::json!({
        "file_id": file_id, "session_id": STAGING_SESSION, "filename": filename,
        "media_type": media_type, "size": body.len(), "sha256": sha256, "staged": true
    });
    std::fs::write(
        directory.join(format!("{file_id}.json")),
        serde_json::to_vec(&metadata).map_err(|_| HttpError(RepositoryError::Backend))?,
    )
    .map_err(|_| HttpError(RepositoryError::Backend))?;
    Ok(Json(UploadedAttachment {
        file_id,
        session_id: STAGING_SESSION.into(),
        filename,
        media_type: media_type.into(),
        size: body.len(),
        sha256,
        staged: true,
    }))
}

async fn delete_staging(
    State(state): State<AppState>,
    Path(file_id): Path<String>,
) -> Result<StatusCode, HttpError> {
    uuid::Uuid::parse_str(&file_id)
        .map_err(|_| HttpError(RepositoryError::InvalidRequest("file_id 必须是 UUID")))?;
    let directory = state.uploads.join("_staging");
    let mut removed = false;
    if let Ok(entries) = std::fs::read_dir(&directory) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&file_id) {
                std::fs::remove_file(entry.path())
                    .map_err(|_| HttpError(RepositoryError::Backend))?;
                removed = true;
            }
        }
    }
    if !removed {
        return Err(HttpError(RepositoryError::NotFound));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn download_staging(
    State(state): State<AppState>,
    Path(file_id): Path<String>,
) -> Result<Response, HttpError> {
    uuid::Uuid::parse_str(&file_id)
        .map_err(|_| HttpError(RepositoryError::InvalidRequest("file_id 必须是 UUID")))?;
    let directory = state.uploads.join("_staging");
    let metadata_path = directory.join(format!("{file_id}.json"));
    let metadata: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&metadata_path)
            .map_err(|_| HttpError(RepositoryError::InvalidRequest("附件不存在")))?,
    )
    .map_err(|_| HttpError(RepositoryError::Backend))?;
    let mut found = None;
    if let Ok(entries) = std::fs::read_dir(&directory) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&file_id) && !name.ends_with(".json") {
                found = Some(entry.path());
            }
        }
    }
    let path = found.ok_or(HttpError(RepositoryError::InvalidRequest("附件内容不存在")))?;
    let bytes = std::fs::read(path).map_err(|_| HttpError(RepositoryError::Backend))?;
    let filename = metadata["filename"].as_str().unwrap_or("attachment.bin");
    let media_type = metadata["media_type"]
        .as_str()
        .unwrap_or("application/octet-stream");
    Response::builder()
        .header("content-type", media_type)
        .header("content-disposition", content_disposition(filename))
        .header("x-content-type-options", "nosniff")
        .body(Body::from(bytes))
        .map_err(|_| HttpError(RepositoryError::Backend))
}

async fn upload_attachment(
    State(state): State<AppState>,
    Path(session): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<UploadedAttachment>, HttpError> {
    uuid::Uuid::parse_str(&session)
        .map_err(|_| HttpError(RepositoryError::InvalidRequest("session 必须是 UUID")))?;
    // 仅测试用：放慢响应，给浏览器取消上传留出可操作窗口。
    if let Ok(delay) = std::env::var("YUKII_TEST_UPLOAD_DELAY_MS")
        && let Ok(ms) = delay.parse::<u64>()
        && (1..=30_000).contains(&ms)
    {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }
    let filename = upload_filename(&headers);
    let extension = filename
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_lowercase())
        .filter(|ext| {
            matches!(
                ext.as_str(),
                "docx" | "xlsx" | "pptx" | "pdf" | "png" | "jpg" | "jpeg"
            )
        })
        .ok_or(HttpError(RepositoryError::InvalidRequest(
            "仅支持 docx/xlsx/pptx/pdf/png/jpg/jpeg",
        )))?;
    if body.is_empty() || body.len() > 8 * 1024 * 1024 {
        return Err(HttpError(RepositoryError::InvalidRequest(
            "文件为空或超过 8 MiB",
        )));
    }
    let media_type = match extension.as_str() {
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        _ => "application/pdf",
    };
    let file_id = uuid::Uuid::new_v4().to_string();
    let sha256 = format!("{:x}", Sha256::digest(&body));
    let directory = state.uploads.join(&session);
    std::fs::create_dir_all(&directory).map_err(|_| HttpError(RepositoryError::Backend))?;
    let blobs = state.uploads.join("_blobs");
    std::fs::create_dir_all(&blobs).map_err(|_| HttpError(RepositoryError::Backend))?;
    let blob = blobs.join(&sha256);
    if !blob.is_file() {
        std::fs::write(&blob, &body).map_err(|_| HttpError(RepositoryError::Backend))?;
    }
    std::fs::write(directory.join(format!("{file_id}.{extension}")), &body)
        .map_err(|_| HttpError(RepositoryError::Backend))?;
    let metadata = serde_json::json!({
        "file_id": file_id, "session_id": session, "filename": filename,
        "media_type": media_type, "size": body.len(), "sha256": sha256, "staged": true
    });
    std::fs::write(
        directory.join(format!("{file_id}.json")),
        serde_json::to_vec(&metadata).map_err(|_| HttpError(RepositoryError::Backend))?,
    )
    .map_err(|_| HttpError(RepositoryError::Backend))?;
    Ok(Json(UploadedAttachment {
        file_id,
        session_id: session,
        filename,
        media_type: media_type.into(),
        size: body.len(),
        sha256,
        staged: true,
    }))
}

async fn upload_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<UploadedFile>, HttpError> {
    let filename = upload_filename(&headers);
    let (media_type, extension) = match filename.rsplit_once('.').map(|(_, ext)| ext.to_lowercase())
    {
        Some(ext)
            if matches!(
                ext.as_str(),
                "docx" | "xlsx" | "pptx" | "pdf" | "png" | "jpg" | "jpeg"
            ) =>
        {
            (
                match ext.as_str() {
                    "docx" => {
                        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                    }
                    "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                    "pptx" => {
                        "application/vnd.openxmlformats-officedocument.presentationml.presentation"
                    }
                    "png" => "image/png",
                    "jpg" | "jpeg" => "image/jpeg",
                    _ => "application/pdf",
                },
                ext,
            )
        }
        _ => {
            return Err(HttpError(RepositoryError::InvalidRequest(
                "仅支持 docx/xlsx/pptx/pdf",
            )));
        }
    };
    if body.is_empty() || body.len() > 8 * 1024 * 1024 {
        return Err(HttpError(RepositoryError::InvalidRequest(
            "文件为空或超过 8 MiB",
        )));
    }
    std::fs::create_dir_all(&state.uploads).map_err(|_| HttpError(RepositoryError::Backend))?;
    let id = uuid::Uuid::new_v4().to_string();
    let path = state.uploads.join(format!("{id}.{extension}"));
    std::fs::write(&path, &body).map_err(|_| HttpError(RepositoryError::Backend))?;
    Ok(Json(UploadedFile {
        id,
        filename,
        media_type: media_type.into(),
        size: body.len(),
    }))
}

async fn stream_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, HttpError> {
    let store = state.store.clone();
    let check_id = id.clone();
    database(move || store.get(&check_id)).await?;
    let mut last = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    let store = state.store.clone();
    let stream = async_stream::stream! {
        loop {
            let store = store.clone();
            let task_id = id.clone();
            let result = tokio::task::spawn_blocking(move || {
                let events = store.events(&task_id)?;
                let task = store.get(&task_id)?;
                Ok::<_, RepositoryError>((events, task))
            })
            .await;
            match result {
                Ok(Ok((events, task))) => {
                    let fresh: Vec<_> = events
                        .into_iter()
                        .filter(|event| event.sequence > last)
                        .collect();
                    for event in fresh {
                        last = event.sequence;
                        let data = serde_json::to_string(&event).unwrap_or_else(|_| "{}".into());
                        yield Ok(Event::default()
                            .id(event.sequence.to_string())
                            .event("task")
                            .data(data));
                    }
                    if is_terminal_task(task.status) {
                        yield Ok(Event::default().event("done").data("{}"));
                        break;
                    }
                }
                _ => {
                    yield Ok(Event::default().event("error").data("{\"message\":\"任务事件读取失败\"}"));
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

fn is_terminal_task(status: yukii_contracts::TaskStatus) -> bool {
    matches!(
        status,
        yukii_contracts::TaskStatus::Completed
            | yukii_contracts::TaskStatus::Failed
            | yukii_contracts::TaskStatus::Cancelled
    )
}

async fn cancel_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Task>, HttpError> {
    let store = state.store.clone();
    let task_id = id.clone();
    let task = database(move || store.cancel(&task_id)).await?;
    state.executor.cancel(&id);
    Ok(Json(task))
}

async fn run_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Task>), HttpError> {
    Ok((
        StatusCode::ACCEPTED,
        Json(state.executor.start(id).await.map_err(HttpError)?),
    ))
}
async fn task_tool_calls(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<serde_json::Value>>, HttpError> {
    if uuid::Uuid::parse_str(&id).is_err() {
        return Err(HttpError(RepositoryError::InvalidRequest(
            "task id 必须是 UUID",
        )));
    }
    Ok(Json(
        database(move || state.store.task_tool_calls(&id)).await?,
    ))
}

async fn task_attachments(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<serde_json::Value>>, HttpError> {
    if uuid::Uuid::parse_str(&id).is_err() {
        return Err(HttpError(RepositoryError::InvalidRequest(
            "task id 必须是 UUID",
        )));
    }
    Ok(Json(
        database(move || state.store.task_attachments(&id)).await?,
    ))
}

async fn task_artifacts(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<Artifact>>, HttpError> {
    Ok(Json(database(move || state.store.artifacts(&id)).await?))
}
async fn download_artifact(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, HttpError> {
    // UUID 校验与固定文件名避免 Content-Disposition 注入；正文作为附件而非 HTML 执行。
    let uuid = uuid::Uuid::parse_str(&id).map_err(|_| HttpError(RepositoryError::NotFound))?;
    let artifact_id = uuid.to_string();
    let store = state.store.clone();
    let artifact = database(move || store.artifact(&artifact_id)).await?;
    let file_id = uuid.to_string();
    let store = state.store.clone();
    let file = database(move || Ok::<_, RepositoryError>(store.artifact_file(&file_id))).await?;
    let bytes = if let Some(path) = file {
        std::fs::read(path).map_err(|_| HttpError(RepositoryError::Backend))?
    } else {
        let content_id = uuid.to_string();
        let content = database(move || state.store.artifact_content(&content_id)).await?;
        if artifact.encoding == "base64" {
            STANDARD
                .decode(content.as_bytes())
                .map_err(|_| HttpError(RepositoryError::Backend))?
        } else {
            content.into_bytes()
        }
    };
    let filename = artifact
        .filename
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty() && !name.contains('"'))
        .unwrap_or("document.bin");
    let content_type = if artifact.encoding == "utf8" && artifact.media_type.starts_with("text/") {
        format!("{}; charset=utf-8", artifact.media_type)
    } else {
        artifact.media_type.clone()
    };
    Response::builder()
        .header("content-type", content_type)
        .header("content-disposition", content_disposition(filename))
        .header("x-content-type-options", "nosniff")
        .body(Body::from(bytes))
        .map_err(|_| HttpError(RepositoryError::Backend))
}

struct HttpError(RepositoryError);
impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self.0 {
            RepositoryError::Embedding(_) => (
                StatusCode::BAD_GATEWAY,
                "embedding_error",
                self.0.to_string(),
            ),
            RepositoryError::InvalidRequest(message) => (
                StatusCode::BAD_REQUEST,
                "invalid_request",
                message.to_string(),
            ),
            RepositoryError::ConversationBusy => (
                StatusCode::CONFLICT,
                "conversation_busy",
                self.0.to_string(),
            ),
            RepositoryError::IdempotencyConflict => (
                StatusCode::CONFLICT,
                "idempotency_conflict",
                self.0.to_string(),
            ),
            RepositoryError::QueueFull => (
                StatusCode::TOO_MANY_REQUESTS,
                "queue_full",
                self.0.to_string(),
            ),
            RepositoryError::ShuttingDown => (
                StatusCode::SERVICE_UNAVAILABLE,
                "shutting_down",
                self.0.to_string(),
            ),
            RepositoryError::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "model_not_configured",
                self.0.to_string(),
            ),
            RepositoryError::Validation(yukii_agent_core::TaskError::InvalidTransition) => (
                StatusCode::CONFLICT,
                "invalid_transition",
                self.0.to_string(),
            ),
            RepositoryError::Validation(_) => {
                (StatusCode::BAD_REQUEST, "invalid_task", self.0.to_string())
            }
            RepositoryError::NotFound => (StatusCode::NOT_FOUND, "not_found", self.0.to_string()),
            _ => {
                tracing::error!(error = %self.0, "存储操作失败");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "storage_error",
                    "服务暂时不可用，请稍后重试".into(),
                )
            }
        };
        (
            status,
            Json(ApiError {
                code: code.into(),
                message,
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    #[tokio::test]
    async fn api_rejects_local_execution_and_unknown_tasks() {
        let app = router(Arc::new(TaskStore::open(":memory:").unwrap()));
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/tasks")
                    .header("host", "localhost:8765")
                    .header("idempotency-key", "reject-local")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"prompt":"打开电脑","target":"local_computer"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = app
            .oneshot(
                Request::get("/api/tasks/missing/events")
                    .header("host", "localhost:8765")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
