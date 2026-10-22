//! 最小 MCP stdio client：JSON-RPC 2.0 + initialize + tools/list + tools/call。
//!
//! 约束：
//! - server 命令只来自本地配置文件，不接受模型参数；
//! - 子进程清空环境变量，只注入配置中的 env 和 PATH；
//! - 所有请求有超时，stdout/stderr 有上限；
//! - 工具名统一命名空间 `mcp__<server>__<tool>`，避免与内置工具冲突。
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};
use url::Url;
use yukii_agent_ports::{
    model::{ImageContent, ToolDefinition},
    repository::RepositoryError,
    tools::{ExecutionTarget, Tool, ToolCatalog, ToolContext, ToolEffect},
};

const PROTOCOL_VERSION: &str = "2024-11-05";
const MAX_LINE_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOOLS: usize = 64;
const MAX_PAGES: usize = 16;

#[derive(Debug, Error)]
pub enum McpError {
    #[error("MCP 配置无效：{0}")]
    Config(String),
    #[error("无法启动 MCP server：{0}")]
    Startup(String),
    #[error("MCP 进程 I/O 失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("MCP 协议 JSON 无效：{0}")]
    Json(#[from] serde_json::Error),
    #[error("MCP 协议错误：{0}")]
    Protocol(String),
    #[error("MCP server 返回错误：{0}")]
    Rpc(String),
    #[error("MCP 请求超时")]
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpTransportKind {
    Stdio,
    StreamableHttp,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    pub name: String,
    #[serde(default)]
    pub transport: Option<McpTransportKind>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// 为空表示该 server 的全部工具；非空时只暴露列出的原始 MCP 工具名。
    #[serde(default)]
    pub enabled_tools: Vec<String>,
    #[serde(default)]
    pub target: Option<ExecutionTarget>,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
}

fn default_timeout_seconds() -> u64 {
    30
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

impl McpConfig {
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, McpError> {
        let bytes = std::fs::read(path.as_ref()).map_err(|error| {
            McpError::Config(format!("读取 {} 失败：{error}", path.as_ref().display()))
        })?;
        let config: Self = serde_json::from_slice(&bytes)?;
        if config.servers.len() > 16 {
            return Err(McpError::Config("最多允许 16 个 MCP server".into()));
        }
        for server in &config.servers {
            server.validate()?;
        }
        Ok(config)
    }
}

impl McpServerConfig {
    pub fn transport_kind(&self) -> McpTransportKind {
        self.transport.unwrap_or_else(|| {
            if self.url.is_some() {
                McpTransportKind::StreamableHttp
            } else {
                McpTransportKind::Stdio
            }
        })
    }

    fn validate(&self) -> Result<(), McpError> {
        if !is_identifier(&self.name) {
            return Err(McpError::Config(format!(
                "server 名称 {} 必须是小写字母开头的 1–24 位字母/数字/下划线",
                self.name
            )));
        }
        if !(1..=120).contains(&self.timeout_seconds) {
            return Err(McpError::Config("timeout_seconds 必须在 1–120 之间".into()));
        }
        match self.transport_kind() {
            McpTransportKind::Stdio => {
                if self.url.is_some() || !self.headers.is_empty() {
                    return Err(McpError::Config(
                        "stdio server 不能同时配置 url/headers".into(),
                    ));
                }
                let command = self.command.as_deref().unwrap_or("");
                if command.trim().is_empty() || command.len() > 4096 {
                    return Err(McpError::Config(
                        "command 不能为空且不能超过 4096 字节".into(),
                    ));
                }
                if self.args.len() > 64 || self.args.iter().any(|arg| arg.len() > 16_000) {
                    return Err(McpError::Config(
                        "args 最多 64 项，每项最多 16000 字节".into(),
                    ));
                }
                validate_env(&self.env)?;
            }
            McpTransportKind::StreamableHttp => {
                if self.command.is_some()
                    || !self.args.is_empty()
                    || !self.env.is_empty()
                    || self.cwd.is_some()
                {
                    return Err(McpError::Config(
                        "streamable-http server 不能配置 command/args/env/cwd".into(),
                    ));
                }
                let raw = self
                    .url
                    .as_deref()
                    .ok_or_else(|| McpError::Config("streamable-http 需要 url".into()))?;
                let url = Url::parse(raw).map_err(|_| McpError::Config("MCP url 无效".into()))?;
                if !matches!(url.scheme(), "http" | "https")
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.fragment().is_some()
                {
                    return Err(McpError::Config(
                        "MCP url 必须是 http(s)、无认证信息、无 fragment".into(),
                    ));
                }
                if url.scheme() == "http"
                    && !matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
                {
                    return Err(McpError::Config("非本机 MCP HTTP 必须使用 https".into()));
                }
                if self.headers.len() > 16
                    || self.headers.iter().any(|(key, value)| {
                        key.is_empty()
                            || key.contains('\n')
                            || key.eq_ignore_ascii_case("mcp-session-id")
                            || value.contains('\n')
                            || value.len() > 16_000
                    })
                {
                    return Err(McpError::Config(
                        "headers 最多 16 项，不能包含 Mcp-Session-Id 或换行".into(),
                    ));
                }
            }
        }
        Ok(())
    }
}

fn validate_env(env: &BTreeMap<String, String>) -> Result<(), McpError> {
    if env.len() > 32
        || env
            .iter()
            .any(|(key, value)| key.is_empty() || key.contains('=') || value.len() > 16_000)
    {
        return Err(McpError::Config(
            "env 最多 32 项，名称不能为空且 value 不能超过 16000 字节".into(),
        ));
    }
    Ok(())
}

fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 24
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        && !value.contains("__")
}

#[async_trait]
trait McpTransport: Send {
    async fn send(&mut self, line: &str) -> Result<(), McpError>;
    async fn recv(&mut self) -> Result<Option<String>, McpError>;
}

struct StdioTransport {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl StdioTransport {
    fn new(config: &McpServerConfig) -> Result<Self, McpError> {
        let command = config.command.as_deref().unwrap_or_default();
        let mut process = Command::new(command);
        process
            .args(&config.args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(path) = std::env::var_os("PATH") {
            process.env("PATH", path);
        }
        for (key, value) in &config.env {
            process.env(key, value);
        }
        if let Some(cwd) = &config.cwd {
            process.current_dir(cwd);
        }
        let mut child = process
            .spawn()
            .map_err(|error| McpError::Startup(format!("{command}: {error}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Startup("stdin 不可用".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Startup("stdout 不可用".into()))?;
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::warn!(target: "yukii_mcp", "MCP stderr: {}", truncate(&line, 500));
                }
            });
        }
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn send(&mut self, line: &str) -> Result<(), McpError> {
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<String>, McpError> {
        let mut line = String::new();
        let read = self.stdout.read_line(&mut line).await?;
        if read == 0 {
            return Ok(None);
        }
        if line.len() > MAX_LINE_BYTES {
            return Err(McpError::Protocol("MCP 响应行超过 2 MiB".into()));
        }
        Ok(Some(line))
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

/// MCP Streamable HTTP transport（基础版）。
/// 只处理 POST 响应（JSON 或 SSE）和响应中的 Mcp-Session-Id；
/// 暂不实现 GET 服务端推送、重连和 DELETE 会话终止。
struct HttpTransport {
    client: reqwest::Client,
    url: Url,
    headers: BTreeMap<String, String>,
    session: Option<String>,
    queue: VecDeque<String>,
}

impl HttpTransport {
    fn new(config: &McpServerConfig) -> Result<Self, McpError> {
        let raw = config
            .url
            .as_deref()
            .ok_or_else(|| McpError::Config("streamable-http 需要 url".into()))?;
        let url = Url::parse(raw).map_err(|_| McpError::Config("MCP url 无效".into()))?;
        for (key, value) in &config.headers {
            reqwest::header::HeaderName::from_bytes(key.as_bytes())
                .map_err(|_| McpError::Config(format!("无效的 MCP header 名称：{key}")))?;
            reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| McpError::Config(format!("无效的 MCP header 值：{key}")))?;
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .build()
            .map_err(|error| McpError::Startup(format!("HTTP client 初始化失败：{error}")))?;
        Ok(Self {
            client,
            url,
            headers: config.headers.clone(),
            session: None,
            queue: VecDeque::new(),
        })
    }
}

#[async_trait]
impl McpTransport for HttpTransport {
    async fn send(&mut self, line: &str) -> Result<(), McpError> {
        let mut request = self
            .client
            .post(self.url.clone())
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header("Mcp-Protocol-Version", PROTOCOL_VERSION)
            .body(line.to_owned());
        for (key, value) in &self.headers {
            request = request.header(key.as_str(), value.as_str());
        }
        if let Some(session) = &self.session {
            request = request.header("Mcp-Session-Id", session.as_str());
        }
        let response = request
            .send()
            .await
            .map_err(|error| McpError::Startup(format!("MCP HTTP 请求失败：{error}")))?;
        if let Some(session) = response
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
        {
            self.session = Some(session.to_owned());
        }
        let status = response.status();
        if status == reqwest::StatusCode::ACCEPTED {
            return Ok(());
        }
        if let Some(length) = response.content_length()
            && length as usize > MAX_LINE_BYTES
        {
            return Err(McpError::Protocol("MCP HTTP 响应超过 2 MiB".into()));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let body = response
            .text()
            .await
            .map_err(|error| McpError::Startup(format!("读取 MCP HTTP 响应失败：{error}")))?;
        if body.len() > MAX_LINE_BYTES {
            return Err(McpError::Protocol("MCP HTTP 响应超过 2 MiB".into()));
        }
        if !status.is_success() {
            return Err(McpError::Rpc(format!(
                "MCP HTTP {status}: {}",
                truncate(&body, 300)
            )));
        }
        if body.trim().is_empty() {
            return Ok(());
        }
        let messages = if content_type.contains("text/event-stream") {
            parse_sse(&body)?
        } else if body.trim_start().starts_with('[') {
            serde_json::from_str::<Vec<Value>>(&body)?
                .into_iter()
                .map(|value| value.to_string())
                .collect()
        } else {
            vec![body]
        };
        for message in messages {
            if message.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(message.trim())?;
            self.queue.push_back(value.to_string());
        }
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<String>, McpError> {
        Ok(self.queue.pop_front())
    }
}

fn parse_sse(body: &str) -> Result<Vec<String>, McpError> {
    let mut messages = Vec::new();
    let mut data = Vec::new();
    for line in body.lines() {
        if line.trim().is_empty() {
            if !data.is_empty() {
                let value = data.join("\n");
                // 每个 data 块必须是合法 JSON；注释/空事件忽略。
                let _: Value = serde_json::from_str(value.trim())
                    .map_err(|_| McpError::Protocol("MCP SSE data 不是合法 JSON".into()))?;
                messages.push(value);
                data.clear();
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            data.push(rest.strip_prefix(' ').unwrap_or(rest).to_owned());
        }
    }
    if !data.is_empty() {
        let value = data.join("\n");
        let _: Value = serde_json::from_str(value.trim())
            .map_err(|_| McpError::Protocol("MCP SSE data 不是合法 JSON".into()))?;
        messages.push(value);
    }
    Ok(messages)
}

#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

pub struct McpClient {
    server_name: String,
    transport: Mutex<Box<dyn McpTransport>>,
    next_id: AtomicU64,
    timeout: Duration,
}

impl McpClient {
    pub async fn connect(config: &McpServerConfig) -> Result<Self, McpError> {
        config.validate()?;
        let timeout = Duration::from_secs(config.timeout_seconds);
        let transport: Box<dyn McpTransport> = match config.transport_kind() {
            McpTransportKind::Stdio => Box::new(StdioTransport::new(config)?),
            McpTransportKind::StreamableHttp => Box::new(HttpTransport::new(config)?),
        };
        let client = Self {
            server_name: config.name.clone(),
            transport: Mutex::new(transport),
            next_id: AtomicU64::new(1),
            timeout,
        };
        client.initialize().await?;
        Ok(client)
    }

    #[cfg(test)]
    fn with_transport(
        server_name: &str,
        transport: Box<dyn McpTransport>,
        timeout: Duration,
    ) -> Self {
        Self {
            server_name: server_name.into(),
            transport: Mutex::new(transport),
            next_id: AtomicU64::new(1),
            timeout,
        }
    }

    async fn initialize(&self) -> Result<(), McpError> {
        let result = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "yukii", "version": env!("CARGO_PKG_VERSION")}
                }),
            )
            .await?;
        if !result.is_object() {
            return Err(McpError::Protocol("initialize 结果不是对象".into()));
        }
        self.notify("notifications/initialized", json!({})).await
    }

    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        let mut cursor: Option<String> = None;
        let mut tools = Vec::new();
        let mut seen = BTreeSet::new();
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(cursor) => json!({"cursor": cursor}),
                None => json!({}),
            };
            let result = self.request("tools/list", params).await?;
            let page = result
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| McpError::Protocol("tools/list 缺少 tools 数组".into()))?;
            for tool in page {
                let info = parse_tool(tool)?;
                if !seen.insert(info.name.clone()) {
                    return Err(McpError::Protocol(format!("MCP 工具重名：{}", info.name)));
                }
                tools.push(info);
                if tools.len() > MAX_TOOLS {
                    return Err(McpError::Protocol("单个 MCP server 工具数超过 64".into()));
                }
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            if cursor.is_none() {
                return Ok(tools);
            }
        }
        Err(McpError::Protocol("tools/list 分页超过 16 页".into()))
    }

    async fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolEffect, McpError> {
        let result = self
            .request("tools/call", json!({"name": name, "arguments": arguments}))
            .await?;
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut text = Vec::new();
        let mut images = Vec::new();
        if let Some(content) = result.get("content").and_then(Value::as_array) {
            for item in content {
                match item.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(value) = item.get("text").and_then(Value::as_str) {
                            text.push(value.to_owned());
                        }
                    }
                    Some("image") => {
                        let media_type = item
                            .get("mimeType")
                            .and_then(Value::as_str)
                            .unwrap_or("image/png");
                        let data = item
                            .get("data")
                            .and_then(Value::as_str)
                            .ok_or_else(|| McpError::Protocol("image 内容缺少 data".into()))?;
                        if media_type == "image/png" || media_type == "image/jpeg" {
                            images.push(ImageContent {
                                media_type: media_type.into(),
                                base64: data.into(),
                                width: 0,
                                height: 0,
                            });
                        } else {
                            text.push(format!("[unsupported image {media_type}]"));
                        }
                    }
                    Some("resource") => {
                        if let Some(resource) = item.get("resource") {
                            if let Some(value) = resource.get("text").and_then(Value::as_str) {
                                text.push(value.to_owned());
                            } else if let Some(blob) = resource.get("blob").and_then(Value::as_str)
                            {
                                text.push(format!("[resource blob: {} bytes]", blob.len()));
                            }
                        }
                    }
                    Some(other) => text.push(format!("[unsupported content: {other}]")),
                    None => {}
                }
            }
        }
        if let Some(structured) = result.get("structuredContent") {
            text.push(serde_json::to_string_pretty(structured)?);
        }
        let combined = text.join("\n");
        if is_error {
            return Err(McpError::Rpc(if combined.is_empty() {
                "工具返回错误".into()
            } else {
                combined
            }));
        }
        if images.is_empty() {
            Ok(ToolEffect::Result(Value::String(combined)))
        } else {
            Ok(ToolEffect::ResultWithImages {
                value: Value::String(combined),
                images,
            })
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let payload = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let mut transport = self.transport.lock().await;
        transport.send(&payload.to_string()).await
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let payload = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let exchange = async {
            let mut transport = self.transport.lock().await;
            transport.send(&payload.to_string()).await?;
            loop {
                let line = transport.recv().await?.ok_or_else(|| {
                    McpError::Protocol(format!("{} 关闭了 stdout", self.server_name))
                })?;
                let value: Value = serde_json::from_str(line.trim())?;
                if value.get("id").and_then(Value::as_u64) != Some(id) {
                    continue;
                }
                if let Some(error) = value.get("error") {
                    return Err(McpError::Rpc(format_error(error)));
                }
                return Ok(value.get("result").cloned().unwrap_or(Value::Null));
            }
        };
        tokio::time::timeout(self.timeout, exchange)
            .await
            .map_err(|_| McpError::Timeout)?
    }
}

fn parse_tool(value: &Value) -> Result<McpToolInfo, McpError> {
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty() && name.len() <= 128)
        .ok_or_else(|| McpError::Protocol("工具 name 缺失或过长".into()))?;
    let description = value
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("MCP 工具");
    let input_schema = value
        .get("inputSchema")
        .cloned()
        .unwrap_or_else(|| json!({"type": "object"}));
    if input_schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err(McpError::Protocol(format!(
            "工具 {name} 的 inputSchema 不是 object"
        )));
    }
    Ok(McpToolInfo {
        name: name.into(),
        description: truncate(description, 1000),
        input_schema,
    })
}

fn format_error(value: &Value) -> String {
    let code = value.get("code").and_then(Value::as_i64).unwrap_or(0);
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("未知错误");
    format!("{code}: {message}")
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn namespaced(server: &str, tool: &str) -> String {
    format!("mcp__{}__{}", sanitize(server), sanitize(tool))
}

fn sanitize(value: &str) -> String {
    let mut output = String::new();
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            output.push(character.to_ascii_lowercase());
        } else if !output.ends_with('_') {
            output.push('_');
        }
    }
    let trimmed = output.trim_matches('_');
    if trimmed.is_empty() {
        "tool".into()
    } else {
        trimmed.chars().take(40).collect()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct McpToolView {
    /// 暴露给模型的 namespaced 名称，如 mcp__text_tools__text_stats。
    pub name: String,
    pub server: String,
    pub remote_name: String,
    pub description: String,
    pub target: ExecutionTarget,
    pub enabled: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct McpState {
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    disabled: Vec<String>,
}

struct McpEntry {
    view: McpToolView,
    tool: std::sync::Arc<dyn Tool>,
}

struct McpTool {
    client: std::sync::Arc<McpClient>,
    info: McpToolInfo,
    exposed_name: String,
    target: ExecutionTarget,
}

#[async_trait]
impl Tool for McpTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.exposed_name.clone(),
            description: format!("MCP 工具：{}", self.info.description),
            schema: self.info.input_schema.clone(),
        }
    }

    fn execution_target(&self) -> ExecutionTarget {
        self.target
    }

    async fn execute(
        &self,
        context: &ToolContext,
        mut arguments: Value,
    ) -> Result<ToolEffect, String> {
        // 只有 CUA begin_app 需要审批上下文；不能污染其他 MCP server 的严格 schema。
        if self.info.name == "cua_begin_app"
            && let Some(object) = arguments.as_object_mut()
        {
            object.insert(
                "__yukii_context".into(),
                json!({"task_id": context.task_id, "call_id": context.call_id}),
            );
        }
        self.client
            .call_tool(&self.info.name, arguments)
            .await
            .map_err(|error| error.to_string())
    }
}

fn select_tools(tools: Vec<McpToolInfo>, enabled: &[String]) -> Result<Vec<McpToolInfo>, McpError> {
    if enabled.is_empty() {
        return Ok(tools);
    }
    let known: BTreeSet<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
    let missing: Vec<&str> = enabled
        .iter()
        .map(String::as_str)
        .filter(|name| !known.contains(name))
        .collect();
    if !missing.is_empty() {
        return Err(McpError::Config(format!(
            "enabled_tools 引用了不存在的 MCP 工具：{}",
            missing.join(", ")
        )));
    }
    let allowed: BTreeSet<&str> = enabled.iter().map(String::as_str).collect();
    Ok(tools
        .into_iter()
        .filter(|tool| allowed.contains(tool.name.as_str()))
        .collect())
}

pub struct McpToolCatalog {
    entries: Vec<McpEntry>,
    disabled: tokio::sync::RwLock<BTreeSet<String>>,
    state_path: Option<PathBuf>,
}

impl McpToolCatalog {
    pub async fn connect(servers: Vec<McpServerConfig>) -> Result<Self, McpError> {
        Self::connect_with_state(servers, None).await
    }

    pub async fn connect_with_state(
        servers: Vec<McpServerConfig>,
        state_path: Option<PathBuf>,
    ) -> Result<Self, McpError> {
        let disabled = match &state_path {
            Some(path) => load_state(path)?.disabled.into_iter().collect(),
            None => BTreeSet::new(),
        };
        let mut entries = Vec::new();
        let mut names = BTreeSet::new();
        for server in servers {
            let client = std::sync::Arc::new(McpClient::connect(&server).await?);
            let target = server.target.unwrap_or(ExecutionTarget::CloudOffice);
            let selected = select_tools(client.list_tools().await?, &server.enabled_tools)?;
            for info in selected {
                let exposed_name = namespaced(&server.name, &info.name);
                if !names.insert(exposed_name.clone()) {
                    return Err(McpError::Config(format!("MCP 工具名冲突：{exposed_name}")));
                }
                entries.push(McpEntry {
                    view: McpToolView {
                        name: exposed_name.clone(),
                        server: server.name.clone(),
                        remote_name: info.name.clone(),
                        description: info.description.clone(),
                        target,
                        enabled: !disabled.contains(&exposed_name),
                    },
                    tool: std::sync::Arc::new(McpTool {
                        client: client.clone(),
                        info,
                        exposed_name,
                        target,
                    }),
                });
            }
        }
        if entries.len() > MAX_TOOLS {
            return Err(McpError::Config("MCP 工具总数超过 64".into()));
        }
        Ok(Self {
            entries,
            disabled: tokio::sync::RwLock::new(disabled),
            state_path,
        })
    }

    pub async fn inventory(&self) -> Vec<McpToolView> {
        let disabled = self.disabled.read().await;
        self.entries
            .iter()
            .map(|entry| McpToolView {
                enabled: !disabled.contains(&entry.view.name),
                ..entry.view.clone()
            })
            .collect()
    }

    pub async fn set_enabled(&self, name: &str, enabled: bool) -> Result<McpToolView, McpError> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.view.name == name)
            .ok_or_else(|| McpError::Config(format!("MCP 工具不存在：{name}")))?;
        let mut disabled = self.disabled.write().await;
        let mut next = disabled.clone();
        if enabled {
            next.remove(name);
        } else {
            next.insert(name.to_owned());
        }
        if let Some(path) = &self.state_path {
            save_state(path, &next)?;
        }
        *disabled = next;
        Ok(McpToolView {
            enabled,
            ..entry.view.clone()
        })
    }
}

fn load_state(path: &std::path::Path) -> Result<McpState, McpError> {
    if !path.exists() {
        return Ok(McpState::default());
    }
    let bytes = std::fs::read(path)?;
    let state: McpState = serde_json::from_slice(&bytes)?;
    if state.schema_version > 1 {
        return Err(McpError::Config("MCP 状态文件版本不支持".into()));
    }
    Ok(state)
}

fn save_state(path: &std::path::Path, disabled: &BTreeSet<String>) -> Result<(), McpError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let state = McpState {
        schema_version: 1,
        disabled: disabled.iter().cloned().collect(),
    };
    let bytes = serde_json::to_vec_pretty(&state)?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

#[async_trait]
impl ToolCatalog for McpToolCatalog {
    async fn tools(&self) -> Result<Vec<std::sync::Arc<dyn Tool>>, RepositoryError> {
        let disabled = self.disabled.read().await;
        Ok(self
            .entries
            .iter()
            .filter(|entry| !disabled.contains(&entry.view.name))
            .map(|entry| entry.tool.clone())
            .collect())
    }
}

#[cfg(test)]
impl McpClient {
    async fn initialize_for_test(&self) -> Result<(), McpError> {
        self.initialize().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct FakeTransport {
        responses: VecDeque<String>,
        sent: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    }

    #[async_trait]
    impl McpTransport for FakeTransport {
        async fn send(&mut self, line: &str) -> Result<(), McpError> {
            self.sent
                .lock()
                .unwrap()
                .push(serde_json::from_str(line).unwrap());
            Ok(())
        }

        async fn recv(&mut self) -> Result<Option<String>, McpError> {
            Ok(self.responses.pop_front())
        }
    }

    fn fake_client(
        responses: Vec<Value>,
    ) -> (McpClient, std::sync::Arc<std::sync::Mutex<Vec<Value>>>) {
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let transport = FakeTransport {
            responses: responses
                .into_iter()
                .map(|value| value.to_string())
                .collect(),
            sent: sent.clone(),
        };
        (
            McpClient::with_transport("demo", Box::new(transport), Duration::from_secs(1)),
            sent,
        )
    }

    #[tokio::test]
    async fn lists_and_calls_tools_with_namespacing_and_text_result() {
        let (client, sent) = fake_client(vec![
            json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"demo","version":"1"}}}),
            json!({"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"text_stats","description":"stats","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}]}}),
            json!({"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"{\"characters\":3}"}]}}),
        ]);
        client.initialize_for_test().await.unwrap();
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "text_stats");
        let effect = client
            .call_tool("text_stats", json!({"text":"abc"}))
            .await
            .unwrap();
        match effect {
            ToolEffect::Result(Value::String(text)) => assert_eq!(text, "{\"characters\":3}"),
            _ => panic!("unexpected effect"),
        }
        let messages = sent.lock().unwrap();
        assert_eq!(messages[0]["method"], "initialize");
        assert_eq!(messages[1]["method"], "notifications/initialized");
        assert_eq!(messages[2]["method"], "tools/list");
        assert_eq!(messages[3]["method"], "tools/call");
    }

    fn tool_info(name: &str) -> McpToolInfo {
        McpToolInfo {
            name: name.into(),
            description: name.into(),
            input_schema: json!({"type": "object"}),
        }
    }

    #[test]
    fn enabled_tools_filters_and_fails_closed_on_unknown_names() {
        let tools = vec![tool_info("a"), tool_info("b")];
        let selected = select_tools(tools.clone(), &["b".into()]).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "b");
        assert!(select_tools(tools.clone(), &[]).unwrap().len() == 2);
        let error = select_tools(tools, &["missing".into()]).unwrap_err();
        assert!(error.to_string().contains("missing"));
    }

    #[test]
    fn parses_sse_data_blocks() {
        let messages =
            parse_sse("event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n")
                .unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("\"id\":1"));
        assert!(parse_sse("data: not-json\n\n").is_err());
    }

    #[test]
    fn state_round_trips_and_rejects_newer_schema() {
        let path = std::env::temp_dir().join(format!(
            "yukii-mcp-state-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let disabled = ["mcp__demo__a".to_owned()].into_iter().collect();
        save_state(&path, &disabled).unwrap();
        let loaded = load_state(&path).unwrap();
        assert_eq!(loaded.disabled, vec!["mcp__demo__a"]);
        std::fs::write(&path, "{\"schema_version\":2}").unwrap();
        assert!(load_state(&path).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn namespaced_names_are_stable() {
        assert_eq!(
            namespaced("text_tools", "text-stats"),
            "mcp__text_tools__text_stats"
        );
        assert_eq!(sanitize("  Hello World! "), "hello_world");
    }
}
