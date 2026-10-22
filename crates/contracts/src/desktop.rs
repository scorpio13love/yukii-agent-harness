//! 私有 stdio IPC v2。仅由受信父进程持有管道；不是网页 API，也不支持自授予权限。
//!
//! v2 在文件执行之外增加受控桌面观察与 GUI 动作。所有图像只返回给创建管道的宿主，
//! 是否把截图发送给模型由宿主单独征得用户同意；执行端不读取模型配置，也不调用模型。
use crate::permissions::PermissionBinding;
use serde::{Deserialize, Serialize};

pub const DESKTOP_PROTOCOL_VERSION: u32 = 2;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopRequest {
    pub version: u32,
    pub request_id: String,
    pub command: DesktopCommand,
}

/// serde 不支持在内部 tag 枚举上可靠拒绝额外字段；私有 IPC 在反序列化前单独检查。
/// 未知的 type 留给 serde 明确拒绝，这里只约束已知命令的字段集合。
pub fn validate_command_shape(value: &serde_json::Value) -> Result<(), &'static str> {
    let object = value.as_object().ok_or("command_not_object")?;
    let tag = object
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or("command_type_missing")?;
    let allowed: &[&str] = match tag {
        "hello" => &[],
        "register_workspace" => &["path"],
        "unregister_workspace" => &["workspace_id"],
        "execute_file" => &["request"],
        "acknowledge" => &["permission_id"],
        "computer_permissions" | "list_windows" | "control_status" => &[],
        "begin_control" => &["task_id", "window", "screenshot_sharing", "expires_at"],
        "observe" | "pause_control" | "resume_control" | "end_control" => &["task_id"],
        "act" => &["task_id", "call_id", "observation_id", "action"],
        _ => return Ok(()),
    };
    for key in object.keys() {
        if key != "type" && !allowed.contains(&key.as_str()) {
            return Err("unknown_field");
        }
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesktopCommand {
    Hello,
    RegisterWorkspace {
        path: String,
    },
    UnregisterWorkspace {
        workspace_id: String,
    },
    ExecuteFile {
        request: FileRequest,
    },
    Acknowledge {
        permission_id: String,
    },
    /// 只读系统权限探测；不会触发系统授权弹窗，也不会自行申请 TCC。
    ComputerPermissions,
    /// 列出可被显式选择的窗口；不返回屏幕像素。
    ListWindows,
    /// 宿主在用户明确同意后调用。执行端仍会复查窗口、期限并登记审计记录。
    BeginControl {
        task_id: String,
        window: DesktopWindow,
        screenshot_sharing: bool,
        expires_at: i64,
    },
    Observe {
        task_id: String,
    },
    /// observation_id 绑定一次截图；执行端只允许对它派发一次动作。
    Act {
        task_id: String,
        call_id: String,
        observation_id: String,
        action: GuiAction,
    },
    PauseControl {
        task_id: String,
    },
    ResumeControl {
        task_id: String,
    },
    EndControl {
        task_id: String,
    },
    ControlStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRequest {
    pub permission_id: String,
    pub binding: PermissionBinding,
    pub content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileResult {
    Text { content: String },
    Created { bytes: usize, sha256: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesktopExecutionResult {
    Completed {
        result: FileResult,
    },
    Rejected {
        code: String,
        message: String,
    },
    /// 可能已经发生外部副作用，不能因为断连或重启自动再次执行。
    OutcomeUnknown {
        message: String,
    },
}

/// 由原生桥返回的窗口身份。pid 与 window_id 共同定位，标题只用于让用户确认。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopWindow {
    pub window_id: u32,
    pub pid: i32,
    pub application: String,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlState {
    Active,
    Paused,
    Ended,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlSession {
    pub control_id: String,
    pub task_id: String,
    pub window: DesktopWindow,
    pub screenshot_sharing: bool,
    pub generation: u64,
    pub expires_at: i64,
    pub state: ControlState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerPermissions {
    pub screen_capture: bool,
    pub accessibility: bool,
}

/// 平台无关的可访问性元素快照。macOS 映射 AX，Windows 映射 UIA，Linux 映射 AT-SPI。
/// element_id 由原生树路径生成，只在最近一次观察内有效。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiElement {
    pub element_id: String,
    pub role: String,
    pub title: String,
    pub description: String,
    /// 密码等安全输入框不回传真实值。
    pub value: String,
    /// 窗口内逻辑坐标 [x, y, width, height]；不可用时为全 0。
    pub frame: [f64; 4],
    pub enabled: bool,
    pub focused: bool,
    pub actions: Vec<String>,
}

/// 一次可被模型查看的窗口截图。图像尺寸是像素，frame 是屏幕坐标系中的逻辑点。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopObservation {
    pub observation_id: String,
    pub window: DesktopWindow,
    pub png_base64: String,
    pub width: u32,
    pub height: u32,
    pub frame: [f64; 4],
    #[serde(default)]
    pub elements: Vec<UiElement>,
    pub captured_at: i64,
    pub expires_at: i64,
}

/// 坐标以截图像素表示，执行端按截图与窗口逻辑尺寸的比例换算后再派发。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum GuiAction {
    Click {
        x: f64,
        y: f64,
    },
    Scroll {
        x: f64,
        y: f64,
        delta_y: f64,
    },
    TypeText {
        text: String,
    },
    /// 选中当前焦点输入框全部内容后输入新文本；用于替换地址栏/搜索框，避免逐字符删除。
    ReplaceText {
        text: String,
    },
    PressKey {
        key: String,
    },
    /// 按最近一次观察返回的 element_id 触发 AX/UIA/AT-SPI 动作。
    ClickElement {
        element_id: String,
    },
    /// 按元素设置/替换文本；clear=false 表示追加。
    TypeElement {
        element_id: String,
        text: String,
        clear: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionDispatch {
    pub action_id: String,
    pub dispatched_at: i64,
    /// Quartz 投递没有应用消费回执，必须重新观察后才能声称目标达成。
    pub verification_required: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopResponse {
    pub version: u32,
    pub request_id: String,
    pub reply: DesktopReply,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesktopReply {
    Hello {
        file_execution: bool,
        computer_use: bool,
    },
    Workspace {
        workspace_id: String,
    },
    Done,
    Execution {
        result: DesktopExecutionResult,
        replayed: bool,
    },
    Permissions {
        permissions: ComputerPermissions,
    },
    Windows {
        windows: Vec<DesktopWindow>,
    },
    Control {
        control: ControlSession,
    },
    Observation {
        observation: DesktopObservation,
    },
    Action {
        dispatch: ActionDispatch,
    },
    ControlStatus {
        control: Option<ControlSession>,
    },
    Error {
        code: String,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_round_trip_keeps_observation_and_action_binding_explicit() {
        let request = DesktopRequest {
            version: DESKTOP_PROTOCOL_VERSION,
            request_id: "request-1".into(),
            command: DesktopCommand::Act {
                task_id: "task".into(),
                call_id: "call".into(),
                observation_id: "observation".into(),
                action: GuiAction::Click { x: 12.5, y: 30.0 },
            },
        };
        let encoded = serde_json::to_string(&request).unwrap();
        assert!(encoded.contains("\"type\":\"act\""));
        assert!(encoded.contains("\"observation_id\":\"observation\""));
        let decoded: DesktopRequest = serde_json::from_str(&encoded).unwrap();
        assert!(matches!(
            decoded.command,
            DesktopCommand::Act {
                action: GuiAction::Click { x, y },
                ..
            } if x == 12.5 && y == 30.0
        ));
    }

    #[test]
    fn unknown_fields_and_protocol_version_are_not_silently_accepted() {
        let command = serde_json::json!({"type": "control_status", "extra": true});
        assert_eq!(validate_command_shape(&command), Err("unknown_field"));
        let command = serde_json::json!({"type": "hello"});
        assert!(validate_command_shape(&command).is_ok());
        let value = serde_json::json!({
            "version": 2,
            "request_id": "r",
            "command": {"type": "hello"}
        });
        assert!(serde_json::from_value::<DesktopRequest>(value).is_ok());
        let old = serde_json::json!({
            "version": 1,
            "request_id": "r",
            "command": {"type": "hello"}
        });
        let decoded: DesktopRequest = serde_json::from_value(old).unwrap();
        assert_ne!(decoded.version, DESKTOP_PROTOCOL_VERSION);
    }
}
