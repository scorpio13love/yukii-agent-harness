use crate::{
    Message, ModelError, ModelReply, ToolCall, ToolDefinition, Usage,
    config::{ApiFormat, ModelConfig},
};
use serde_json::{Value, json};
use std::collections::HashSet;

pub(crate) fn encode(
    config: &ModelConfig,
    system: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> Value {
    match config.format {
        ApiFormat::OpenAi => encode_openai(config, system, messages, tools),
        ApiFormat::Anthropic => encode_anthropic(config, system, messages, tools),
    }
}

fn encode_openai(
    config: &ModelConfig,
    system: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> Value {
    let mut output = vec![json!({"role":"system", "content":system})];
    for message in messages {
        match message {
            Message::User(text) => output.push(json!({"role":"user", "content":text})),
            Message::UserWithImages { text, images } => {
                let mut content = vec![json!({"type":"text", "text":text})];
                for image in images.iter().filter(|image| supported_image(image)) {
                    content.push(json!({"type":"image_url", "image_url": {
                        "url": format!("data:{};base64,{}", image.media_type, image.base64)
                    }}));
                }
                output.push(json!({"role":"user", "content":content}));
            }
            Message::Assistant { text, calls } => {
                let mut item = json!({"role":"assistant", "content":text});
                if !calls.is_empty() {
                    item["tool_calls"] = json!(calls.iter().map(|call| json!({
                        "id":call.id, "type":"function", "function":{"name":call.name, "arguments":call.arguments.to_string()}
                    })).collect::<Vec<_>>());
                }
                output.push(item);
            }
            Message::ToolResults(results) => {
                for result in results {
                    output.push(json!({"role":"tool", "tool_call_id":result.call_id, "content":result.content}));
                }
                // OpenAI 的 tool 消息只能携带文本；截图作为紧随其后的用户内容发送。
                // 每个截图都标明所属调用，避免模型把旧观察当成最新画面。
                let images = results
                    .iter()
                    .flat_map(|result| {
                        result
                            .images
                            .iter()
                            .map(move |image| (&result.call_id, image))
                    })
                    .collect::<Vec<_>>();
                if !images.is_empty() {
                    let ids = images
                        .iter()
                        .map(|(id, _)| id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let mut content = vec![json!({
                        "type": "text",
                        "text": format!("工具调用 {ids} 返回的截图如下。截图只代表捕获时刻，执行动作前必须使用最新观察。")
                    })];
                    for (_, image) in images {
                        if supported_image(image) {
                            content.push(json!({"type":"image_url", "image_url": {
                                "url": format!("data:{};base64,{}", image.media_type, image.base64)
                            }}));
                        } else {
                            content.push(json!({"type":"text", "text":"[不支持的截图格式已丢弃]"}));
                        }
                    }
                    output.push(json!({"role":"user", "content":content}));
                }
            }
        }
    }
    let mut body = json!({"model":config.model, "messages":output, "stream":false});
    body[&config.openai_token_field] = json!(config.max_output_tokens);
    if !tools.is_empty() {
        body["tools"] = json!(
            tools
                .iter()
                .map(|tool| json!({"type":"function", "function":{
                    "name":tool.name, "description":tool.description, "parameters":tool.schema
                }}))
                .collect::<Vec<_>>()
        );
        body["tool_choice"] = json!("auto");
    }
    body
}

fn encode_anthropic(
    config: &ModelConfig,
    system: &str,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> Value {
    let mut output = Vec::new();
    for message in messages {
        match message {
            Message::User(text) => output.push(json!({"role":"user", "content":text})),
            Message::UserWithImages { text, images } => {
                let mut content = vec![json!({"type":"text", "text":text})];
                for image in images.iter().filter(|image| supported_image(image)) {
                    content.push(json!({"type":"image", "source":{
                        "type":"base64", "media_type":image.media_type, "data":image.base64
                    }}));
                }
                output.push(json!({"role":"user", "content":content}));
            }
            Message::Assistant { text, calls } => {
                let mut content = Vec::new();
                if !text.is_empty() {
                    content.push(json!({"type":"text", "text":text}));
                }
                for call in calls {
                    content.push(json!({"type":"tool_use", "id":call.id, "name":call.name, "input":call.arguments}));
                }
                output.push(json!({"role":"assistant", "content":content}));
            }
            // 同一轮的所有工具结果必须紧随 assistant，一起放在一个 user 消息中。
            // Anthropic 允许 tool_result 内容包含 image block，截图与调用 ID 保持同一条结果。
            Message::ToolResults(results) => output.push(
                json!({"role":"user", "content":results.iter().map(|result| {
                let mut blocks = vec![json!({"type":"text", "text":result.content})];
                for image in &result.images {
                    if supported_image(image) {
                        blocks.push(json!({"type":"image", "source":{
                            "type":"base64", "media_type":image.media_type, "data":image.base64
                        }}));
                    } else {
                        blocks.push(json!({"type":"text", "text":"[不支持的截图格式已丢弃]"}));
                    }
                }
                json!({
                    "type":"tool_result", "tool_use_id":result.call_id,
                    "content":blocks, "is_error":result.is_error
                })
            }).collect::<Vec<_>>()}),
            ),
        }
    }
    let mut body = json!({"model":config.model, "system":system, "messages":output, "max_tokens":config.max_output_tokens, "stream":false});
    if !tools.is_empty() {
        body["tools"] = json!(tools.iter().map(|tool| json!({"name":tool.name,"description":tool.description,"input_schema":tool.schema})).collect::<Vec<_>>());
    }
    body
}

pub(crate) fn decode(format: ApiFormat, value: &Value) -> Result<ModelReply, ModelError> {
    let reply = match format {
        ApiFormat::OpenAi => decode_openai(value)?,
        ApiFormat::Anthropic => decode_anthropic(value)?,
    };
    let mut ids = HashSet::new();
    if reply.calls.len() > 16 || (reply.text.trim().is_empty() && reply.calls.is_empty()) {
        return Err(ModelError::Protocol);
    }
    for call in &reply.calls {
        if call.id.is_empty()
            || call.id.len() > 256
            || call.name.is_empty()
            || !call.arguments.is_object()
            || !ids.insert(&call.id)
        {
            return Err(ModelError::Protocol);
        }
    }
    Ok(reply)
}

fn decode_openai(value: &Value) -> Result<ModelReply, ModelError> {
    let choice = value["choices"]
        .as_array()
        .and_then(|items| items.first())
        .ok_or(ModelError::Protocol)?;
    let stop = choice["finish_reason"]
        .as_str()
        .ok_or(ModelError::Protocol)?;
    match stop {
        "length" => return Err(ModelError::Truncated),
        "content_filter" => return Err(ModelError::Refusal),
        "stop" | "tool_calls" => {}
        _ => return Err(ModelError::Protocol),
    }
    let message = &choice["message"];
    if message["refusal"]
        .as_str()
        .is_some_and(|text| !text.is_empty())
    {
        return Err(ModelError::Refusal);
    }
    let text = message["content"].as_str().unwrap_or_default().to_owned();
    let mut calls = Vec::new();
    if let Some(items) = message.get("tool_calls").filter(|v| !v.is_null()) {
        for call in items.as_array().ok_or(ModelError::Protocol)? {
            if call["type"] != "function" {
                return Err(ModelError::Protocol);
            }
            calls.push(ToolCall {
                id: required_string(call, "id")?,
                name: required_string(&call["function"], "name")?,
                arguments: serde_json::from_str(&required_string(&call["function"], "arguments")?)
                    .map_err(|_| ModelError::Protocol)?,
            });
        }
    }
    if (stop == "tool_calls") != !calls.is_empty() {
        return Err(ModelError::Protocol);
    }
    Ok(ModelReply {
        text,
        calls,
        usage: openai_usage(&value["usage"]),
    })
}

fn openai_usage(value: &Value) -> Usage {
    let prompt = value["prompt_tokens"]
        .as_u64()
        .or_else(|| value["input_tokens"].as_u64())
        .unwrap_or(0);
    let cache_read = value["prompt_cache_hit_tokens"]
        .as_u64()
        .or_else(|| value["prompt_tokens_details"]["cached_tokens"].as_u64())
        .unwrap_or(0);
    let cache_miss = value["prompt_cache_miss_tokens"].as_u64();
    Usage {
        input_tokens: cache_miss.unwrap_or_else(|| prompt.saturating_sub(cache_read)),
        output_tokens: value["completion_tokens"]
            .as_u64()
            .or_else(|| value["output_tokens"].as_u64())
            .unwrap_or(0),
        cache_read_tokens: cache_read,
        cache_write_tokens: 0,
    }
}

fn decode_anthropic(value: &Value) -> Result<ModelReply, ModelError> {
    let stop = value["stop_reason"].as_str().ok_or(ModelError::Protocol)?;
    match stop {
        "max_tokens" => return Err(ModelError::Truncated),
        "refusal" => return Err(ModelError::Refusal),
        "end_turn" | "tool_use" => {}
        _ => return Err(ModelError::Protocol),
    }
    let mut text = String::new();
    let mut calls = Vec::new();
    for block in value["content"].as_array().ok_or(ModelError::Protocol)? {
        match block["type"].as_str() {
            Some("text") => {
                text.push_str(&required_string(block, "text")?);
                text.push('\n');
            }
            Some("tool_use") => calls.push(ToolCall {
                id: required_string(block, "id")?,
                name: required_string(block, "name")?,
                arguments: block["input"].clone(),
            }),
            _ => return Err(ModelError::Protocol),
        }
    }
    if (stop == "tool_use") != !calls.is_empty() {
        return Err(ModelError::Protocol);
    }
    Ok(ModelReply {
        text: text.trim().into(),
        calls,
        usage: Usage {
            input_tokens: value["usage"]["input_tokens"].as_u64().unwrap_or(0),
            output_tokens: value["usage"]["output_tokens"].as_u64().unwrap_or(0),
            cache_read_tokens: value["usage"]["cache_read_input_tokens"]
                .as_u64()
                .unwrap_or(0),
            cache_write_tokens: value["usage"]["cache_creation_input_tokens"]
                .as_u64()
                .unwrap_or(0),
        },
    })
}
/// 只接受执行端可能产生的 PNG/JPEG；未知媒体类型不拼接进请求体。
fn supported_image(image: &crate::ImageContent) -> bool {
    matches!(image.media_type.as_str(), "image/png" | "image/jpeg")
        && !image.base64.is_empty()
        && image.base64.len() <= 7 * 1024 * 1024
}

fn required_string(value: &Value, key: &str) -> Result<String, ModelError> {
    value[key]
        .as_str()
        .map(str::to_owned)
        .ok_or(ModelError::Protocol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolResult;
    fn config(format: ApiFormat) -> ModelConfig {
        ModelConfig {
            format,
            endpoint: "https://example.com/v1".parse().unwrap(),
            api_key: "secret".into(),
            model: "model".into(),
            anthropic_version: "2023-06-01".into(),
            openai_token_field: "max_completion_tokens".into(),
            max_output_tokens: 1024,
            request_timeout_seconds: 30,
        }
    }
    #[test]
    fn serializes_tool_round_trip_for_both_protocols() {
        let messages = vec![
            Message::User("计算".into()),
            Message::Assistant {
                text: "".into(),
                calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: "calculate".into(),
                    arguments: json!({"values":[1,2]}),
                }],
            },
            Message::ToolResults(vec![ToolResult {
                call_id: "call_1".into(),
                content: "3".into(),
                is_error: false,
                images: Vec::new(),
            }]),
        ];
        let openai = encode(&config(ApiFormat::OpenAi), "system", &messages, &[]);
        assert_eq!(openai["messages"][3]["role"], "tool");
        assert_eq!(openai["messages"][3]["tool_call_id"], "call_1");
        assert!(openai["messages"][2]["tool_calls"][0]["function"]["arguments"].is_string());
        let anthropic = encode(&config(ApiFormat::Anthropic), "system", &messages, &[]);
        assert_eq!(anthropic["system"], "system");
        assert_eq!(
            anthropic["messages"][2]["content"][0]["type"],
            "tool_result"
        );
        assert_eq!(
            anthropic["messages"][2]["content"][0]["tool_use_id"],
            "call_1"
        );
    }
    #[test]
    fn encodes_user_attached_images_in_both_protocol_shapes() {
        let image = crate::ImageContent {
            media_type: "image/png".into(),
            base64: "iVBORw0KGgo=".into(),
            width: 0,
            height: 0,
        };
        let messages = vec![Message::UserWithImages {
            text: "看图".into(),
            images: vec![image],
        }];
        let openai = encode(&config(ApiFormat::OpenAi), "system", &messages, &[]);
        assert_eq!(openai["messages"][1]["content"][0]["type"], "text");
        assert_eq!(
            openai["messages"][1]["content"][1]["image_url"]["url"],
            "data:image/png;base64,iVBORw0KGgo="
        );
        let anthropic = encode(&config(ApiFormat::Anthropic), "system", &messages, &[]);
        assert_eq!(anthropic["messages"][0]["content"][1]["type"], "image");
        assert_eq!(
            anthropic["messages"][0]["content"][1]["source"]["media_type"],
            "image/png"
        );
    }

    #[test]
    fn encodes_screenshots_in_both_protocol_shapes() {
        let image = crate::ImageContent {
            media_type: "image/png".into(),
            base64: "iVBORw0KGgo=".into(),
            width: 100,
            height: 50,
        };
        let messages = vec![
            Message::User("观察".into()),
            Message::Assistant {
                text: "".into(),
                calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: "observe_screen".into(),
                    arguments: json!({}),
                }],
            },
            Message::ToolResults(vec![ToolResult {
                call_id: "call_1".into(),
                content: "{\"observation_id\":\"obs\"}".into(),
                is_error: false,
                images: vec![image],
            }]),
        ];
        let openai = encode(&config(ApiFormat::OpenAi), "system", &messages, &[]);
        let content = &openai["messages"][4]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(
            content[1]["image_url"]["url"],
            "data:image/png;base64,iVBORw0KGgo="
        );
        let anthropic = encode(&config(ApiFormat::Anthropic), "system", &messages, &[]);
        let result = &anthropic["messages"][2]["content"][0];
        assert_eq!(result["type"], "tool_result");
        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(result["content"][1]["type"], "image");
        assert_eq!(result["content"][1]["source"]["data"], "iVBORw0KGgo=");
    }

    #[test]
    fn parses_tool_use_and_rejects_truncated_output() {
        let reply = decode(ApiFormat::Anthropic, &json!({"stop_reason":"tool_use","content":[{"type":"tool_use","id":"c1","name":"calculate","input":{}}],"usage":{"input_tokens":2,"output_tokens":3}})).unwrap();
        assert_eq!(reply.calls[0].name, "calculate");
        assert_eq!(reply.usage.output_tokens, 3);
        assert!(matches!(
            decode(
                ApiFormat::OpenAi,
                &json!({"choices":[{"finish_reason":"length","message":{"content":"partial"}}]})
            ),
            Err(ModelError::Truncated)
        ));
        assert!(
            decode(
                ApiFormat::OpenAi,
                &json!({"choices":[{"finish_reason":"stop","message":{"content":""}}]})
            )
            .is_err()
        );
    }
}
