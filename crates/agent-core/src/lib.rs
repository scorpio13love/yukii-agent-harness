//! 模型与存储无关的 Agent 执行循环。
pub mod runner;
pub use yukii_agent_domain::{TaskError, transition, validate_task};
