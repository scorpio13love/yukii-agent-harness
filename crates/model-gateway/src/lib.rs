//! 模型协议转换层：Agent 仅依赖统一消息，不感知供应商 JSON 差异。
pub mod config;
pub mod provider;
pub mod providers;
mod wire;

use async_trait::async_trait;
use config::{ApiFormat, ModelConfig};
use futures_util::StreamExt;
use serde_json::Value;
use std::{collections::BTreeMap, time::Duration};

pub use yukii_agent_ports::model::*;

pub struct HttpModel {
    config: ModelConfig,
    client: reqwest::Client,
}
impl HttpModel {
    pub fn new(config: ModelConfig) -> Result<Self, ModelError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.request_timeout_seconds))
            .connect_timeout(Duration::from_secs(15))
            // 禁止重定向携带认证头到其他地址；请求也不做隐式重试。
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| ModelError::Transport)?;
        Ok(Self { config, client })
    }
}

#[async_trait]
impl LanguageModel for HttpModel {
    async fn stream(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
        sink: &mut dyn DeltaSink,
    ) -> Result<ModelReply, ModelError> {
        if self.config.format == ApiFormat::Anthropic {
            let reply = self.complete(system, messages, tools).await?;
            if !reply.text.is_empty() {
                sink.emit(ModelStreamEvent::Text(reply.text.clone()))
                    .await?;
            }
            return Ok(reply);
        }
        let mut body = wire::encode(&self.config, system, messages, tools);
        body["stream"] = true.into();
        body["stream_options"] = serde_json::json!({"include_usage": true});
        let retries = std::env::var("YUKII_MODEL_RETRY_ATTEMPTS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(2)
            .min(5);
        let mut attempt = 0;
        let response = loop {
            let request = self
                .client
                .post(self.config.endpoint.clone())
                .bearer_auth(&self.config.api_key)
                .json(&body);
            match request.send().await {
                Ok(response) if response.status().is_success() => break response,
                Ok(response) => {
                    let status = response.status().as_u16();
                    let body_text = response.text().await.unwrap_or_default();
                    let retryable = status == 429 || (500..600).contains(&status);
                    if retryable && attempt < retries {
                        attempt += 1;
                        sink.emit(ModelStreamEvent::Retry {
                            attempt,
                            error: format!("HTTP {status}"),
                        })
                        .await?;
                        tokio::time::sleep(Duration::from_millis(250 * u64::from(attempt))).await;
                        continue;
                    }
                    return Err(classify_http_error(status, &body_text));
                }
                Err(error) => {
                    if attempt < retries {
                        attempt += 1;
                        let mapped = map_transport(error);
                        sink.emit(ModelStreamEvent::Retry {
                            attempt,
                            error: mapped.to_string(),
                        })
                        .await?;
                        tokio::time::sleep(Duration::from_millis(250 * u64::from(attempt))).await;
                        continue;
                    }
                    return Err(ModelError::Transport);
                }
            }
        };
        // 兼容不实现 SSE 的 OpenAI-compatible 网关和测试服务器：按普通 JSON 降级。
        if !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("text/event-stream"))
        {
            let bytes = response.bytes().await.map_err(map_transport)?;
            let value: Value = serde_json::from_slice(&bytes).map_err(|_| ModelError::Protocol)?;
            let reply = wire::decode(self.config.format, &value)?;
            if !reply.text.is_empty() {
                sink.emit(ModelStreamEvent::Text(reply.text.clone()))
                    .await?;
            }
            return Ok(reply);
        }
        let mut buffer = String::new();
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut calls: BTreeMap<usize, (String, String, String)> = BTreeMap::new();
        let mut usage = Usage::default();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(map_transport)?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(index) = buffer.find('\n') {
                let line = buffer[..index].trim().to_owned();
                buffer.drain(..=index);
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                if let Some(value_usage) = value.get("usage") {
                    let prompt = value_usage["prompt_tokens"]
                        .as_u64()
                        .or_else(|| value_usage["input_tokens"].as_u64())
                        .unwrap_or(0);
                    let cache_read = value_usage["prompt_cache_hit_tokens"]
                        .as_u64()
                        .or_else(|| value_usage["cache_read_input_tokens"].as_u64())
                        .unwrap_or(0);
                    let cache_write = value_usage["cache_creation_input_tokens"]
                        .as_u64()
                        .unwrap_or(0);
                    let cache_miss = value_usage["prompt_cache_miss_tokens"].as_u64();
                    usage.input_tokens =
                        cache_miss.unwrap_or_else(|| prompt.saturating_sub(cache_read));
                    usage.cache_read_tokens = cache_read;
                    usage.cache_write_tokens = cache_write;
                    usage.output_tokens = value_usage["completion_tokens"]
                        .as_u64()
                        .or_else(|| value_usage["output_tokens"].as_u64())
                        .unwrap_or(usage.output_tokens);
                }
                for event in providers::deepseek::parse_stream_chunk(&value) {
                    match &event {
                        ModelStreamEvent::Text(value) => text.push_str(value),
                        ModelStreamEvent::Reasoning(value) => reasoning.push_str(value),
                        ModelStreamEvent::Retry { .. } => {}
                        ModelStreamEvent::ToolCall {
                            index,
                            id,
                            name,
                            arguments,
                        } => {
                            let entry = calls.entry(*index).or_default();
                            if let Some(id) = id {
                                entry.0.push_str(id);
                            }
                            if let Some(name) = name {
                                entry.1.push_str(name);
                            }
                            entry.2.push_str(arguments);
                        }
                    }
                    sink.emit(event).await?;
                }
            }
        }
        let mut reply_calls = Vec::new();
        for (_, (id, name, arguments)) in calls {
            let arguments = if arguments.trim().is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_str(&arguments).map_err(|_| ModelError::Protocol)?
            };
            reply_calls.push(ToolCall {
                id,
                name,
                arguments,
            });
        }
        if reply_calls.len() > 16 || (text.trim().is_empty() && reply_calls.is_empty()) {
            return Err(ModelError::Protocol);
        }
        let _ = reasoning;
        Ok(ModelReply {
            text,
            calls: reply_calls,
            usage,
        })
    }

    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<ModelReply, ModelError> {
        let body = wire::encode(&self.config, system, messages, tools);
        let mut request = self.client.post(self.config.endpoint.clone()).json(&body);
        request = match self.config.format {
            ApiFormat::OpenAi => request.bearer_auth(&self.config.api_key),
            ApiFormat::Anthropic => request
                .header("x-api-key", &self.config.api_key)
                .header("anthropic-version", &self.config.anthropic_version),
        };
        let mut response = request.send().await.map_err(map_transport)?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(classify_http_error(status, &body));
        }
        // 不输出原始错误响应，避免第三方网关将密钥或用户内容回显到日志。
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(map_transport)? {
            if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
                return Err(ModelError::Protocol);
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| ModelError::Protocol)?;
        wire::decode(self.config.format, &value)
    }

    async fn count_tokens(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<Option<usize>, ModelError> {
        if self.config.format == ApiFormat::OpenAi {
            let encoder = match std::env::var("YUKII_TOKENIZER_ENCODING").as_deref() {
                Ok("o200k") => tiktoken_rs::o200k_base(),
                _ => tiktoken_rs::cl100k_base(),
            }
            .map_err(|_| ModelError::Protocol)?;
            let mut tokens = encoder
                .encode_with_special_tokens(system)
                .len()
                .saturating_add(3);
            for message in messages {
                tokens = tokens.saturating_add(4);
                match message {
                    Message::User(text) => tokens += encoder.encode_with_special_tokens(text).len(),
                    Message::UserWithImages { text, images } => {
                        tokens += encoder.encode_with_special_tokens(text).len();
                        // 图片 token 由 provider 按分辨率计算；这里只计文本占位，不冒充精确。
                        tokens += images.len() * 85;
                    }
                    Message::Assistant { text, calls } => {
                        tokens += encoder.encode_with_special_tokens(text).len();
                        for call in calls {
                            tokens += encoder.encode_with_special_tokens(&call.name).len();
                            tokens += encoder
                                .encode_with_special_tokens(&call.arguments.to_string())
                                .len();
                        }
                    }
                    Message::ToolResults(results) => {
                        for result in results {
                            tokens += encoder.encode_with_special_tokens(&result.content).len();
                        }
                    }
                }
            }
            for tool in tools {
                tokens += encoder.encode_with_special_tokens(&tool.name).len();
                tokens += encoder.encode_with_special_tokens(&tool.description).len();
                tokens += encoder
                    .encode_with_special_tokens(&tool.schema.to_string())
                    .len();
            }
            return Ok(Some(tokens));
        }
        if self.config.format != ApiFormat::Anthropic {
            return Ok(None);
        }
        let mut endpoint = self.config.endpoint.clone();
        let path = endpoint.path().trim_end_matches('/');
        let base = path.strip_suffix("/messages").unwrap_or(path);
        endpoint.set_path(&format!("{base}/messages/count_tokens"));
        let body = wire::encode(&self.config, system, messages, tools);
        let response = self
            .client
            .post(endpoint)
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", &self.config.anthropic_version)
            .json(&body)
            .send()
            .await
            .map_err(map_transport)?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(classify_http_error(status, &body));
        }
        let value: Value = response.json().await.map_err(|_| ModelError::Protocol)?;
        Ok(value
            .get("input_tokens")
            .and_then(Value::as_u64)
            .map(|value| value as usize))
    }
}
fn classify_http_error(status: u16, body: &str) -> ModelError {
    let normalized = body.to_lowercase();
    if status == 400
        && (normalized.contains("context length")
            || normalized.contains("context window")
            || normalized.contains("maximum context")
            || (normalized.contains("too many tokens")))
    {
        ModelError::ContextOverflow
    } else {
        ModelError::Http(status)
    }
}

fn map_transport(error: reqwest::Error) -> ModelError {
    if error.is_timeout() {
        ModelError::Timeout
    } else {
        ModelError::Transport
    }
}
pub mod embedding;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn counts_openai_tokens_locally() {
        let model = HttpModel::new(ModelConfig {
            format: ApiFormat::OpenAi,
            endpoint: reqwest::Url::parse("http://127.0.0.1/v1/chat/completions").unwrap(),
            api_key: "test".into(),
            model: "test".into(),
            anthropic_version: "2023-06-01".into(),
            openai_token_field: "max_tokens".into(),
            max_output_tokens: 128,
            request_timeout_seconds: 1,
        })
        .unwrap();
        let count = model
            .count_tokens("system", &[Message::User("hello world".into())], &[])
            .await
            .unwrap()
            .unwrap();
        assert!(count > 0);
    }

    #[test]
    fn classifies_context_window_errors_only_for_400() {
        assert!(matches!(
            classify_http_error(400, "maximum context length exceeded"),
            ModelError::ContextOverflow
        ));
        assert!(matches!(
            classify_http_error(401, "maximum context length exceeded"),
            ModelError::Http(401)
        ));
        assert!(matches!(
            classify_http_error(400, "invalid model"),
            ModelError::Http(400)
        ));
    }
}
