//! 窄浏览器工具：open_url + CDP 导航/读数/点击/截图。
//! 不提供 shell、任意 JS 或 file://；模型只能操作 http(s) 页面。
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use url::Url;
use yukii_agent_ports::{
    model::{ImageContent, ToolDefinition},
    repository::RepositoryError,
    tools::{ExecutionTarget, Tool, ToolCatalog, ToolContext, ToolEffect},
};

const DEFAULT_CDP: &str = "http://127.0.0.1:9222";
const MAX_TEXT: usize = 20_000;

pub struct BrowserTools;

impl Default for BrowserTools {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserTools {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct UrlInput {
    url: String,
}

#[derive(Deserialize)]
struct ClickInput {
    selector: Option<String>,
    text: Option<String>,
    #[serde(default)]
    index: usize,
}

fn checked_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw.trim()).map_err(|_| "URL 无效")?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("只允许 http/https URL".into());
    }
    Ok(url)
}

fn cdp_base() -> String {
    std::env::var("YUKII_CDP_URL").unwrap_or_else(|_| DEFAULT_CDP.into())
}

async fn cdp_ready(client: &reqwest::Client) -> bool {
    client
        .get(format!("{}/json/version", cdp_base().trim_end_matches('/')))
        .timeout(Duration::from_secs(1))
        .send()
        .await
        .map(|response| response.status().is_success())
        .unwrap_or(false)
}

/// 启动独立的 Chrome 自动化 profile；不碰用户现有 Chrome 配置。
fn launch_automation_chrome() -> Result<(), String> {
    let data_dir = std::env::current_dir()
        .map_err(|_| "无法定位工作目录")?
        .join(".data/chrome-automation");
    std::fs::create_dir_all(&data_dir).map_err(|_| "无法创建 Chrome 自动化目录")?;
    let port = Url::parse(&cdp_base())
        .ok()
        .and_then(|url| url.port())
        .unwrap_or(9222)
        .to_string();
    #[cfg(target_os = "macos")]
    {
        let chrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
        std::process::Command::new(chrome)
            .args([
                &format!("--remote-debugging-port={port}"),
                "--remote-allow-origins=*",
                &format!("--user-data-dir={}", data_dir.to_string_lossy()),
                "--no-first-run",
                "--no-default-browser-check",
                "--no-sandbox",
                "--disable-gpu",
                "--disable-crash-reporter",
                "about:blank",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|_| "无法启动 Chrome")?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (data_dir, port);
        Err("浏览器控制目前仅支持 macOS Chrome".into())
    }
}

struct Cdp {
    socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    next_id: u64,
}

impl Cdp {
    async fn connect() -> Result<Self, String> {
        let client = reqwest::Client::new();
        if !cdp_ready(&client).await {
            launch_automation_chrome()?;
            let mut ready = false;
            for _ in 0..40 {
                tokio::time::sleep(Duration::from_millis(250)).await;
                if cdp_ready(&client).await {
                    ready = true;
                    break;
                }
            }
            if !ready {
                return Err("Chrome 调试端口未就绪（需要 Google Chrome）".into());
            }
        }
        let base = cdp_base();
        let targets: Value = client
            .get(format!("{}/json/list", base.trim_end_matches('/')))
            .send()
            .await
            .map_err(|_| "无法读取 Chrome targets")?
            .json()
            .await
            .map_err(|_| "Chrome targets 协议无效")?;
        let mut ws = targets
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item["type"] == "page")
                    .and_then(|item| item["webSocketDebuggerUrl"].as_str())
            })
            .map(ToOwned::to_owned);
        if ws.is_none() {
            let created: Value = client
                .put(format!(
                    "{}/json/new?about:blank",
                    base.trim_end_matches('/')
                ))
                .send()
                .await
                .map_err(|_| "无法新建 Chrome 页面")?
                .json()
                .await
                .map_err(|_| "Chrome 新页面协议无效")?;
            ws = created["webSocketDebuggerUrl"]
                .as_str()
                .map(ToOwned::to_owned);
        }
        let ws = ws.ok_or("Chrome 没有可控制的页面")?;
        let (socket, _) = connect_async(ws)
            .await
            .map_err(|_| "无法连接 Chrome DevTools")?;
        Ok(Self { socket, next_id: 0 })
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        let request = json!({"id": id, "method": method, "params": params});
        self.socket
            .send(Message::Text(request.to_string().into()))
            .await
            .map_err(|_| "Chrome 连接已断开")?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let message = tokio::time::timeout(remaining, self.socket.next())
                .await
                .map_err(|_| "Chrome 响应超时")?
                .ok_or("Chrome 连接已关闭")?
                .map_err(|_| "Chrome 消息读取失败")?;
            let Message::Text(text) = message else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if value["id"] != id {
                continue;
            }
            if let Some(error) = value.get("error") {
                return Err(error["message"]
                    .as_str()
                    .unwrap_or("Chrome 调用失败")
                    .into());
            }
            return Ok(value["result"].clone());
        }
    }

    async fn evaluate(&mut self, expression: &str) -> Result<Value, String> {
        let result = self
            .call(
                "Runtime.evaluate",
                json!({"expression": expression, "returnByValue": true, "awaitPromise": true}),
            )
            .await?;
        if let Some(exception) = result["exceptionDetails"].as_object() {
            return Err(exception
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("页面脚本失败")
                .into());
        }
        Ok(result["result"]["value"].clone())
    }
}

struct BrowserTool {
    kind: Kind,
}

#[derive(Clone, Copy)]
enum Kind {
    OpenUrl,
    Navigate,
    Read,
    Click,
    Screenshot,
}

#[async_trait]
impl ToolCatalog for BrowserTools {
    async fn tools(&self) -> Result<Vec<Arc<dyn Tool>>, RepositoryError> {
        Ok([
            Kind::OpenUrl,
            Kind::Navigate,
            Kind::Read,
            Kind::Click,
            Kind::Screenshot,
        ]
        .into_iter()
        .map(|kind| Arc::new(BrowserTool { kind }) as Arc<dyn Tool>)
        .collect())
    }
}

#[async_trait]
impl Tool for BrowserTool {
    fn definition(&self) -> ToolDefinition {
        let (name, description, schema) = match self.kind {
            Kind::OpenUrl => (
                "open_url",
                "在 Chrome 中打开一个 http/https 网址；只启动页面，不控制页面。需要点击/读取时用 browser_*。",
                json!({"type":"object","properties":{"url":{"type":"string","maxLength":2000}},"required":["url"],"additionalProperties":false}),
            ),
            Kind::Navigate => (
                "browser_navigate",
                "用受控 Chrome 打开 http/https 页面，并返回标题与 URL。",
                json!({"type":"object","properties":{"url":{"type":"string","maxLength":2000}},"required":["url"],"additionalProperties":false}),
            ),
            Kind::Read => (
                "browser_read",
                "读取当前受控页面的标题、URL 和可见文本（截断），用于理解页面与选择元素。",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            Kind::Click => (
                "browser_click",
                "在当前受控页面点击元素：优先 CSS selector，或按可见文本匹配（可指定 index）。",
                json!({"type":"object","properties":{"selector":{"type":"string","maxLength":500},"text":{"type":"string","maxLength":500},"index":{"type":"integer","minimum":0}},"additionalProperties":false}),
            ),
            Kind::Screenshot => (
                "browser_screenshot",
                "截取当前受控页面；只在需要视觉确认时使用，普通读取优先 browser_read。",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
        };
        ToolDefinition {
            name: name.into(),
            description: description.into(),
            schema,
        }
    }

    fn execution_target(&self) -> ExecutionTarget {
        ExecutionTarget::CloudOffice
    }

    async fn execute(&self, _: &ToolContext, arguments: Value) -> Result<ToolEffect, String> {
        match self.kind {
            Kind::OpenUrl => {
                let input: UrlInput =
                    serde_json::from_value(arguments).map_err(|_| "URL 参数无效")?;
                let url = checked_url(&input.url)?;
                let status = std::process::Command::new("open")
                    .args(["-a", "Google Chrome", url.as_str()])
                    .status()
                    .map_err(|_| "无法调用 Google Chrome")?;
                if !status.success() {
                    return Err("Chrome 打开网址失败".into());
                }
                Ok(ToolEffect::Result(json!({"opened": url.as_str()})))
            }
            Kind::Navigate => {
                let input: UrlInput =
                    serde_json::from_value(arguments).map_err(|_| "URL 参数无效")?;
                let url = checked_url(&input.url)?;
                let mut cdp = Cdp::connect().await?;
                cdp.call("Page.enable", json!({})).await?;
                cdp.call("Page.navigate", json!({"url": url.as_str()}))
                    .await?;
                tokio::time::sleep(Duration::from_millis(900)).await;
                let info = cdp
                    .evaluate("({title:document.title,url:location.href})")
                    .await?;
                Ok(ToolEffect::Result(info))
            }
            Kind::Read => {
                let mut cdp = Cdp::connect().await?;
                let info = cdp
                    .evaluate(&format!(
                        "(()=>{{const t=(document.body?.innerText||'').slice(0,{MAX_TEXT});return {{title:document.title,url:location.href,text:t}}}})()"
                    ))
                    .await?;
                Ok(ToolEffect::Result(info))
            }
            Kind::Click => {
                let input: ClickInput =
                    serde_json::from_value(arguments).map_err(|_| "点击参数无效")?;
                let selector = input.selector.filter(|value| !value.trim().is_empty());
                let text = input.text.filter(|value| !value.trim().is_empty());
                if selector.is_none() && text.is_none() {
                    return Err("需要 selector 或 text".into());
                }
                let mut cdp = Cdp::connect().await?;
                let expression = if let Some(selector) = selector {
                    let literal = serde_json::to_string(&selector).map_err(|_| "selector 无效")?;
                    format!(
                        "(()=>{{const e=document.querySelector({literal});if(!e)return {{clicked:false,reason:'not_found'}};e.scrollIntoView({{block:'center'}});e.click();return {{clicked:true,tag:e.tagName,text:(e.innerText||e.getAttribute('aria-label')||'').slice(0,200)}}}})()"
                    )
                } else {
                    let literal = serde_json::to_string(text.as_deref().unwrap_or(""))
                        .map_err(|_| "text 无效")?;
                    format!(
                        "(()=>{{const wanted={literal};const nodes=[...document.querySelectorAll('a,button,[role=button],input')];const hits=nodes.filter(e=>((e.innerText||e.value||e.getAttribute('aria-label')||'').includes(wanted)));const e=hits[{}];if(!e)return {{clicked:false,reason:'not_found',candidates:hits.length}};e.scrollIntoView({{block:'center'}});e.click();return {{clicked:true,tag:e.tagName,text:(e.innerText||e.value||e.getAttribute('aria-label')||'').slice(0,200),candidates:hits.length}}}})()",
                        input.index
                    )
                };
                let result = cdp.evaluate(&expression).await?;
                tokio::time::sleep(Duration::from_millis(500)).await;
                Ok(ToolEffect::Result(result))
            }
            Kind::Screenshot => {
                let mut cdp = Cdp::connect().await?;
                let result = cdp
                    .call("Page.captureScreenshot", json!({"format":"png"}))
                    .await?;
                let data = result["data"].as_str().ok_or("截图没有返回内容")?;
                Ok(ToolEffect::ResultWithImages {
                    value: json!({"captured": true}),
                    images: vec![ImageContent {
                        media_type: "image/png".into(),
                        base64: data.to_owned(),
                        width: 0,
                        height: 0,
                    }],
                })
            }
        }
    }
}
