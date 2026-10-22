//! 本机桌面执行模块。由受信宿主注入权限仓储与原生桥，不开放 HTTP。
//! 文件能力针对 Unix；Computer Use 的跨平台策略与 Unix 文件实现分开。
mod computer;
#[cfg(unix)]
mod files;
#[cfg(unix)]
mod unix;
pub use computer::{
    ComputerExecutionError, ComputerExecutor, MAX_LISTED_WINDOWS, MAX_PNG_BYTES,
    MAX_RETAINED_OBSERVATIONS, NativeBridge, NativeCapture, NativeError,
};
#[cfg(unix)]
pub use files::{FileExecutionError, FileExecutor, FileOutcome, MAX_FILE_BYTES, content_digest};
