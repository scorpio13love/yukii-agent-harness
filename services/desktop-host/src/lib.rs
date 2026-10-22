//! 受信桌面宿主库：窗口选择、会话同意、动作审批、Executor 监督与 IPC 客户端。
//! 该库不提供 HTTP 路由，不被网页入口引用。
pub mod approval;
pub mod browser;
pub mod host;
pub mod ipc;
pub mod preview;
