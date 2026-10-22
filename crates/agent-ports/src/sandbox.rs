use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub struct SandboxRequest {
    pub image: String,
    pub command: Vec<String>,
    pub input: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxStatus {
    pub configured: bool,
    pub available: bool,
    pub backend: String,
    pub message: String,
}

#[async_trait]
pub trait Sandbox: Send + Sync {
    async fn status(&self) -> SandboxStatus;
    /// 实现必须在 Future 被取消时清理外部进程，不能只结束等待。
    async fn execute(&self, request: SandboxRequest) -> Result<Value, String>;
    async fn shutdown(&self);
}
