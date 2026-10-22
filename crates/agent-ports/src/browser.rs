//! 浏览器控制端口。实现可以使用 CDP；工具层只依赖此抽象。
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserElement {
    /// 由最近一次观察生成，页面重渲染或重新观察后可能变化。
    pub id: String,
    pub tag: String,
    pub role: String,
    pub text: String,
    pub input_type: String,
    pub href: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserObservation {
    pub url: String,
    pub title: String,
    /// 页面可见文本，已截断；属于不可信页面内容。
    pub text: String,
    pub elements: Vec<BrowserElement>,
    pub screenshot_base64: String,
    pub width: u32,
    pub height: u32,
}

/// 受控浏览器的最小控制面。实现负责 CDP/进程生命周期；工具不直接持有 Chrome。
#[async_trait]
pub trait BrowserControl: Send + Sync {
    async fn observe(&self) -> Result<BrowserObservation, String>;
    async fn navigate(&self, url: &str) -> Result<BrowserObservation, String>;
    async fn click(&self, element_id: &str) -> Result<(), String>;
    async fn type_text(&self, element_id: &str, text: &str, clear: bool) -> Result<(), String>;
    async fn press_key(&self, key: &str) -> Result<(), String>;
    async fn scroll(&self, delta_y: i32) -> Result<(), String>;
}
