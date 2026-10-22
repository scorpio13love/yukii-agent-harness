//! Computer Use 工具目录。只通过宿主注入的 DesktopControl 访问受控窗口；
//! 工具不读取原生桥路径、不选择窗口、不申请系统权限，也不能绕过控制会话。
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use yukii_agent_ports::{
    model::ToolDefinition,
    repository::RepositoryError,
    tools::{DesktopControl, ExecutionTarget, Tool, ToolCatalog, ToolContext, ToolEffect},
};
use yukii_contracts::desktop::GuiAction;

pub struct ComputerTools {
    control: Arc<dyn DesktopControl>,
}

impl ComputerTools {
    pub fn new(control: Arc<dyn DesktopControl>) -> Self {
        Self { control }
    }

    pub fn into_tools(self) -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(ObserveScreen(self.control.clone())),
            Arc::new(RecordScreen(self.control.clone())),
            Arc::new(ClickElement(self.control.clone())),
            Arc::new(TypeElement(self.control.clone())),
            Arc::new(Click(self.control.clone())),
            Arc::new(TypeText(self.control.clone())),
            Arc::new(ReplaceText(self.control.clone())),
            Arc::new(PressKey(self.control.clone())),
            Arc::new(Scroll(self.control.clone())),
        ]
    }
}

#[async_trait]
impl ToolCatalog for ComputerTools {
    async fn tools(&self) -> Result<Vec<Arc<dyn Tool>>, RepositoryError> {
        Ok(ComputerTools {
            control: self.control.clone(),
        }
        .into_tools())
    }
}

fn definition(name: &str, description: &str, schema: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: description.into(),
        schema,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClickArguments {
    observation_id: String,
    x: f64,
    y: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextArguments {
    observation_id: String,
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyArguments {
    observation_id: String,
    key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScrollArguments {
    observation_id: String,
    x: f64,
    y: f64,
    delta_y: f64,
}

fn parse<T: for<'de> Deserialize<'de>>(arguments: Value) -> Result<T, String> {
    serde_json::from_value(arguments).map_err(|_| "工具参数格式无效".into())
}

fn validate_observation_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
    {
        return Err("观察标识无效".into());
    }
    Ok(())
}

fn validate_text(text: &str) -> Result<(), String> {
    if text.is_empty()
        || text.encode_utf16().count() > 256
        || text.chars().any(|character| character.is_control())
    {
        return Err("输入文本为空、过长或包含控制字符".into());
    }
    Ok(())
}

fn validate_key(key: &str) -> Result<(), String> {
    if ![
        "enter",
        "tab",
        "escape",
        "backspace",
        "left",
        "right",
        "down",
        "up",
        "cmd+a",
        "cmd+l",
        "cmd+backspace",
    ]
    .contains(&key)
    {
        return Err("不支持的按键".into());
    }
    Ok(())
}

fn validate_point(x: f64, y: f64) -> Result<(), String> {
    if !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0 {
        return Err("坐标无效".into());
    }
    Ok(())
}

fn validate_element_id(value: &str) -> Result<(), String> {
    if value.len() > 128
        || !["ax-", "uia-", "atspi-"]
            .iter()
            .any(|prefix| value.starts_with(prefix))
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
    {
        return Err("元素 ID 无效".into());
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ElementClickArguments {
    observation_id: String,
    element_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ElementTypeArguments {
    observation_id: String,
    element_id: String,
    text: String,
    #[serde(default = "default_true")]
    clear: bool,
}

fn default_true() -> bool {
    true
}

struct ClickElement(Arc<dyn DesktopControl>);

#[async_trait]
impl Tool for ClickElement {
    fn definition(&self) -> ToolDefinition {
        definition(
            "click_element",
            "按最近一次 observe_screen 返回的 element_id 点击可访问性元素。优先于坐标 click；元素 ID 只在本次观察内有效，动作后必须重新观察。",
            json!({
                "type":"object",
                "properties":{
                    "observation_id":{"type":"string","minLength":1,"maxLength":128},
                    "element_id":{"type":"string","minLength":1,"maxLength":128}
                },
                "required":["observation_id","element_id"],
                "additionalProperties":false
            }),
        )
    }

    fn execution_target(&self) -> ExecutionTarget {
        ExecutionTarget::LocalComputer
    }

    async fn execute(&self, context: &ToolContext, arguments: Value) -> Result<ToolEffect, String> {
        let input: ElementClickArguments = parse(arguments)?;
        validate_observation_id(&input.observation_id)?;
        validate_element_id(&input.element_id)?;
        let result = self
            .0
            .act(
                &context.task_id,
                &context.call_id,
                &input.observation_id,
                GuiAction::ClickElement {
                    element_id: input.element_id,
                },
            )
            .await?;
        Ok(ToolEffect::Result(json!({
            "dispatched": true,
            "verification_required": true,
            "detail": result
        })))
    }
}

struct TypeElement(Arc<dyn DesktopControl>);

#[async_trait]
impl Tool for TypeElement {
    fn definition(&self) -> ToolDefinition {
        definition(
            "type_element",
            "按最近一次 observe_screen 返回的 element_id 向输入元素输入文本，优先使用 AX/UIA 设置值；clear=true 替换，false 追加。不要向密码/安全输入框输入。动作后必须重新观察。",
            json!({
                "type":"object",
                "properties":{
                    "observation_id":{"type":"string","minLength":1,"maxLength":128},
                    "element_id":{"type":"string","minLength":1,"maxLength":128},
                    "text":{"type":"string","minLength":1,"maxLength":256},
                    "clear":{"type":"boolean","default":true}
                },
                "required":["observation_id","element_id","text"],
                "additionalProperties":false
            }),
        )
    }

    fn execution_target(&self) -> ExecutionTarget {
        ExecutionTarget::LocalComputer
    }

    async fn execute(&self, context: &ToolContext, arguments: Value) -> Result<ToolEffect, String> {
        let input: ElementTypeArguments = parse(arguments)?;
        validate_observation_id(&input.observation_id)?;
        validate_element_id(&input.element_id)?;
        validate_text(&input.text)?;
        let result = self
            .0
            .act(
                &context.task_id,
                &context.call_id,
                &input.observation_id,
                GuiAction::TypeElement {
                    element_id: input.element_id,
                    text: input.text,
                    clear: input.clear,
                },
            )
            .await?;
        Ok(ToolEffect::Result(json!({
            "dispatched": true,
            "verification_required": true,
            "detail": result
        })))
    }
}

struct RecordScreen(Arc<dyn DesktopControl>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordArguments {
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    frames: Option<u32>,
}

#[async_trait]
impl Tool for RecordScreen {
    fn definition(&self) -> ToolDefinition {
        definition(
            "record_screen",
            "连续抓取当前受控窗口的多帧关键画面（录屏式观察），用于动画、视频、过渡和动态 UI；优先于单张 observe_screen 处理动态内容。返回多张图像和每帧 observation_id，最后一次观察可用于紧接着的一次动作。",
            json!({
                "type":"object",
                "properties":{
                    "duration_ms":{"type":"integer","minimum":500,"maximum":3000},
                    "frames":{"type":"integer","minimum":2,"maximum":6}
                },
                "additionalProperties":false
            }),
        )
    }

    fn execution_target(&self) -> ExecutionTarget {
        ExecutionTarget::LocalComputer
    }

    async fn execute(&self, context: &ToolContext, arguments: Value) -> Result<ToolEffect, String> {
        let input: RecordArguments = parse(arguments)?;
        let duration = input.duration_ms.unwrap_or(1500).clamp(500, 3000);
        let frames = input.frames.unwrap_or(4).clamp(2, 6);
        let interval = duration / u64::from(frames.saturating_sub(1).max(1));
        let mut images = Vec::new();
        let mut records = Vec::new();
        let mut latest = String::new();
        for index in 0..frames {
            let observation = self.0.observe(&context.task_id).await?;
            validate_observation_id(&observation.observation_id)?;
            latest = observation.observation_id.clone();
            records.push(json!({
                "index": index,
                "observation_id": observation.observation_id,
                "width": observation.width,
                "height": observation.height,
            }));
            images.push(yukii_agent_ports::model::ImageContent {
                media_type: "image/png".into(),
                base64: observation.png_base64,
                width: observation.width,
                height: observation.height,
            });
            if index + 1 < frames {
                tokio::time::sleep(std::time::Duration::from_millis(interval)).await;
            }
        }
        Ok(ToolEffect::ResultWithImages {
            value: json!({
                "frames": records,
                "latest_observation_id": latest,
                "duration_ms": duration,
                "note": "这是连续关键帧，不是视频文件；动态任务优先使用它，动作仍绑定最近一次 observation_id。"
            }),
            images,
        })
    }
}

struct ObserveScreen(Arc<dyn DesktopControl>);

#[async_trait]
impl Tool for ObserveScreen {
    fn definition(&self) -> ToolDefinition {
        definition(
            "observe_screen",
            "截取当前受控窗口并查看画面。截图会作为图像发送给模型。每次点击、输入或滚动前都必须先观察，取得新的 observation_id；旧截图不能用于新动作。",
            json!({"type":"object","properties":{},"additionalProperties":false}),
        )
    }

    fn execution_target(&self) -> ExecutionTarget {
        ExecutionTarget::LocalComputer
    }

    async fn execute(
        &self,
        context: &ToolContext,
        _arguments: Value,
    ) -> Result<ToolEffect, String> {
        let observation = self.0.observe(&context.task_id).await?;
        validate_observation_id(&observation.observation_id)?;
        let content = json!({
            "observation_id": observation.observation_id,
            "width": observation.width,
            "height": observation.height,
            "coordinate_space": "image_pixels",
            "elements": observation.elements.iter().take(200).cloned().collect::<Vec<_>>(),
            "element_note": "优先使用 elements 中的 element_id 调用 click_element / type_element；坐标点击只作为回退。",
            "note": "截图对应一次观察；它只能用于紧接着的一次 GUI 动作。"
        });
        Ok(ToolEffect::ResultWithImages {
            value: content,
            images: vec![yukii_agent_ports::model::ImageContent {
                media_type: "image/png".into(),
                base64: observation.png_base64,
                width: observation.width,
                height: observation.height,
            }],
        })
    }
}

struct Click(Arc<dyn DesktopControl>);

#[async_trait]
impl Tool for Click {
    fn definition(&self) -> ToolDefinition {
        definition(
            "click",
            "在受控窗口内点击。坐标使用最近一次 observe_screen 截图的像素坐标，原点在图像左上角。必须提供该截图返回的 observation_id；执行后必须重新观察，不能仅凭“已派发”声称操作成功。",
            json!({
                "type":"object",
                "properties":{
                    "observation_id":{"type":"string","minLength":1,"maxLength":128},
                    "x":{"type":"number","minimum":0},
                    "y":{"type":"number","minimum":0}
                },
                "required":["observation_id","x","y"],
                "additionalProperties":false
            }),
        )
    }

    fn execution_target(&self) -> ExecutionTarget {
        ExecutionTarget::LocalComputer
    }

    async fn execute(&self, context: &ToolContext, arguments: Value) -> Result<ToolEffect, String> {
        let input: ClickArguments = parse(arguments)?;
        validate_observation_id(&input.observation_id)?;
        validate_point(input.x, input.y)?;
        let result = self
            .0
            .act(
                &context.task_id,
                &context.call_id,
                &input.observation_id,
                GuiAction::Click {
                    x: input.x,
                    y: input.y,
                },
            )
            .await?;
        Ok(ToolEffect::Result(json!({
            "dispatched": true,
            "verification_required": true,
            "detail": result
        })))
    }
}

struct TypeText(Arc<dyn DesktopControl>);

#[async_trait]
impl Tool for TypeText {
    fn definition(&self) -> ToolDefinition {
        definition(
            "type_text",
            "向受控窗口当前焦点输入最多 256 个 UTF-16 单元的文本。必须绑定最近一次观察；不包含回车等控制字符，发送/回车需另行使用 press_key。输入后必须重新观察确认。",
            json!({
                "type":"object",
                "properties":{
                    "observation_id":{"type":"string","minLength":1,"maxLength":128},
                    "text":{"type":"string","minLength":1,"maxLength":256}
                },
                "required":["observation_id","text"],
                "additionalProperties":false
            }),
        )
    }

    fn execution_target(&self) -> ExecutionTarget {
        ExecutionTarget::LocalComputer
    }

    async fn execute(&self, context: &ToolContext, arguments: Value) -> Result<ToolEffect, String> {
        let input: TextArguments = parse(arguments)?;
        validate_observation_id(&input.observation_id)?;
        validate_text(&input.text)?;
        let result = self
            .0
            .act(
                &context.task_id,
                &context.call_id,
                &input.observation_id,
                GuiAction::TypeText { text: input.text },
            )
            .await?;
        Ok(ToolEffect::Result(json!({
            "dispatched": true,
            "verification_required": true,
            "detail": result
        })))
    }
}

struct ReplaceText(Arc<dyn DesktopControl>);

#[async_trait]
impl Tool for ReplaceText {
    fn definition(&self) -> ToolDefinition {
        definition(
            "replace_text",
            "替换受控窗口当前焦点输入框的全部文本：内部先全选再输入，不会逐字符删除。必须绑定最近一次观察；适合地址栏、搜索框和表单。输入后重新观察确认。",
            json!({
                "type":"object",
                "properties":{
                    "observation_id":{"type":"string","minLength":1,"maxLength":128},
                    "text":{"type":"string","minLength":1,"maxLength":256}
                },
                "required":["observation_id","text"],
                "additionalProperties":false
            }),
        )
    }

    fn execution_target(&self) -> ExecutionTarget {
        ExecutionTarget::LocalComputer
    }

    async fn execute(&self, context: &ToolContext, arguments: Value) -> Result<ToolEffect, String> {
        let input: TextArguments = parse(arguments)?;
        validate_observation_id(&input.observation_id)?;
        validate_text(&input.text)?;
        let result = self
            .0
            .act(
                &context.task_id,
                &context.call_id,
                &input.observation_id,
                GuiAction::ReplaceText { text: input.text },
            )
            .await?;
        Ok(ToolEffect::Result(json!({
            "dispatched": true,
            "verification_required": true,
            "detail": result
        })))
    }
}

struct PressKey(Arc<dyn DesktopControl>);

#[async_trait]
impl Tool for PressKey {
    fn definition(&self) -> ToolDefinition {
        definition(
            "press_key",
            "向受控窗口发送受限按键：enter、tab、escape、backspace、left、right、down、up，以及组合键 cmd+a（全选）、cmd+l（聚焦地址栏）、cmd+backspace。替换整段文本优先用 replace_text，不要连续按 backspace。必须绑定最近一次观察；执行后重新观察。",
            json!({
                "type":"object",
                "properties":{
                    "observation_id":{"type":"string","minLength":1,"maxLength":128},
                    "key":{"type":"string","enum":["enter","tab","escape","backspace","left","right","down","up","cmd+a","cmd+l","cmd+backspace"]}
                },
                "required":["observation_id","key"],
                "additionalProperties":false
            }),
        )
    }

    fn execution_target(&self) -> ExecutionTarget {
        ExecutionTarget::LocalComputer
    }

    async fn execute(&self, context: &ToolContext, arguments: Value) -> Result<ToolEffect, String> {
        let input: KeyArguments = parse(arguments)?;
        validate_observation_id(&input.observation_id)?;
        validate_key(&input.key)?;
        let result = self
            .0
            .act(
                &context.task_id,
                &context.call_id,
                &input.observation_id,
                GuiAction::PressKey { key: input.key },
            )
            .await?;
        Ok(ToolEffect::Result(json!({
            "dispatched": true,
            "verification_required": true,
            "detail": result
        })))
    }
}

struct Scroll(Arc<dyn DesktopControl>);

#[async_trait]
impl Tool for Scroll {
    fn definition(&self) -> ToolDefinition {
        definition(
            "scroll",
            "在受控窗口内滚动。坐标使用最近一次截图的像素坐标；delta_y 为 -1000 到 1000 的非零值，负值向上。必须绑定 observation_id，执行后重新观察。",
            json!({
                "type":"object",
                "properties":{
                    "observation_id":{"type":"string","minLength":1,"maxLength":128},
                    "x":{"type":"number","minimum":0},
                    "y":{"type":"number","minimum":0},
                    "delta_y":{"type":"number","minimum":-1000,"maximum":1000}
                },
                "required":["observation_id","x","y","delta_y"],
                "additionalProperties":false
            }),
        )
    }

    fn execution_target(&self) -> ExecutionTarget {
        ExecutionTarget::LocalComputer
    }

    async fn execute(&self, context: &ToolContext, arguments: Value) -> Result<ToolEffect, String> {
        let input: ScrollArguments = parse(arguments)?;
        validate_observation_id(&input.observation_id)?;
        validate_point(input.x, input.y)?;
        if !input.delta_y.is_finite() || input.delta_y.abs() < 1.0 || input.delta_y.abs() > 1000.0 {
            return Err("滚动量无效".into());
        }
        let result = self
            .0
            .act(
                &context.task_id,
                &context.call_id,
                &input.observation_id,
                GuiAction::Scroll {
                    x: input.x,
                    y: input.y,
                    delta_y: input.delta_y,
                },
            )
            .await?;
        Ok(ToolEffect::Result(json!({
            "dispatched": true,
            "verification_required": true,
            "detail": result
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use yukii_agent_ports::tools::DesktopObservationResult;
    use yukii_contracts::desktop::UiElement;

    #[derive(Default)]
    struct FakeControl {
        calls: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl DesktopControl for FakeControl {
        async fn observe(&self, task_id: &str) -> Result<DesktopObservationResult, String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("observe:{task_id}"));
            Ok(DesktopObservationResult {
                observation_id: "observation_1".into(),
                png_base64: "iVBORw0KGgo=".into(),
                width: 100,
                height: 50,
                elements: vec![UiElement {
                    element_id: "ax-0.3".into(),
                    role: "AXTextField".into(),
                    title: "输入框".into(),
                    description: String::new(),
                    value: String::new(),
                    frame: [10.0, 20.0, 100.0, 30.0],
                    enabled: true,
                    focused: true,
                    actions: vec!["AXConfirm".into()],
                }],
            })
        }

        async fn act(
            &self,
            task_id: &str,
            call_id: &str,
            observation_id: &str,
            action: GuiAction,
        ) -> Result<String, String> {
            self.calls.lock().unwrap().push(format!(
                "act:{task_id}:{call_id}:{observation_id}:{action:?}"
            ));
            Ok("ok".into())
        }
    }

    fn tool(name: &str, control: Arc<FakeControl>) -> Arc<dyn Tool> {
        ComputerTools::new(control)
            .into_tools()
            .into_iter()
            .find(|tool| tool.definition().name == name)
            .unwrap()
    }

    #[tokio::test]
    async fn observation_is_the_only_source_of_images_and_all_tools_are_local() {
        let control = Arc::new(FakeControl::default());
        let observe = tool("observe_screen", control.clone());
        assert_eq!(observe.execution_target(), ExecutionTarget::LocalComputer);
        let context = ToolContext::desktop("task", "call");
        let effect = observe.execute(&context, json!({})).await.unwrap();
        match effect {
            ToolEffect::ResultWithImages { value, images } => {
                assert_eq!(value["observation_id"], "observation_1");
                assert_eq!(value["elements"][0]["element_id"], "ax-0.3");
                assert_eq!(images.len(), 1);
                assert_eq!(images[0].media_type, "image/png");
            }
            _ => panic!("观察必须返回图像结果"),
        }
        let click = tool("click", control.clone());
        click
            .execute(
                &context,
                json!({"observation_id":"observation_1","x":10,"y":20}),
            )
            .await
            .unwrap();
        assert!(
            click
                .execute(
                    &context,
                    json!({"observation_id":"observation_1","x":"10","y":20})
                )
                .await
                .is_err()
        );
        assert!(
            click
                .execute(
                    &context,
                    json!({"observation_id":"observation_1","x":10,"y":20,"approved":true})
                )
                .await
                .is_err()
        );
        assert!(control.calls.lock().unwrap().len() == 2);

        let click_element = tool("click_element", control.clone());
        click_element
            .execute(
                &context,
                json!({"observation_id":"observation_1","element_id":"ax-0.3"}),
            )
            .await
            .unwrap();
        let type_element = tool("type_element", control.clone());
        type_element
            .execute(
                &context,
                json!({"observation_id":"observation_1","element_id":"ax-0.4","text":"hello","clear":false}),
            )
            .await
            .unwrap();
        assert!(
            click_element
                .execute(
                    &context,
                    json!({"observation_id":"observation_1","element_id":"ax-0.3;rm"})
                )
                .await
                .is_err()
        );
        let calls = control.calls.lock().unwrap();
        assert!(calls.iter().any(|call| call.contains("ClickElement")));
        assert!(calls.iter().any(|call| call.contains("TypeElement")));
    }

    #[tokio::test]
    async fn text_and_key_tools_reject_unsupported_control_characters_at_the_domain_boundary() {
        let control = Arc::new(FakeControl::default());
        let context = ToolContext::desktop("task", "call");
        let type_text = tool("type_text", control.clone());
        type_text
            .execute(
                &context,
                json!({"observation_id":"observation_1","text":"你好"}),
            )
            .await
            .unwrap();
        assert!(
            type_text
                .execute(
                    &context,
                    json!({"observation_id":"observation_1","text":"a\nb"})
                )
                .await
                .is_err()
        );
        let press = tool("press_key", control.clone());
        assert!(
            press
                .execute(
                    &context,
                    json!({"observation_id":"observation_1","key":"volume_up"})
                )
                .await
                .is_err()
        );
        assert!(
            press
                .execute(
                    &context,
                    json!({"observation_id":"observation_1","key":"cmd+l"})
                )
                .await
                .is_ok()
        );
        let replace = tool("replace_text", control);
        assert!(
            replace
                .execute(
                    &context,
                    json!({"observation_id":"observation_1","text":"bilibili.com"})
                )
                .await
                .is_ok()
        );
    }
}
