//! 调用级执行记录；执行结果不等于已交付给模型或客户端。
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Running,
    Succeeded,
    Failed,
    /// 进程中断或任务终止后，没有已提交的回执；不得自动重放副作用。
    OutcomeUnknown,
}

/// 当前仅由内部仓储读取，尚未作为 HTTP 或桌面 IPC 协议公开。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInvocation {
    pub execution_id: String,
    pub task_id: String,
    pub call_id: String,
    pub tool_name: String,
    pub state: ExecutionState,
}
