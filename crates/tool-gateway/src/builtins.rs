//! 内置工具仅执行确定性计算、计划更新和任务内 Markdown 产物生成，不访问宿主文件。
use serde::Deserialize;
use serde_json::{Value, json};
use yukii_agent_ports::model::ToolDefinition;

pub use yukii_agent_ports::tools::ToolEffect;

pub fn definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition { name:"calculate".into(), description:"对有限数字列表进行求和、减法、乘法、除法或平均值计算。subtract/divide 从第一个数开始顺序计算。".into(), schema:json!({"type":"object","properties":{"operation":{"type":"string","enum":["sum","subtract","multiply","divide","average"]},"values":{"type":"array","items":{"type":"number"},"minItems":1,"maxItems":1000}},"required":["operation","values"],"additionalProperties":false}) },
        ToolDefinition { name:"update_plan".into(), description:"登记当前任务的简短可执行计划；仅说明工作步骤，不记录隐藏推理。".into(), schema:json!({"type":"object","properties":{"steps":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":12}},"required":["steps"],"additionalProperties":false}) },
        ToolDefinition { name:"spawn_subagents".into(), description:"并行执行最多 4 个受控子 Agent；继承父任务工具目录与权限，禁止递归，返回聚合结果。".into(), schema:json!({"type":"object","properties":{"prompts":{"type":"array","items":{"type":"string","maxLength":8000},"minItems":1,"maxItems":4}},"required":["prompts"],"additionalProperties":false}) },
        ToolDefinition { name:"spawn_subagent".into(), description:"把独立子目标交给受控子 Agent；继承父任务的工具目录与权限，禁止递归，结果返回父调用。".into(), schema:json!({"type":"object","properties":{"prompt":{"type":"string","maxLength":8000}},"required":["prompt"],"additionalProperties":false}) },
        ToolDefinition { name:"send_message".into(), description:"向一个可继续子 Agent 发送消息；会启动或恢复该子 Agent 的子 Session，并返回其响应。".into(), schema:json!({"type":"object","properties":{"agent_id":{"type":"string","maxLength":128},"message":{"type":"string","maxLength":8000}},"required":["agent_id","message"],"additionalProperties":false}) },
        ToolDefinition { name:"list_agents".into(), description:"列出当前父任务启动的子 Agent（含 child session id 与状态）。".into(), schema:json!({"type":"object","properties":{},"additionalProperties":false}) },
        ToolDefinition { name:"interrupt_agent".into(), description:"停止一个正在运行的子 Agent。".into(), schema:json!({"type":"object","properties":{"agent_id":{"type":"string","maxLength":128}},"required":["agent_id"],"additionalProperties":false}) },
        ToolDefinition { name:"invoke_tool".into(), description:"在受控工具目录内执行一个子工具调用；子调用继承父调用的目标与权限，仅用于需要组合工具的受信流程。".into(), schema:json!({"type":"object","properties":{"name":{"type":"string"},"arguments":{"type":"object"}},"required":["name","arguments"],"additionalProperties":false}) },
        ToolDefinition { name:"create_markdown".into(), description:"生成一份可下载的 Markdown 文档；只能生成当前任务的产物，不是 Word/PDF，不写入用户电脑任意路径。".into(), schema:json!({"type":"object","properties":{"title":{"type":"string"},"content":{"type":"string"}},"required":["title","content"],"additionalProperties":false}) },
    ]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Calculation {
    operation: String,
    values: Vec<f64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    steps: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    title: String,
    content: String,
}

/// 参数再次校验，不能假设模型遵守了 JSON Schema。
pub fn execute(name: &str, arguments: Value) -> Result<ToolEffect, String> {
    if arguments.to_string().len() > 200_000 {
        return Err("工具参数过大".into());
    }
    match name {
        "calculate" => {
            calculate(serde_json::from_value(arguments).map_err(|_| "计算参数格式不正确")?)
        }
        "update_plan" => {
            let plan: Plan = serde_json::from_value(arguments).map_err(|_| "计划参数格式不正确")?;
            if plan.steps.is_empty()
                || plan.steps.len() > 12
                || plan
                    .steps
                    .iter()
                    .any(|s| s.trim().is_empty() || s.chars().count() > 300)
            {
                return Err("计划需要 1～12 个非空步骤，每步最多 300 字".into());
            }
            Ok(ToolEffect::Plan(plan.steps))
        }
        "create_markdown" => {
            let document: Document =
                serde_json::from_value(arguments).map_err(|_| "文档参数格式不正确")?;
            if document.title.trim().is_empty()
                || document.title.chars().count() > 120
                || document.content.trim().is_empty()
                || document.content.len() > 100_000
            {
                return Err("文档标题需为 1～120 字，正文不能为空且不能超过 100KB".into());
            }
            Ok(ToolEffect::Document {
                title: document.title,
                content: document.content,
            })
        }
        _ => Err("未知工具；当前仅支持 calculate、update_plan、create_markdown".into()),
    }
}
fn calculate(input: Calculation) -> Result<ToolEffect, String> {
    let values = &input.values;
    if values.is_empty() || values.len() > 1000 || values.iter().any(|v| !v.is_finite()) {
        return Err("需要 1～1000 个有限数字".into());
    }
    let result = match input.operation.as_str() {
        "sum" => values.iter().sum(),
        "subtract" => values[1..].iter().fold(values[0], |a, b| a - b),
        "multiply" => values.iter().product(),
        "divide" => {
            if values[1..].contains(&0.0) {
                return Err("除数不能为 0".into());
            }
            values[1..].iter().fold(values[0], |a, b| a / b)
        }
        "average" => values.iter().sum::<f64>() / values.len() as f64,
        _ => return Err("不支持的计算类型".into()),
    };
    if !result.is_finite() {
        return Err("结果超出浮点数范围".into());
    }
    Ok(ToolEffect::Result(
        json!({"result":result,"precision":"IEEE 754 f64；金融精确计价需使用专用十进制工具"}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_tools_and_never_accepts_file_paths_or_shell_commands() {
        assert!(execute("bash", json!({"command":"ls"})).is_err());
        assert!(
            execute(
                "create_markdown",
                json!({"title":"x","content":"ok","path":"/tmp/file"})
            )
            .is_err()
        );
        assert!(execute("calculate", json!({"operation":"divide","values":[1,0]})).is_err());
        assert!(
            matches!(execute("calculate",json!({"operation":"sum","values":[1,2,3]})).unwrap(),ToolEffect::Result(v) if v["result"] == 6.0)
        );
    }
}
