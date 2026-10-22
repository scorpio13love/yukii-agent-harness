//! 最小 CDP WebSocket 客户端：命令/响应关联、页面脚本与截图。
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::TcpStream,
    sync::{Mutex, oneshot},
    time::timeout,
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

pub struct CdpClient {
    writer: Mutex<SplitSink<Socket, Message>>,
    pending: Pending,
    next_id: AtomicU64,
}

impl CdpClient {
    /// 只连接本机或其他受控 CDP 端点；调用方负责 endpoint 的来源与授权。
    pub async fn connect(web_socket_url: &str) -> Result<Self, String> {
        let (socket, _) = connect_async(web_socket_url)
            .await
            .map_err(|error| format!("无法连接 CDP：{error}"))?;
        let (writer, mut reader) = socket.split();
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = pending.clone();
        tokio::spawn(async move {
            while let Some(message) = reader.next().await {
                match message {
                    Ok(Message::Text(text)) => {
                        let Ok(value) = serde_json::from_str::<Value>(text.as_str()) else {
                            continue;
                        };
                        let Some(id) = value.get("id").and_then(Value::as_u64) else {
                            continue;
                        };
                        if let Some(sender) = reader_pending.lock().await.remove(&id) {
                            let _ = sender.send(Ok(value));
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => break,
                    _ => {}
                }
            }
            let mut pending = reader_pending.lock().await;
            for (_, sender) in pending.drain() {
                let _ = sender.send(Err("CDP 连接已断开".into()));
            }
        });
        Ok(Self {
            writer: Mutex::new(writer),
            pending,
            next_id: AtomicU64::new(1),
        })
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().await.insert(id, sender);
        let payload = json!({"id":id,"method":method,"params":params}).to_string();
        let sent = self
            .writer
            .lock()
            .await
            .send(Message::Text(payload.into()))
            .await;
        if let Err(error) = sent {
            self.pending.lock().await.remove(&id);
            return Err(format!("CDP 写入失败：{error}"));
        }
        let response = match timeout(Duration::from_secs(20), receiver).await {
            Ok(Ok(Ok(response))) => response,
            Ok(Ok(Err(error))) => return Err(error),
            Ok(Err(_)) => return Err("CDP 响应通道关闭".into()),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                return Err(format!("CDP {method} 超时"));
            }
        };
        if let Some(error) = response.get("error") {
            let message = error["message"].as_str().unwrap_or("未知 CDP 错误");
            return Err(format!("CDP {method} 失败：{message}"));
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    pub async fn evaluate(&self, expression: &str) -> Result<Value, String> {
        let result = self
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true
                }),
            )
            .await?;
        if let Some(details) = result.get("exceptionDetails") {
            let text = details["text"]
                .as_str()
                .or_else(|| details["exception"]["description"].as_str())
                .unwrap_or("页面脚本异常");
            return Err(text.chars().take(300).collect());
        }
        Ok(result
            .get("result")
            .and_then(|value| value.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }

    pub async fn navigate(&self, url: &str) -> Result<(), String> {
        let result = self.call("Page.navigate", json!({"url":url})).await?;
        if let Some(error) = result.get("errorText").and_then(Value::as_str) {
            return Err(format!("页面导航失败：{error}"));
        }
        Ok(())
    }

    /// 默认按 CSS 视口截取，避免 Retina 设备像素把图片放大 2 倍。
    pub async fn screenshot(&self, viewport: Option<(u32, u32)>) -> Result<String, String> {
        let params = match viewport {
            Some((width, height)) if width > 0 && height > 0 => json!({
                "format": "png",
                "fromSurface": true,
                "captureBeyondViewport": false,
                "clip": {"x": 0, "y": 0, "width": width, "height": height, "scale": 1}
            }),
            _ => json!({"format":"png","fromSurface":true}),
        };
        let result = self.call("Page.captureScreenshot", params).await?;
        result["data"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "CDP 截图缺少数据".into())
    }

    pub async fn insert_text(&self, text: &str) -> Result<(), String> {
        self.call("Input.insertText", json!({"text":text}))
            .await
            .map(|_| ())
    }

    pub async fn key(&self, key: &str) -> Result<(), String> {
        let (code, virtual_key, text) =
            key_mapping(key).ok_or_else(|| "不支持的浏览器按键".to_string())?;
        let mut params = json!({
            "type": "keyDown",
            "key": code,
            "code": code,
            "windowsVirtualKeyCode": virtual_key,
            "nativeVirtualKeyCode": virtual_key
        });
        if let Some(text) = text {
            params["text"] = json!(text);
            params["unmodifiedText"] = json!(text);
        }
        self.call("Input.dispatchKeyEvent", params).await?;
        self.call(
            "Input.dispatchKeyEvent",
            json!({
                "type": "keyUp",
                "key": code,
                "code": code,
                "windowsVirtualKeyCode": virtual_key,
                "nativeVirtualKeyCode": virtual_key
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn enable(&self) -> Result<(), String> {
        self.call("Page.enable", json!({})).await?;
        self.call("Runtime.enable", json!({})).await?;
        Ok(())
    }

    /// 固定模型截图视口，避免 Retina 设备像素和窗口尺寸导致图像过大。
    pub async fn set_viewport(&self, width: u32, height: u32) -> Result<(), String> {
        self.call(
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width": width,
                "height": height,
                "deviceScaleFactor": 1,
                "mobile": false
            }),
        )
        .await?;
        Ok(())
    }
}

/// 仅开放浏览器任务需要的按键；返回 (DOM code, virtual key, text)。
fn key_mapping(key: &str) -> Option<(&'static str, u32, Option<&'static str>)> {
    Some(match key {
        "enter" => ("Enter", 13, Some("\r")),
        "tab" => ("Tab", 9, None),
        "escape" => ("Escape", 27, None),
        "backspace" => ("Backspace", 8, None),
        "arrowleft" => ("ArrowLeft", 37, None),
        "arrowup" => ("ArrowUp", 38, None),
        "arrowright" => ("ArrowRight", 39, None),
        "arrowdown" => ("ArrowDown", 40, None),
        "pageup" => ("PageUp", 33, None),
        "pagedown" => ("PageDown", 34, None),
        "home" => ("Home", 36, None),
        "end" => ("End", 35, None),
        _ => return None,
    })
}
