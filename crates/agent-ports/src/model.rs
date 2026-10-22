use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub schema: Value,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageContent {
    /// 仅允许原生桥产生的 image/png 或 image/jpeg；网关按此生成 data URL / image block。
    pub media_type: String,
    pub base64: String,
    pub width: u32,
    pub height: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub content: String,
    pub is_error: bool,
    /// 截图等模型可查看的内容；旧数据库记录缺失时按空数组读取。
    #[serde(default)]
    pub images: Vec<ImageContent>,
}
#[derive(Debug, Clone)]
pub enum Message {
    User(String),
    UserWithImages {
        text: String,
        images: Vec<ImageContent>,
    },
    Assistant {
        text: String,
        calls: Vec<ToolCall>,
    },
    ToolResults(Vec<ToolResult>),
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
}
#[derive(Debug)]
pub struct ModelReply {
    pub text: String,
    pub calls: Vec<ToolCall>,
    pub usage: Usage,
}

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("模型请求超时，请调整请求超时或稍后重试")]
    Timeout,
    #[error("无法连接模型服务，请检查 API 地址和网络")]
    Transport,
    #[error("模型服务返回 HTTP {0}；请检查密钥、模型名、配额和协议格式")]
    Http(u16),
    #[error("模型上下文窗口已满，需要压缩历史后重试")]
    ContextOverflow,
    #[error("模型响应协议不兼容或内容不完整")]
    Protocol,
    #[error("模型输出达到上限，未将截断内容标记为完成")]
    Truncated,
    #[error("模型拒绝了该请求")]
    Refusal,
}

/// 流式增量；reasoning 与正文分离，工具参数按 index 累积。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelStreamEvent {
    Text(String),
    Reasoning(String),
    ToolCall {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
    /// 模型请求已失败并准备重试；由 Host 记录，不进入模型上下文。
    Retry {
        attempt: u32,
        error: String,
    },
}

#[async_trait]
pub trait DeltaSink: Send {
    async fn emit(&mut self, event: ModelStreamEvent) -> Result<(), ModelError>;
}

#[async_trait]
pub trait LanguageModel: Send + Sync {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<ModelReply, ModelError>;

    /// 可选精确计数；provider 不支持时返回 None，由调用方明确降级到估算。
    async fn count_tokens(
        &self,
        _system: &str,
        _messages: &[Message],
        _tools: &[ToolDefinition],
    ) -> Result<Option<usize>, ModelError> {
        Ok(None)
    }

    /// 默认退化为一次性结果；支持 SSE 的 provider 覆盖此方法。
    async fn stream(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        sink: &mut dyn DeltaSink,
    ) -> Result<ModelReply, ModelError> {
        let reply = self.complete(system, messages, tools).await?;
        if !reply.text.is_empty() {
            sink.emit(ModelStreamEvent::Text(reply.text.clone()))
                .await?;
        }
        Ok(reply)
    }
}
