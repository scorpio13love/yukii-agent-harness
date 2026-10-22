//! 对话内 CUA 单次审批：MCP server 写 pending，用户决策写 decision；不持久化应用授权。
use axum::{
    Json,
    extract::{Path, State},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use yukii_agent_ports::repository::RepositoryError;

use crate::{AppState, HttpError};

fn approvals_dir() -> PathBuf {
    std::env::var("YUKII_CUA_APPROVALS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".data/cua-mcp/approvals"))
}

pub(crate) async fn list(State(state): State<AppState>) -> Result<Json<Value>, HttpError> {
    let dir = approvals_dir();
    let mut approvals = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                approvals.push(value);
            }
        }
    }
    approvals.sort_by_key(|value| value["created_at"].as_i64().unwrap_or_default());
    for approval in &approvals {
        let approval_id = approval["id"].as_str().unwrap_or_default().to_owned();
        let task_id = approval["task_id"].as_str().unwrap_or_default().to_owned();
        let app_id = approval["app_id"].as_str().unwrap_or_default().to_owned();
        let application = approval["application"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if approval_id.is_empty() || task_id.is_empty() || app_id.is_empty() {
            continue;
        }
        let store = state.store.clone();
        let _ = crate::database(move || {
            store.record_approval_asked(&approval_id, &task_id, &app_id, &application)
        })
        .await;
    }
    Ok(Json(json!({"approvals": approvals})))
}

#[derive(Deserialize)]
pub struct Decision {
    allowed: bool,
}

pub(crate) async fn decide(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<Decision>,
) -> Result<Json<Value>, HttpError> {
    if uuid::Uuid::parse_str(&id).is_err() {
        return Err(HttpError(RepositoryError::InvalidRequest("审批 id 无效")));
    }
    let dir = approvals_dir();
    let request = dir.join(format!("{id}.json"));
    if !request.is_file() {
        return Err(HttpError(RepositoryError::NotFound));
    }
    let pending: Value = serde_json::from_slice(
        &std::fs::read(&request).map_err(|_| HttpError(RepositoryError::Backend))?,
    )
    .map_err(|_| HttpError(RepositoryError::Backend))?;
    let task_id = pending["task_id"].as_str().unwrap_or_default().to_owned();
    let app_id = pending["app_id"].as_str().unwrap_or_default().to_owned();
    let application = pending["application"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let decision = dir.join(format!("{id}.decision"));
    let temp = dir.join(format!("{id}.decision.tmp"));
    let bytes = serde_json::to_vec(&json!({"allowed": input.allowed}))
        .map_err(|_| HttpError(RepositoryError::Backend))?;
    std::fs::write(&temp, bytes).map_err(|_| HttpError(RepositoryError::Backend))?;
    std::fs::rename(&temp, &decision).map_err(|_| HttpError(RepositoryError::Backend))?;
    if !task_id.is_empty() && !app_id.is_empty() {
        let allowed = input.allowed;
        let _ = crate::database(move || {
            state
                .store
                .record_approval(&task_id, &app_id, &application, allowed)
        })
        .await;
    }
    Ok(Json(json!({"id": id, "allowed": input.allowed})))
}
