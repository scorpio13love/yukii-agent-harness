//! Session 工作区：未绑定项目时使用按日期归档的默认目录。
use chrono::Local;
use serde_json::Value;
use std::{
    env,
    path::{Path, PathBuf},
};
use yukii_agent_ports::repository::{RepositoryError, TaskRepository};
use yukii_contracts::ExecutionTarget;

/// 默认会话目录根：优先 YUKII_WORKSPACE_ROOT，否则 ~/Yukii/chats。
pub fn default_chat_root() -> PathBuf {
    if let Ok(root) = env::var("YUKII_WORKSPACE_ROOT") {
        let root = root.trim();
        if !root.is_empty() {
            return PathBuf::from(root);
        }
    }
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Yukii")
        .join("chats")
}

/// 默认会话目录：<root>/<YYYY-MM-DD>/<session_id>。
pub fn default_workspace_values(
    root: &Path,
    session_id: &str,
    title: &str,
) -> (PathBuf, String, String) {
    let date = Local::now().format("%Y-%m-%d").to_string();
    let path = root.join(date).join(session_id);
    let raw_name = title.trim();
    let name = if raw_name.is_empty() {
        "新对话".to_owned()
    } else {
        raw_name.chars().take(80).collect()
    };
    (path.clone(), path.to_string_lossy().to_string(), name)
}

/// 确保会话有可用工作区；已有项目绑定时不覆盖。
/// force=true 用于“解除项目绑定”，恢复到默认会话目录。
pub fn ensure_default_workspace(
    store: &dyn TaskRepository,
    root: &Path,
    session_id: &str,
    title: &str,
    force: bool,
) -> Result<Value, RepositoryError> {
    if !force && let Some(existing) = store.session_workspace(session_id)? {
        let kind = existing["kind"].as_str().unwrap_or("session");
        let path = existing["path"].as_str().unwrap_or_default();
        if kind != "session" || Path::new(path).is_dir() {
            return Ok(existing);
        }
    }
    let (path, path_text, name) = default_workspace_values(root, session_id, title);
    std::fs::create_dir_all(&path).map_err(|_| RepositoryError::Backend)?;
    store.set_session_workspace(session_id, &path_text, &name, "session")
}

/// 启动时给还没有工作区的既有会话补默认目录；单个失败不阻塞服务启动。
pub fn backfill_default_workspaces(
    store: &dyn TaskRepository,
    root: &Path,
) -> Result<(), RepositoryError> {
    for task in store.conversations()? {
        if task.target != ExecutionTarget::CloudOffice {
            continue;
        }
        if let Err(error) = ensure_default_workspace(
            store,
            root,
            &task.conversation_id,
            &task.conversation_title,
            false,
        ) {
            tracing::warn!(session_id=%task.conversation_id, %error, "默认工作区创建失败");
        }
    }
    Ok(())
}
