//! 工作区基础能力：文件读写受 Session workspace 约束；bash/python 在无网络 Docker 容器内执行。
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::process::Command;
use yukii_agent_ports::{
    model::ToolDefinition,
    repository::{RepositoryError, TaskRepository},
    tools::{ExecutionTarget, Tool, ToolCatalog, ToolContext, ToolEffect},
};

const MAX_FILE: usize = 2 * 1024 * 1024;
const MAX_OUTPUT: usize = 64 * 1024;
const DOCKER_IMAGE: &str = "python:3.13-alpine";

pub struct DevTools {
    store: Arc<dyn TaskRepository>,
}

impl DevTools {
    pub fn new(store: Arc<dyn TaskRepository>) -> Self {
        Self { store }
    }

    fn workspace(&self, task_id: &str) -> Result<PathBuf, String> {
        let task = self.store.get(task_id).map_err(|_| "任务不存在")?;
        let workspace = self
            .store
            .session_workspace(&task.conversation_id)
            .map_err(|_| "无法读取工作区")?
            .ok_or("请先在左侧为当前 Session 绑定工作区")?;
        let path = workspace["path"].as_str().ok_or("工作区路径无效")?;
        let root = std::fs::canonicalize(path).map_err(|_| "工作区路径不可用")?;
        if !root.is_dir() {
            return Err("工作区不是目录".into());
        }
        Ok(root)
    }
}

#[derive(Deserialize)]
struct PathInput {
    path: String,
}

#[derive(Deserialize)]
struct WriteInput {
    path: String,
    content: String,
}

#[derive(Deserialize)]
struct EditInput {
    path: String,
    old: String,
    new: String,
    #[serde(default)]
    replace_all: bool,
}

#[derive(Deserialize)]
struct CommandInput {
    command: String,
}

#[derive(Deserialize)]
struct PythonInput {
    code: String,
}

fn resolve(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.contains('\0') {
        return Err("路径为空或包含 NUL".into());
    }
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        return Err("只允许工作区相对路径".into());
    }
    for component in candidate.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err("路径不能跳出工作区".into());
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    Ok(root.join(candidate))
}

fn existing_file(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let path = resolve(root, raw)?;
    let canonical = std::fs::canonicalize(&path).map_err(|_| "文件不存在")?;
    if !canonical.starts_with(root) {
        return Err("路径不能跳出工作区".into());
    }
    if !canonical.is_file() {
        return Err("目标不是文件".into());
    }
    Ok(canonical)
}

fn writable_file(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let path = resolve(root, raw)?;
    let parent = path.parent().ok_or("路径没有父目录")?;
    std::fs::create_dir_all(parent).map_err(|_| "无法创建目录")?;
    let parent = std::fs::canonicalize(parent).map_err(|_| "父目录不可用")?;
    if !parent.starts_with(root) {
        return Err("路径不能跳出工作区".into());
    }
    let name = path.file_name().ok_or("路径没有文件名")?;
    Ok(parent.join(name))
}

async fn docker_run(root: &Path, command: &str) -> Result<Value, String> {
    if command.trim().is_empty() || command.chars().count() > 8000 {
        return Err("命令需要 1–8000 字符".into());
    }
    let uid = unsafe { libc::geteuid() };
    let gid = unsafe { libc::getegid() };
    let child = Command::new("docker")
        .args([
            "run",
            "--rm",
            "--network=none",
            "--read-only",
            "--tmpfs",
            "/tmp:rw,noexec,nosuid,size=67108864",
            "--memory=1g",
            "--cpus=1",
            "--pids-limit=256",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges:true",
            "--user",
            &format!("{uid}:{gid}"),
            "-e",
            "HOME=/tmp",
            "-e",
            "PYTHONDONTWRITEBYTECODE=1",
            "-v",
            &format!("{}:/workspace", root.display()),
            "-w",
            "/workspace",
            DOCKER_IMAGE,
            "/bin/sh",
            "-lc",
            command,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "无法启动 Docker；请确认 Docker Desktop 正在运行")?;
    let output = match tokio::time::timeout(Duration::from_secs(60), child.wait_with_output()).await
    {
        Ok(Ok(output)) => output,
        Ok(Err(_)) => return Err("命令执行失败".into()),
        Err(_) => return Err("命令超过 60 秒，已终止".into()),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut combined = format!("{stdout}\n{stderr}");
    if combined.len() > MAX_OUTPUT {
        combined.truncate(MAX_OUTPUT);
        combined.push_str("\n[输出已截断]");
    }
    Ok(json!({
        "exit_code": output.status.code(),
        "output": combined.trim(),
        "sandbox": "docker",
        "network": "none"
    }))
}

struct DevTool {
    store: Arc<dyn TaskRepository>,
    kind: Kind,
}

#[derive(Clone, Copy)]
enum Kind {
    Read,
    Write,
    Edit,
    List,
    Bash,
    Python,
}

#[async_trait]
impl ToolCatalog for DevTools {
    async fn tools(&self) -> Result<Vec<Arc<dyn Tool>>, RepositoryError> {
        Ok([
            Kind::Read,
            Kind::Write,
            Kind::Edit,
            Kind::List,
            Kind::Bash,
            Kind::Python,
        ]
        .into_iter()
        .map(|kind| {
            Arc::new(DevTool {
                store: self.store.clone(),
                kind,
            }) as Arc<dyn Tool>
        })
        .collect())
    }
}

#[async_trait]
impl Tool for DevTool {
    fn definition(&self) -> ToolDefinition {
        let (name, description, schema) = match self.kind {
            Kind::Read => (
                "read_file",
                "读取当前 Session 工作区内的 UTF-8 文件；路径必须是相对路径。",
                json!({"type":"object","properties":{"path":{"type":"string","maxLength":1000}},"required":["path"],"additionalProperties":false}),
            ),
            Kind::Write => (
                "write_file",
                "写入当前 Session 工作区内的 UTF-8 文件（覆盖）；路径必须是相对路径。",
                json!({"type":"object","properties":{"path":{"type":"string","maxLength":1000},"content":{"type":"string","maxLength":2000000}},"required":["path","content"],"additionalProperties":false}),
            ),
            Kind::Edit => (
                "edit_file",
                "在工作区文件内替换一段文本；old 必须唯一，除非 replace_all=true。",
                json!({"type":"object","properties":{"path":{"type":"string","maxLength":1000},"old":{"type":"string"},"new":{"type":"string"},"replace_all":{"type":"boolean"}},"required":["path","old","new"],"additionalProperties":false}),
            ),
            Kind::List => (
                "list_dir",
                "列出当前 Session 工作区内的目录；默认列出工作区根目录。",
                json!({"type":"object","properties":{"path":{"type":"string","maxLength":1000}},"additionalProperties":false}),
            ),
            Kind::Bash => (
                "bash",
                "在无网络 Docker 容器中执行 shell 命令，工作目录为绑定工作区；不能访问宿主其他路径，60 秒超时。",
                json!({"type":"object","properties":{"command":{"type":"string","maxLength":8000}},"required":["command"],"additionalProperties":false}),
            ),
            Kind::Python => (
                "python",
                "在无网络 Docker 容器中运行一段 Python 代码，工作目录为绑定工作区；不能访问宿主其他路径，60 秒超时。",
                json!({"type":"object","properties":{"code":{"type":"string","maxLength":200000}},"required":["code"],"additionalProperties":false}),
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
        let root = {
            let store = self.store.clone();
            let task_id = context.task_id.clone();
            tokio::task::spawn_blocking(move || DevTools { store }.workspace(&task_id))
                .await
                .map_err(|_| "工作区读取失败")??
        };
        match self.kind {
            Kind::Read => {
                let input: PathInput =
                    serde_json::from_value(arguments).map_err(|_| "路径参数无效")?;
                let path = existing_file(&root, &input.path)?;
                let bytes = std::fs::read(&path).map_err(|_| "读取文件失败")?;
                if bytes.len() > MAX_FILE {
                    return Err("文件超过 2MB".into());
                }
                let text = String::from_utf8(bytes).map_err(|_| "文件不是 UTF-8 文本")?;
                Ok(ToolEffect::Result(
                    json!({"path": input.path, "content": text}),
                ))
            }
            Kind::Write => {
                let input: WriteInput =
                    serde_json::from_value(arguments).map_err(|_| "写入参数无效")?;
                if input.content.len() > MAX_FILE {
                    return Err("内容超过 2MB".into());
                }
                let path = writable_file(&root, &input.path)?;
                std::fs::write(&path, input.content.as_bytes()).map_err(|_| "写入文件失败")?;
                Ok(ToolEffect::Result(
                    json!({"path": input.path, "bytes": input.content.len()}),
                ))
            }
            Kind::Edit => {
                let input: EditInput =
                    serde_json::from_value(arguments).map_err(|_| "编辑参数无效")?;
                let path = existing_file(&root, &input.path)?;
                let text = std::fs::read_to_string(&path).map_err(|_| "读取文件失败")?;
                let count = text.matches(&input.old).count();
                if input.old.is_empty() || count == 0 {
                    return Err("old 文本不存在".into());
                }
                if !input.replace_all && count > 1 {
                    return Err(format!("old 文本出现 {count} 次，请提供更精确的片段"));
                }
                let updated = if input.replace_all {
                    text.replace(&input.old, &input.new)
                } else {
                    text.replacen(&input.old, &input.new, 1)
                };
                std::fs::write(&path, updated.as_bytes()).map_err(|_| "写入文件失败")?;
                Ok(ToolEffect::Result(
                    json!({"path": input.path, "replacements": if input.replace_all { count } else { 1 }}),
                ))
            }
            Kind::List => {
                let input: PathInput =
                    serde_json::from_value(arguments).unwrap_or(PathInput { path: ".".into() });
                let path = if input.path.trim().is_empty() {
                    root.clone()
                } else {
                    resolve(&root, &input.path)?
                };
                let canonical = std::fs::canonicalize(&path).map_err(|_| "目录不存在")?;
                if !canonical.starts_with(&root) || !canonical.is_dir() {
                    return Err("不是工作区目录".into());
                }
                let mut entries = Vec::new();
                for entry in std::fs::read_dir(&canonical)
                    .map_err(|_| "无法读取目录")?
                    .flatten()
                {
                    let metadata = entry.metadata().ok();
                    entries.push(json!({
                        "name": entry.file_name().to_string_lossy(),
                        "dir": metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                        "size": metadata.map(|m| m.len()).unwrap_or(0)
                    }));
                }
                entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
                Ok(ToolEffect::Result(
                    json!({"path": input.path, "entries": entries}),
                ))
            }
            Kind::Bash => {
                let input: CommandInput =
                    serde_json::from_value(arguments).map_err(|_| "命令参数无效")?;
                Ok(ToolEffect::Result(docker_run(&root, &input.command).await?))
            }
            Kind::Python => {
                let input: PythonInput =
                    serde_json::from_value(arguments).map_err(|_| "Python 参数无效")?;
                if input.code.len() > 200_000 {
                    return Err("Python 代码超过 200KB".into());
                }
                let temp_dir = root.join(".yukii/tmp");
                std::fs::create_dir_all(&temp_dir).map_err(|_| "无法创建临时目录")?;
                let script = temp_dir.join(format!("run-{}.py", uuid::Uuid::new_v4()));
                std::fs::write(&script, input.code.as_bytes()).map_err(|_| "无法写入脚本")?;
                let relative = script
                    .strip_prefix(&root)
                    .map_err(|_| "脚本路径无效")?
                    .to_string_lossy()
                    .to_string();
                let result = docker_run(&root, &format!("python {relative}")).await;
                let _ = std::fs::remove_file(&script);
                Ok(ToolEffect::Result(result?))
            }
        }
    }
}
