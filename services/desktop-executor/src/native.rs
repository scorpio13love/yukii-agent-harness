//! 每次操作启动一个短生命周期原生桥进程。桥不监听网络，stdin/stdout 只承载单个 JSON 请求/响应。
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use thiserror::Error;
use tokio::{io::AsyncWriteExt, process::Command, time::timeout};
use yukii_contracts::desktop::{ComputerPermissions, DesktopWindow, GuiAction, UiElement};
use yukii_desktop_executor::{NativeBridge, NativeCapture, NativeError};

const MAX_BRIDGE_OUTPUT: usize = 8 * 1024 * 1024;
const BRIDGE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum BridgeLaunchError {
    #[error("原生桥必须是可执行的绝对路径")]
    Path,
    #[error("原生桥不可用：{0}")]
    Io(#[from] std::io::Error),
}

/// 进程边界只接受明确路径；不读取 PATH，避免宿主环境中的同名程序劫持。
pub struct ProcessBridge {
    path: PathBuf,
    request_timeout: Duration,
}

impl ProcessBridge {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, BridgeLaunchError> {
        let path = path.into();
        if !path.is_absolute() || !path.is_file() {
            return Err(BridgeLaunchError::Path);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::metadata(&path)?.permissions().mode() & 0o111 == 0 {
                return Err(BridgeLaunchError::Path);
            }
        }
        Ok(Self {
            path,
            request_timeout: BRIDGE_TIMEOUT,
        })
    }

    pub async fn call(&self, request: Value) -> Result<Value, NativeError> {
        let bytes = serde_json::to_vec(&request)
            .map_err(|_| NativeError::Unknown("无法编码原生桥请求".into()))?;
        let mut child = Command::new(&self.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| NativeError::Unknown("无法启动原生桥进程".into()))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| NativeError::Unknown("原生桥输入管道不可用".into()))?;
        stdin
            .write_all(&bytes)
            .await
            .map_err(|_| NativeError::Unknown("写入原生桥失败".into()))?;
        drop(stdin);
        let output = match timeout(self.request_timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(_)) => {
                eprintln!("desktop executor: 等待原生桥进程失败");
                return Err(NativeError::Unknown(
                    "等待原生桥失败，结果可能不确定".into(),
                ));
            }
            Err(_) => {
                eprintln!(
                    "desktop executor: 原生桥操作超过 {} 秒，已终止；结果按未知处理",
                    self.request_timeout.as_secs()
                );
                return Err(NativeError::Unknown(
                    "原生桥操作超时，结果可能不确定；禁止自动重试".into(),
                ));
            }
        };
        if output.stdout.len() > MAX_BRIDGE_OUTPUT {
            eprintln!(
                "desktop executor: 原生桥输出 {} 字节，超过上限",
                output.stdout.len()
            );
            return Err(NativeError::Unknown("原生桥输出超过上限".into()));
        }
        // 非零退出表示原生桥在派发前拒绝；若已派发则桥会以成功状态返回。
        if !output.status.success() {
            if let Ok(value) = serde_json::from_slice::<Value>(&output.stdout)
                && let Some(code) = value["error"].as_str()
            {
                return Err(NativeError::Rejected(describe(code)));
            }
            return Err(NativeError::Unknown(
                "原生桥异常退出，动作结果不确定".into(),
            ));
        }
        let value: Value = serde_json::from_slice(&output.stdout)
            .map_err(|_| NativeError::Unknown("原生桥返回了无效 JSON".into()))?;
        if let Some(code) = value["error"].as_str() {
            return Err(NativeError::Rejected(describe(code)));
        }
        Ok(value)
    }

    async fn request_screen_capture(
        &self,
        window: &DesktopWindow,
    ) -> Result<NativeCapture, NativeError> {
        let value = self
            .call(json!({
                "operation": "observe",
                "window_id": window.window_id,
                "pid": window.pid
            }))
            .await?;
        let png_base64 = value["png_base64"]
            .as_str()
            .ok_or_else(|| NativeError::Unknown("原生桥截图缺少内容".into()))?
            .to_owned();
        let width = value["width"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| NativeError::Unknown("原生桥截图宽度无效".into()))?;
        let height = value["height"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| NativeError::Unknown("原生桥截图高度无效".into()))?;
        let frame = value["frame"]
            .as_array()
            .filter(|items| items.len() == 4)
            .and_then(|items| {
                let mut frame = [0.0; 4];
                for (index, item) in items.iter().enumerate() {
                    frame[index] = item.as_f64()?;
                }
                Some(frame)
            })
            .ok_or_else(|| NativeError::Unknown("原生桥窗口几何信息无效".into()))?;
        let elements: Vec<UiElement> = value
            .get("elements")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| NativeError::Unknown("原生桥元素快照格式无效".into()))?
            .unwrap_or_default();
        Ok(NativeCapture {
            png_base64,
            width,
            height,
            frame,
            elements,
        })
    }
}

#[async_trait]
impl NativeBridge for ProcessBridge {
    async fn permissions(&self) -> Result<ComputerPermissions, NativeError> {
        let value = self.call(json!({"operation":"permissions"})).await?;
        Ok(ComputerPermissions {
            screen_capture: value["screen_capture"]
                .as_bool()
                .ok_or_else(|| NativeError::Unknown("原生桥权限响应无效".into()))?,
            accessibility: value["accessibility"]
                .as_bool()
                .ok_or_else(|| NativeError::Unknown("原生桥权限响应无效".into()))?,
        })
    }

    async fn windows(&self) -> Result<Vec<DesktopWindow>, NativeError> {
        let value = self.call(json!({"operation":"windows"})).await?;
        let items = value["windows"]
            .as_array()
            .ok_or_else(|| NativeError::Unknown("原生桥窗口列表无效".into()))?;
        let mut windows = Vec::new();
        for item in items.iter().take(128) {
            let window: DesktopWindow = serde_json::from_value(item.clone())
                .map_err(|_| NativeError::Unknown("原生桥返回了无效窗口".into()))?;
            windows.push(window);
        }
        Ok(windows)
    }

    async fn observe(&self, window: &DesktopWindow) -> Result<NativeCapture, NativeError> {
        self.request_screen_capture(window).await
    }

    async fn act(
        &self,
        window: &DesktopWindow,
        frame: [f64; 4],
        action: &GuiAction,
    ) -> Result<(), NativeError> {
        let mut request = json!({
            "window_id": window.window_id,
            "pid": window.pid,
            "frame": frame
        });
        match action {
            GuiAction::Click { x, y } => {
                request["operation"] = json!("click");
                request["x"] = json!(x);
                request["y"] = json!(y);
            }
            GuiAction::Scroll { x, y, delta_y } => {
                request["operation"] = json!("scroll");
                request["x"] = json!(x);
                request["y"] = json!(y);
                request["delta_y"] = json!(delta_y);
            }
            GuiAction::TypeText { text } => {
                request["operation"] = json!("type_text");
                request["text"] = json!(text);
            }
            GuiAction::ReplaceText { text } => {
                request["operation"] = json!("replace_text");
                request["text"] = json!(text);
            }
            GuiAction::PressKey { key } => {
                request["operation"] = json!("key");
                request["key"] = json!(key);
            }
            GuiAction::ClickElement { element_id } => {
                request["operation"] = json!("click_element");
                request["element_id"] = json!(element_id);
            }
            GuiAction::TypeElement {
                element_id,
                text,
                clear,
            } => {
                request["operation"] = json!("type_element");
                request["element_id"] = json!(element_id);
                request["text"] = json!(text);
                request["clear"] = json!(clear);
            }
        }
        let value = self.call(request).await?;
        if value["dispatched"] != json!(true) {
            return Err(NativeError::Unknown(
                "原生桥未确认派发，不能当作动作已完成".into(),
            ));
        }
        Ok(())
    }
}

/// 原生桥只返回稳定错误码；这里映射为可操作的中文说明，不拼接系统路径或窗口标题。
fn describe(code: &str) -> String {
    match code {
        "screen_capture_permission_required" => "需要屏幕录制权限，请在系统设置中授权后重新开始",
        "accessibility_permission_required" => "需要辅助功能权限，请在系统设置中授权后重新开始",
        "target_window_missing" => "目标窗口已关闭或不可见，请重新选择",
        "target_not_frontmost" => "目标窗口不在前台，请让目标窗口获得焦点后重试",
        "window_moved_reobserve" => "窗口位置已变化，请重新观察",
        "different_focused_window" => "目标应用的前台窗口已变化，请重新观察",
        "coordinates_outside_window" => "点击坐标超出受控窗口",
        "invalid_text" => "输入文本包含不支持的控制字符或长度超限",
        "unsupported_key" => "该按键不在允许列表",
        "event_creation_failed" => "系统拒绝创建输入事件",
        "screenshot_encoding_failed" => "截图编码失败或超过大小上限",
        "invalid_request" => "原生桥请求格式无效",
        other => other,
    }
    .to_owned()
}

/// 供宿主/测试检查桥路径，不读取桥内容。
pub fn bridge_exists(path: &Path) -> bool {
    path.is_absolute() && path.is_file()
}
