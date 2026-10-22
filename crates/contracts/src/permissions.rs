//! 受信桌面宿主使用的内部审批协议，尚未暴露给 HTTP 或模型工具。
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileOperation {
    Read,
    /// 审批预览必须使用与该摘要相同的内容；Executor 还需重新计算摘要。
    Create {
        content_sha256: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileScope {
    /// 宿主目录选择器创建的引用，不是模型提供的宿主绝对路径。
    pub workspace_id: String,
    pub relative_path: String,
    pub operation: FileOperation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionBinding {
    pub session_id: String,
    pub task_id: String,
    pub call_id: String,
    pub scope: FileScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionState {
    Pending,
    Approved,
    Denied,
    Claimed,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    Approve,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRecord {
    pub id: String,
    pub binding: PermissionBinding,
    pub expires_at: i64,
    pub state: PermissionState,
}

/// 一次领取的执行凭据。不是密码学签名，不能替代受信 IPC 的对端验证。
#[derive(Debug, Clone)]
pub struct ExecutionPermit {
    pub permission_id: String,
    pub claim_id: String,
    pub binding: PermissionBinding,
}
