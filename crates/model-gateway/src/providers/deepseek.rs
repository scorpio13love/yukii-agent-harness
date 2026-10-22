//! DeepSeek / OpenAI-compatible Chat Completions 增量解析。
use crate::provider::ModelDelta;
use serde_json::Value;
use yukii_agent_ports::model::ModelStreamEvent;

/// 映射到 Agent Runtime 的流式事件；Done 由调用方处理。
pub fn parse_stream_chunk(value: &Value) -> Vec<ModelStreamEvent> {
    parse_chunk(value)
        .into_iter()
        .filter_map(|delta| match delta {
            ModelDelta::Text(text) => Some(ModelStreamEvent::Text(text)),
            ModelDelta::Reasoning(text) => Some(ModelStreamEvent::Reasoning(text)),
            ModelDelta::ToolCall {
                index,
                id,
                name,
                arguments,
            } => Some(ModelStreamEvent::ToolCall {
                index,
                id,
                name,
                arguments,
            }),
            ModelDelta::Done => None,
        })
        .collect()
}

/// 解析一个流式 chunk；不把 reasoning 混入正文。
pub fn parse_chunk(value: &Value) -> Vec<ModelDelta> {
    let mut deltas = Vec::new();
    let Some(choice) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    else {
        return deltas;
    };
    let delta = &choice["delta"];
    if let Some(text) = delta.get("content").and_then(Value::as_str)
        && !text.is_empty()
    {
        deltas.push(ModelDelta::Text(text.to_owned()));
    }
    if let Some(reasoning) = delta.get("reasoning_content").and_then(Value::as_str)
        && !reasoning.is_empty()
    {
        deltas.push(ModelDelta::Reasoning(reasoning.to_owned()));
    }
    if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
        for (index, call) in calls.iter().enumerate() {
            let index = call
                .get("index")
                .and_then(Value::as_u64)
                .map(|value| value as usize)
                .unwrap_or(index);
            let function = &call["function"];
            deltas.push(ModelDelta::ToolCall {
                index,
                id: call.get("id").and_then(Value::as_str).map(str::to_owned),
                name: function
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                arguments: function
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
            });
        }
    }
    if choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .is_some()
    {
        deltas.push(ModelDelta::Done);
    }
    deltas
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keeps_reasoning_separate_and_accumulates_tool_arguments() {
        let deltas = parse_chunk(&json!({
            "choices": [{
                "delta": {
                    "content": "答",
                    "reasoning_content": "先想",
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "function": {"name": "text_stats", "arguments": "{\"text\":"}
                    }]
                },
                "finish_reason": null
            }]
        }));
        assert!(deltas.contains(&ModelDelta::Text("答".into())));
        assert!(deltas.contains(&ModelDelta::Reasoning("先想".into())));
        assert!(deltas.iter().any(|delta| matches!(delta, ModelDelta::ToolCall { name: Some(name), arguments, .. } if name == "text_stats" && arguments == "{\"text\":")));
    }
}
