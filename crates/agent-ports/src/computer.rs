//! 桌面执行端的审计端口。执行规则留在 Domain，SQLite 细节留在 Infra。
use thiserror::Error;
use yukii_contracts::desktop::{ControlSession, ControlState, DesktopObservation, GuiAction};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ComputerRepositoryError {
    #[error("桌面控制记录不存在或状态不允许")]
    ControlUnavailable,
    #[error("桌面观察已过期、已使用或不属于当前控制会话")]
    ObservationUnavailable,
    #[error("任务已结束，不能再派发 GUI 动作")]
    TaskInactive,
    #[error("动作记录状态冲突")]
    Conflict,
    #[error("桌面审计存储不可用")]
    Backend,
}

/// 执行端在产生外部副作用前登记 intent，派发后再写终态。
/// 该端口不负责截图内容；像素只存在于执行端内存与受信宿主之间。
pub trait ComputerRepository: Send + Sync {
    fn start_session(
        &self,
        session_id: &str,
        control: &ControlSession,
    ) -> Result<(), ComputerRepositoryError>;
    fn set_session_state(
        &self,
        control_id: &str,
        state: ControlState,
    ) -> Result<ControlSession, ComputerRepositoryError>;
    fn record_observation(
        &self,
        control_id: &str,
        observation: &DesktopObservation,
        image_sha256: &str,
    ) -> Result<(), ComputerRepositoryError>;
    fn begin_action(
        &self,
        control_id: &str,
        observation_id: &str,
        task_id: &str,
        call_id: &str,
        action: &GuiAction,
        action_id: &str,
    ) -> Result<(), ComputerRepositoryError>;
    fn finish_action(
        &self,
        action_id: &str,
        state: &str,
        result: Option<&str>,
    ) -> Result<(), ComputerRepositoryError>;
    /// 执行进程启动时结束上一个进程留下的 active/paused 会话，绝不自动恢复控制。
    fn close_stale_sessions(&self, session_id: &str) -> Result<usize, ComputerRepositoryError>;
}
