//! Provider 层的统一增量协议；协议 JSON 解析留在 providers/ 下。
use serde_json::Value;
use yukii_agent_ports::model::{Message, ToolDefinition};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelDelta {
    Text(String),
    Reasoning(String),
    ToolCall {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
    Done,
}

#[derive(Debug, Clone)]
pub struct ProviderRequest {
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub max_output_tokens: u32,
}

#[derive(Debug, Clone)]
pub struct ProviderReply {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<Value>,
}
