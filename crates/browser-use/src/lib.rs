//! Browser Use：受控 Chrome 启动器、CDP 客户端与 browser_* 工具。
mod cdp;
mod session;
mod tools;

pub use session::{BrowserConfig, BrowserSession};
pub use tools::BrowserTools;
