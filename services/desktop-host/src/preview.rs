//! 实时预览/控制面板的私有 stdio 客户端。面板是临时原生助手，最终 UI 仍为 Chromium 147。
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::mpsc,
    time::timeout,
};
use yukii_contracts::desktop::DesktopWindow;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickedWindow {
    pub window_id: u32,
    pub pid: i32,
}

#[derive(Debug)]
enum PickerEvent {
    Selected(PickedWindow),
    Cancelled,
    Error(String),
}

/// 窗口选择器的独立进程只是“选择 + 会话同意”；行动仍由后续预览/控制会话执行。
pub struct WindowPicker;

impl WindowPicker {
    pub async fn pick(path: &Path) -> Result<PickedWindow, String> {
        if !path.is_file() {
            return Err(format!("找不到窗口选择器：{}", path.display()));
        }
        let mut child = Command::new(path)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("无法启动窗口选择器：{error}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "窗口选择器输出管道不可用".to_string())?;
        let result = timeout(Duration::from_secs(300), async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                match parse_picker_event(&line) {
                    Some(PickerEvent::Selected(window)) => return Ok(window),
                    Some(PickerEvent::Cancelled) => return Err("用户取消了窗口选择".into()),
                    Some(PickerEvent::Error(message)) => return Err(message),
                    None => {}
                }
            }
            Err("窗口选择器已关闭".into())
        })
        .await
        .map_err(|_| "窗口选择超时".to_string())?;
        let _ = child.kill().await;
        let _ = child.wait().await;
        result
    }
}

fn parse_picker_event(line: &str) -> Option<PickerEvent> {
    let value: Value = serde_json::from_str(line).ok()?;
    match value["event"].as_str()? {
        "selected" => {
            let window_id = value["window_id"].as_u64()? as u32;
            let pid = value["pid"].as_i64()? as i32;
            (window_id > 0 && pid > 0)
                .then_some(PickerEvent::Selected(PickedWindow { window_id, pid }))
        }
        "cancelled" => Some(PickerEvent::Cancelled),
        "error" => Some(PickerEvent::Error(
            value["message"].as_str().unwrap_or("窗口选择失败").into(),
        )),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewEvent {
    Ready { width: u32, height: u32 },
    Paused,
    Resumed,
    Stop,
    Closed,
    Error(String),
}

pub struct PreviewPanel {
    child: Child,
    stdin: ChildStdin,
    events: Option<mpsc::UnboundedReceiver<PreviewEvent>>,
}

impl PreviewPanel {
    /// 由宿主启动，stdin/stdout 专用；stderr 继承到宿主终端用于诊断。
    pub async fn spawn(path: &Path, window: &DesktopWindow) -> Result<Self, String> {
        if !path.is_file() {
            return Err(format!("找不到预览面板：{}", path.display()));
        }
        let mut child = Command::new(path)
            .arg("--window-id")
            .arg(window.window_id.to_string())
            .arg("--pid")
            .arg(window.pid.to_string())
            .arg("--title")
            .arg(&window.title)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("无法启动预览面板：{error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "预览面板输入管道不可用".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "预览面板输出管道不可用".to_string())?;
        let (sender, receiver) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(event) = parse_event(&line) {
                    let _ = sender.send(event);
                }
            }
            let _ = sender.send(PreviewEvent::Closed);
        });
        Ok(Self {
            child,
            stdin,
            events: Some(receiver),
        })
    }

    /// 事件接收器只能取一次；宿主在任务选择循环中使用它。
    pub fn take_events(&mut self) -> Option<mpsc::UnboundedReceiver<PreviewEvent>> {
        self.events.take()
    }

    pub async fn status(&mut self, text: &str) -> Result<(), String> {
        self.send(json!({"command":"status","text":text})).await
    }

    pub async fn close(&mut self) {
        let _ = self.send(json!({"command":"close"})).await;
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }

    async fn send(&mut self, value: Value) -> Result<(), String> {
        let mut line = value.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|_| "预览面板输入已关闭".to_string())?;
        self.stdin
            .flush()
            .await
            .map_err(|_| "预览面板输入刷新失败".to_string())
    }
}

fn parse_event(line: &str) -> Option<PreviewEvent> {
    let value: Value = serde_json::from_str(line).ok()?;
    match value["event"].as_str()? {
        "ready" => Some(PreviewEvent::Ready {
            width: value["width"].as_u64().unwrap_or(0) as u32,
            height: value["height"].as_u64().unwrap_or(0) as u32,
        }),
        "paused" => Some(PreviewEvent::Paused),
        "resumed" => Some(PreviewEvent::Resumed),
        "stopped" => Some(PreviewEvent::Stop),
        "closed" => Some(PreviewEvent::Closed),
        "error" => Some(PreviewEvent::Error(
            value["message"].as_str().unwrap_or("预览错误").into(),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_panel_events_and_ignores_noise() {
        assert_eq!(
            parse_event(r#"{"event":"ready","width":560,"height":332}"#),
            Some(PreviewEvent::Ready {
                width: 560,
                height: 332
            })
        );
        assert_eq!(
            parse_event(r#"{"event":"paused"}"#),
            Some(PreviewEvent::Paused)
        );
        assert_eq!(
            parse_event(r#"{"event":"error","message":"no capture"}"#),
            Some(PreviewEvent::Error("no capture".into()))
        );
        assert_eq!(parse_event("not json"), None);
        assert_eq!(parse_event(r#"{"event":"unknown"}"#), None);
    }

    #[test]
    fn parses_picker_selection_and_rejects_invalid_ids() {
        assert_eq!(
            parse_picker_event(r#"{"event":"selected","window_id":42,"pid":123}"#).map(|event| {
                match event {
                    PickerEvent::Selected(window) => Some(window),
                    _ => None,
                }
            }),
            Some(Some(PickedWindow {
                window_id: 42,
                pid: 123
            }))
        );
        assert!(matches!(
            parse_picker_event(r#"{"event":"cancelled"}"#),
            Some(PickerEvent::Cancelled)
        ));
        assert!(parse_picker_event(r#"{"event":"selected","window_id":0,"pid":1}"#).is_none());
        assert!(parse_picker_event(r#"{"event":"selected","window_id":1,"pid":-1}"#).is_none());
    }
}
