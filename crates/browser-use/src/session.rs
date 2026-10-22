//! 受控 Chrome 会话：启动/连接 CDP、页面快照、导航与输入动作。
use crate::cdp::CdpClient;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::sleep;
use url::Url;
use yukii_agent_ports::browser::{BrowserControl, BrowserElement, BrowserObservation};

const MAX_SCREENSHOT_BASE64: usize = 7 * 1024 * 1024;
const MAX_PAGE_TEXT: usize = 12_000;
const MAX_ELEMENTS: usize = 200;

pub struct BrowserConfig {
    pub path: PathBuf,
    pub profile_dir: PathBuf,
    pub initial_url: Option<String>,
    /// 仅用于测试环境（Chrome 在受限沙箱中无法初始化自己的 sandbox）；生产默认关闭。
    pub no_sandbox: bool,
}

pub struct BrowserSession {
    client: Arc<CdpClient>,
    endpoint: String,
    child: Mutex<Option<Child>>,
    launched: bool,
}

impl BrowserSession {
    /// 连接已有 CDP 端点。target_hint 用于选择包含该 URL 的页面。
    pub async fn connect(endpoint: &str, target_hint: Option<&str>) -> Result<Self, String> {
        let endpoint = endpoint.trim_end_matches('/').to_owned();
        let list: Value = reqwest::Client::new()
            .get(format!("{endpoint}/json/list"))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|error| format!("无法读取 CDP 目标列表：{error}"))?
            .json()
            .await
            .map_err(|error| format!("CDP 目标列表格式无效：{error}"))?;
        let pages = list
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter(|item| item["type"] == "page")
                    .filter(|item| item["webSocketDebuggerUrl"].is_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let chosen = target_hint
            .and_then(|hint| {
                pages.iter().copied().find(|item| {
                    item["url"]
                        .as_str()
                        .is_some_and(|url| url.contains(hint) || url == hint)
                })
            })
            .or_else(|| {
                pages.iter().copied().find(|item| {
                    item["url"]
                        .as_str()
                        .is_some_and(|url| !url.starts_with("chrome://") && url != "about:blank")
                })
            })
            .or_else(|| pages.first().copied())
            .ok_or_else(|| "CDP 端点没有可用的 page 目标".to_string())?;
        let ws_url = chosen["webSocketDebuggerUrl"]
            .as_str()
            .ok_or_else(|| "CDP page 目标缺少 WebSocket 地址".to_string())?;
        let client = Arc::new(CdpClient::connect(ws_url).await?);
        client.enable().await?;
        // 浏览器任务使用固定 CSS 视口；模型截图稳定在 1440x900 以内。
        client.set_viewport(1440, 900).await?;
        Ok(Self {
            client,
            endpoint,
            child: Mutex::new(None),
            launched: false,
        })
    }

    /// 启动独立 profile 的 Chrome；不会读取用户现有 Chrome profile。
    pub async fn launch(config: BrowserConfig) -> Result<Self, String> {
        if !config.path.is_file() {
            return Err(format!("找不到 Chrome/Chromium：{}", config.path.display()));
        }
        fs::create_dir_all(&config.profile_dir)
            .map_err(|error| format!("无法创建浏览器 profile：{error}"))?;
        let log_path = config.profile_dir.join("chrome.log");
        let log = fs::File::create(&log_path)
            .map_err(|error| format!("无法创建 Chrome 日志：{error}"))?;
        let log_err = log
            .try_clone()
            .map_err(|error| format!("无法复制 Chrome 日志句柄：{error}"))?;
        let initial_url = config
            .initial_url
            .as_deref()
            .map(normalize_url)
            .transpose()?
            .unwrap_or_else(|| "about:blank".into());
        let mut command = Command::new(&config.path);
        command
            .arg("--user-data-dir")
            .arg(&config.profile_dir)
            .arg("--remote-debugging-port=0")
            .arg("--remote-allow-origins=*")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-extensions")
            .arg("--new-window")
            .arg(&initial_url)
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(log_err);
        if config.no_sandbox {
            command.arg("--no-sandbox");
        }
        let child = command
            .spawn()
            .map_err(|error| format!("无法启动 Chrome：{error}"))?;
        let mut child = Some(child);
        let port_file = config.profile_dir.join("DevToolsActivePort");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let port = loop {
            if let Ok(text) = fs::read_to_string(&port_file)
                && let Some(port) = text
                    .lines()
                    .next()
                    .and_then(|line| line.trim().parse::<u16>().ok())
            {
                break port;
            }
            if let Some(process) = child.as_mut()
                && let Ok(Some(status)) = process.try_wait()
            {
                return Err(format!(
                    "Chrome 在 CDP 就绪前退出（{status}）；日志：{}",
                    log_path.display()
                ));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "等待 Chrome CDP 超时；日志：{}",
                    log_path.display()
                ));
            }
            sleep(Duration::from_millis(150)).await;
        };
        let endpoint = format!("http://127.0.0.1:{port}");
        let mut session = Self::connect(&endpoint, Some(&initial_url)).await?;
        if initial_url != "about:blank" {
            let _ = session.navigate(&initial_url).await;
        }
        *session
            .child
            .get_mut()
            .map_err(|_| "浏览器进程状态不可用")? = child;
        session.launched = true;
        Ok(session)
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub async fn observe(&self) -> Result<BrowserObservation, String> {
        let snapshot = self.client.evaluate(SNAPSHOT_JS).await?;
        let url = snapshot["url"].as_str().unwrap_or_default().to_owned();
        let title = snapshot["title"].as_str().unwrap_or_default().to_owned();
        let text = truncate_chars(snapshot["text"].as_str().unwrap_or_default(), MAX_PAGE_TEXT);
        let elements: Vec<BrowserElement> = serde_json::from_value(snapshot["elements"].clone())
            .map_err(|_| "页面元素快照格式无效".to_string())?;
        let viewport = (
            snapshot["viewport"]["width"].as_u64().unwrap_or(0) as u32,
            snapshot["viewport"]["height"].as_u64().unwrap_or(0) as u32,
        );
        let screenshot = self.client.screenshot(Some(viewport)).await?;
        if screenshot.len() > MAX_SCREENSHOT_BASE64 {
            return Err("页面截图超过大小上限".into());
        }
        let png = STANDARD
            .decode(&screenshot)
            .map_err(|_| "页面截图不是有效 base64".to_string())?;
        let (width, height) =
            png_dimensions(&png).ok_or_else(|| "页面截图不是有效 PNG".to_string())?;
        Ok(BrowserObservation {
            url,
            title,
            text,
            elements: elements.into_iter().take(MAX_ELEMENTS).collect(),
            screenshot_base64: screenshot,
            width,
            height,
        })
    }

    pub async fn navigate(&self, raw_url: &str) -> Result<BrowserObservation, String> {
        let url = normalize_url(raw_url)?;
        self.client.navigate(&url).await?;
        self.wait_ready().await?;
        self.observe().await
    }

    async fn wait_ready(&self) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(Value::String(state)) = self.client.evaluate("document.readyState").await
                && (state == "complete" || state == "interactive")
            {
                sleep(Duration::from_millis(300)).await;
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("等待页面加载超时".into());
            }
            sleep(Duration::from_millis(150)).await;
        }
    }

    pub async fn click(&self, element_id: &str) -> Result<(), String> {
        let selector = element_selector(element_id)?;
        let expression = format!(
            r#"(() => {{
                const el = document.querySelector('{selector}');
                if (!el) return false;
                el.scrollIntoView({{block:'center'}});
                el.click();
                return true;
            }})()"#
        );
        if self.client.evaluate(&expression).await? != Value::Bool(true) {
            return Err("页面元素不存在或已失效，请重新观察".into());
        }
        sleep(Duration::from_millis(300)).await;
        Ok(())
    }

    pub async fn type_text(&self, element_id: &str, text: &str, clear: bool) -> Result<(), String> {
        if text.is_empty() || text.chars().count() > 2000 {
            return Err("输入文本为空或过长".into());
        }
        let selector = element_selector(element_id)?;
        let expression = format!(
            r#"(() => {{
                const el = document.querySelector('{selector}');
                if (!el) return false;
                el.scrollIntoView({{block:'center'}});
                el.focus();
                const clear = {clear};
                if (clear && typeof el.select === 'function') {{
                    try {{ el.select(); }} catch (_) {{}}
                }} else if (clear && el.isContentEditable) {{
                    const range = document.createRange();
                    range.selectNodeContents(el);
                    const selection = getSelection();
                    selection.removeAllRanges();
                    selection.addRange(range);
                }} else if (!clear && typeof el.setSelectionRange === 'function' && typeof el.value === 'string') {{
                    el.setSelectionRange(el.value.length, el.value.length);
                }}
                return true;
            }})()"#
        );
        if self.client.evaluate(&expression).await? != Value::Bool(true) {
            return Err("页面元素不存在或无法聚焦，请重新观察".into());
        }
        self.client.insert_text(text).await?;
        sleep(Duration::from_millis(120)).await;
        Ok(())
    }

    pub async fn press_key(&self, key: &str) -> Result<(), String> {
        self.client.key(key).await
    }

    pub async fn scroll(&self, delta_y: i32) -> Result<(), String> {
        let delta = delta_y.clamp(-5000, 5000);
        self.client
            .evaluate(&format!(
                "window.scrollBy({{top:{delta},left:0,behavior:'instant'}}); true"
            ))
            .await?;
        sleep(Duration::from_millis(150)).await;
        Ok(())
    }

    /// 只终止本进程启动的 Chrome；连接已有端点时保持用户浏览器不变。
    pub fn close(&self) {
        if !self.launched {
            return;
        }
        if let Ok(mut child) = self.child.lock()
            && let Some(mut child) = child.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[async_trait::async_trait]
impl BrowserControl for BrowserSession {
    async fn observe(&self) -> Result<BrowserObservation, String> {
        BrowserSession::observe(self).await
    }

    async fn navigate(&self, url: &str) -> Result<BrowserObservation, String> {
        BrowserSession::navigate(self, url).await
    }

    async fn click(&self, element_id: &str) -> Result<(), String> {
        BrowserSession::click(self, element_id).await
    }

    async fn type_text(&self, element_id: &str, text: &str, clear: bool) -> Result<(), String> {
        BrowserSession::type_text(self, element_id, text, clear).await
    }

    async fn press_key(&self, key: &str) -> Result<(), String> {
        BrowserSession::press_key(self, key).await
    }

    async fn scroll(&self, delta_y: i32) -> Result<(), String> {
        BrowserSession::scroll(self, delta_y).await
    }
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        if self.launched
            && let Ok(mut child) = self.child.lock()
            && let Some(mut child) = child.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn element_selector(element_id: &str) -> Result<String, String> {
    let valid = element_id.strip_prefix("el-").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.len() <= 4 && suffix.chars().all(|c| c.is_ascii_digit())
    });
    if !valid {
        return Err("元素 ID 无效".into());
    }
    Ok(format!("[data-yukii-id=\"{element_id}\"]"))
}

fn normalize_url(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 2048 {
        return Err("URL 为空或过长".into());
    }
    let candidate = if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_owned()
    } else if raw.contains("://") {
        return Err("只允许 http/https URL".into());
    } else {
        format!("https://{raw}")
    };
    let url = Url::parse(&candidate).map_err(|_| "URL 格式无效".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("只允许 http/https URL".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URL 不能包含用户名或密码".into());
    }
    Ok(url.to_string())
}

fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

fn truncate_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

const SNAPSHOT_JS: &str = r#"(() => {
    const selector = 'a,button,input,textarea,select,[role="button"],[role="link"],[role="textbox"],[contenteditable="true"]';
    const visible = (el) => {
        const rect = el.getBoundingClientRect();
        const style = getComputedStyle(el);
        return rect.width > 0 && rect.height > 0 && style.visibility !== 'hidden' && style.display !== 'none';
    };
    const elements = Array.from(document.querySelectorAll(selector))
        .filter(visible)
        .slice(0, 200)
        .map((el, index) => {
            el.setAttribute('data-yukii-id', 'el-' + index);
            const type = (el.getAttribute('type') || '').toLowerCase();
            return {
                id: 'el-' + index,
                tag: el.tagName.toLowerCase(),
                role: el.getAttribute('role') || '',
                text: (el.innerText || el.getAttribute('aria-label') || el.getAttribute('placeholder') || '').trim().slice(0, 160),
                input_type: type,
                href: el.href ? String(el.href).slice(0, 300) : '',
                value: type === 'password' ? '' : (typeof el.value === 'string' ? el.value.slice(0, 160) : '')
            };
        });
    return {
        url: location.href,
        title: document.title,
        text: document.body ? document.body.innerText.slice(0, 12000) : '',
        elements,
        viewport: {width: window.innerWidth, height: window.innerHeight}
    };
})()"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_only_http_urls_and_rejects_credentials() {
        assert_eq!(
            normalize_url("example.com").unwrap(),
            "https://example.com/"
        );
        assert_eq!(
            normalize_url("http://example.com/a").unwrap(),
            "http://example.com/a"
        );
        for value in [
            "",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://user:pass@example.com",
        ] {
            assert!(normalize_url(value).is_err(), "{value}");
        }
    }

    #[test]
    fn element_ids_are_bounded_and_selector_safe() {
        assert_eq!(
            element_selector("el-12").unwrap(),
            "[data-yukii-id=\"el-12\"]"
        );
        for value in ["", "el-", "el-12345", "el-1;alert(1)", "x"] {
            assert!(element_selector(value).is_err(), "{value}");
        }
    }

    #[test]
    fn png_dimensions_are_read_from_ihdr() {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&560u32.to_be_bytes());
        bytes.extend_from_slice(&332u32.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        assert_eq!(png_dimensions(&bytes), Some((560, 332)));
        assert_eq!(png_dimensions(b"not png"), None);
    }
}
