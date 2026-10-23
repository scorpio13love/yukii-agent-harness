//! 双端共享的任务协议。能力字段表示当前实际可用能力，不表示规划目标。
use serde::{Deserialize, Serialize};

// v2 将创建请求的持久化 Idempotency-Key 设为必填，旧客户端应明确拒绝不兼容协议。
pub const PROTOCOL_VERSION: u32 = 2;
pub const CHROMIUM_MAJOR: u32 = 152;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionTarget {
    CloudOffice,
    LocalComputer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Planning,
    Running,
    WaitingForUser,
    Verifying,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTask {
    /// 省略时创建工作 Session；续聊必须传入已有 Session 标识。
    #[serde(rename = "session_id")]
    pub conversation_id: Option<String>,
    pub prompt: String,
    pub target: ExecutionTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// 执行记录属于长期 Session；id 仍标识本轮执行。旧 conversation_* 字段仅作读兼容。
    #[serde(rename = "session_id", alias = "conversation_id")]
    pub conversation_id: String,
    #[serde(rename = "session_title", alias = "conversation_title")]
    pub conversation_title: String,
    pub id: String,
    pub prompt: String,
    pub target: ExecutionTarget,
    pub status: TaskStatus,
    pub created_at: i64,
    /// 用户通过斜杠命令创建本轮时记录的命令；命令本身不进入模型 prompt。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub context_tokens: u64,
    #[serde(default)]
    pub context_window_tokens: u64,
    #[serde(default)]
    pub sources: Vec<knowledge::KnowledgeHit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEvent {
    pub sequence: i64,
    pub task_id: String,
    pub status: TaskStatus,
    pub message: String,
    /// 流式增量：text / reasoning / tool_call；普通状态事件为 None。
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub delta: Option<String>,
    pub created_at: i64,
    /// 毫秒时间；旧事件缺失时为 0，读取方回退到 created_at 秒。
    #[serde(default)]
    pub created_at_ms: i64,
}

#[derive(Debug, Serialize)]
pub struct Capabilities {
    pub protocol_version: u32,
    pub chromium_major: u32,
    pub task_creation: bool,
    pub agent_execution: bool,
    pub local_computer: bool,
    pub cloud_computer: bool,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

/// 产物正文单独下载；列表不复制长正文，避免放大轮询响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub id: String,
    pub task_id: String,
    pub title: String,
    pub media_type: String,
    /// 下载文件名；旧记录缺省为 document.md。
    #[serde(default = "default_artifact_filename")]
    pub filename: String,
    /// utf8 或 base64；旧 Markdown 记录缺省为 utf8。
    #[serde(default = "default_artifact_encoding")]
    pub encoding: String,
    pub created_at: i64,
}

fn default_artifact_filename() -> String {
    "document.md".into()
}

fn default_artifact_encoding() -> String {
    "utf8".into()
}

pub mod desktop;
pub mod extensions;
pub mod invocation;
pub mod knowledge;
pub mod permissions;
