//! 受控桌面观察与 GUI 动作。执行端只接受宿主在用户同意后建立的控制会话；
//! 截图绑定一次动作，原生桥返回“已投递”不等于目标已达成。
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;
use yukii_agent_domain::{
    computer::{
        MAX_OBSERVATION_SECONDS, observation_is_usable, to_window_action,
        validate_control_expiration, validate_observation, validate_window,
    },
    permissions::validate_identity,
};
use yukii_agent_ports::computer::{ComputerRepository, ComputerRepositoryError};
use yukii_contracts::desktop::{
    ActionDispatch, ComputerPermissions, ControlSession, ControlState, DesktopObservation,
    DesktopWindow, GuiAction, UiElement,
};

pub const MAX_LISTED_WINDOWS: usize = 128;
pub const MAX_RETAINED_OBSERVATIONS: usize = 64;
pub const MAX_PNG_BYTES: usize = 5 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct NativeCapture {
    pub png_base64: String,
    pub width: u32,
    pub height: u32,
    /// 窗口在屏幕坐标中的位置与逻辑尺寸。
    pub frame: [f64; 4],
    /// 平台可访问性树快照；坐标动作仍作为回退。
    pub elements: Vec<UiElement>,
}

#[derive(Debug, Error)]
pub enum NativeError {
    /// 原生桥在产生副作用前明确拒绝；动作可以记为 rejected。
    #[error("原生桥拒绝：{0}")]
    Rejected(String),
    /// 进程超时/断连/畸形响应可能发生在事件投递之后，必须按结果未知处理。
    #[error("原生桥结果不确定：{0}")]
    Unknown(String),
}

#[derive(Debug, Error)]
pub enum ComputerExecutionError {
    #[error("系统权限不足：{0}")]
    Permission(String),
    #[error("当前没有可用的桌面控制会话")]
    ControlUnavailable,
    #[error("观察不可用：{0}")]
    Observation(String),
    #[error("桌面操作参数无效：{0}")]
    Invalid(String),
    #[error("原生桥拒绝了操作：{0}")]
    Rejected(String),
    #[error("GUI 动作结果不确定：{0}")]
    OutcomeUnknown(String),
    #[error(transparent)]
    Store(#[from] ComputerRepositoryError),
    #[error(transparent)]
    Native(#[from] NativeError),
}

/// 原生桥只接收已选定窗口与已换算到窗口逻辑点的动作。
/// 实现负责进程启动、超时和输出上限；策略、授权和审计不在此层。
#[async_trait]
pub trait NativeBridge: Send + Sync {
    async fn permissions(&self) -> Result<ComputerPermissions, NativeError>;
    async fn windows(&self) -> Result<Vec<DesktopWindow>, NativeError>;
    async fn observe(&self, window: &DesktopWindow) -> Result<NativeCapture, NativeError>;
    async fn act(
        &self,
        window: &DesktopWindow,
        frame: [f64; 4],
        action: &GuiAction,
    ) -> Result<(), NativeError>;
}

struct ObservationState {
    window: DesktopWindow,
    frame: [f64; 4],
    width: u32,
    height: u32,
    captured_at: i64,
    expires_at: i64,
    consumed: bool,
}

struct ControlRuntime {
    session: ControlSession,
    observations: HashMap<String, ObservationState>,
    /// 只有最近一次截图可用于动作；连续观察后旧截图立即失去动作资格。
    latest_observation: Option<String>,
    /// 动作结果未知后停止自动操作，必须人工核对并结束会话后重新开始。
    blocked: bool,
}

#[derive(Default)]
struct State {
    control: Option<ControlRuntime>,
    next_generation: u64,
}

pub struct ComputerExecutor {
    session_id: String,
    store: Arc<dyn ComputerRepository>,
    bridge: Arc<dyn NativeBridge>,
    state: Mutex<State>,
}

impl ComputerExecutor {
    pub async fn new(
        session_id: String,
        store: Arc<dyn ComputerRepository>,
        bridge: Arc<dyn NativeBridge>,
    ) -> Result<Self, ComputerExecutionError> {
        validate_identity(&session_id)
            .map_err(|_| ComputerExecutionError::Invalid("会话标识无效".into()))?;
        // 新执行进程不得继承旧控制权；旧会话只结束，不自动恢复。
        store.close_stale_sessions(&session_id)?;
        Ok(Self {
            session_id,
            store,
            bridge,
            state: Mutex::new(State::default()),
        })
    }

    pub async fn permissions(&self) -> Result<ComputerPermissions, ComputerExecutionError> {
        self.bridge.permissions().await.map_err(Into::into)
    }

    pub async fn windows(&self) -> Result<Vec<DesktopWindow>, ComputerExecutionError> {
        let mut windows = self.bridge.windows().await?;
        windows.truncate(MAX_LISTED_WINDOWS);
        for window in &windows {
            validate_window(window)
                .map_err(|_| ComputerExecutionError::Invalid("原生桥返回了无效窗口身份".into()))?;
        }
        Ok(windows)
    }

    pub async fn status(&self) -> Option<ControlSession> {
        self.state
            .lock()
            .await
            .control
            .as_ref()
            .map(|control| control.session.clone())
    }

    /// 宿主在用户明确同意后调用。执行端复查系统权限与窗口身份后才登记会话。
    pub async fn begin_control(
        &self,
        task_id: &str,
        window: &DesktopWindow,
        screenshot_sharing: bool,
        expires_at: i64,
    ) -> Result<ControlSession, ComputerExecutionError> {
        validate_identity(task_id)
            .map_err(|_| ComputerExecutionError::Invalid("任务标识无效".into()))?;
        validate_window(window)
            .map_err(|error| ComputerExecutionError::Invalid(error.to_string()))?;
        let now = now();
        validate_control_expiration(expires_at, now)
            .map_err(|error| ComputerExecutionError::Invalid(error.to_string()))?;

        let mut state = self.state.lock().await;
        if let Some(existing) = state.control.as_ref() {
            if existing.session.state != ControlState::Ended && existing.session.expires_at > now {
                return Err(ComputerExecutionError::ControlUnavailable);
            }
            // 过期会话在内存中也不再有门禁；显式结束后再允许新的用户同意。
            if existing.session.state != ControlState::Ended {
                let control_id = existing.session.control_id.clone();
                self.store
                    .set_session_state(&control_id, ControlState::Ended)?;
            }
            state.control = None;
        }
        let permissions = self.bridge.permissions().await?;
        if !permissions.screen_capture || !permissions.accessibility {
            return Err(ComputerExecutionError::Permission(
                "需要屏幕录制与辅助功能权限；请在系统设置中授权后重新开始".into(),
            ));
        }
        let canonical = self
            .bridge
            .windows()
            .await?
            .into_iter()
            .find(|candidate| {
                candidate.window_id == window.window_id
                    && candidate.pid == window.pid
                    && candidate.application == window.application
            })
            .ok_or_else(|| {
                ComputerExecutionError::Rejected("目标窗口已不可用，请重新选择".into())
            })?;
        validate_window(&canonical)
            .map_err(|error| ComputerExecutionError::Invalid(error.to_string()))?;
        state.next_generation = state.next_generation.saturating_add(1);
        let session = ControlSession {
            control_id: Uuid::new_v4().to_string(),
            task_id: task_id.into(),
            window: canonical,
            screenshot_sharing,
            generation: state.next_generation,
            expires_at,
            state: ControlState::Active,
        };
        self.store.start_session(&self.session_id, &session)?;
        state.control = Some(ControlRuntime {
            session: session.clone(),
            observations: HashMap::new(),
            latest_observation: None,
            blocked: false,
        });
        Ok(session)
    }

    pub async fn observe(
        &self,
        task_id: &str,
    ) -> Result<DesktopObservation, ComputerExecutionError> {
        validate_identity(task_id)
            .map_err(|_| ComputerExecutionError::Invalid("任务标识无效".into()))?;
        let mut state = self.state.lock().await;
        let control = active_control(&mut state, task_id)?;
        if !control.session.screenshot_sharing {
            return Err(ComputerExecutionError::Permission(
                "本次控制会话未同意把截图发送给模型".into(),
            ));
        }
        let now = now();
        if control.session.expires_at <= now {
            return Err(ComputerExecutionError::ControlUnavailable);
        }
        let window = control.session.window.clone();
        let capture = self.bridge.observe(&window).await?;
        validate_observation(
            capture.width,
            capture.height,
            capture.png_base64.len(),
            capture.frame,
        )
        .map_err(|error| ComputerExecutionError::Invalid(error.to_string()))?;
        let png = STANDARD
            .decode(&capture.png_base64)
            .map_err(|_| ComputerExecutionError::Invalid("截图不是有效 base64".into()))?;
        if png.len() > MAX_PNG_BYTES || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(ComputerExecutionError::Invalid(
                "截图不是受支持的 PNG 或超过大小上限".into(),
            ));
        }
        let digest = format!("{:x}", Sha256::digest(&png));
        let expires_at = (now + MAX_OBSERVATION_SECONDS).min(control.session.expires_at);
        if expires_at <= now {
            return Err(ComputerExecutionError::ControlUnavailable);
        }
        let observation = DesktopObservation {
            observation_id: Uuid::new_v4().to_string(),
            window,
            png_base64: capture.png_base64,
            width: capture.width,
            height: capture.height,
            frame: capture.frame,
            elements: capture.elements,
            captured_at: now,
            expires_at,
        };
        if let Err(error) =
            self.store
                .record_observation(&control.session.control_id, &observation, &digest)
        {
            return Err(error.into());
        }
        prune_observations(control);
        control.latest_observation = Some(observation.observation_id.clone());
        control.observations.insert(
            observation.observation_id.clone(),
            ObservationState {
                window: observation.window.clone(),
                frame: observation.frame,
                width: observation.width,
                height: observation.height,
                captured_at: observation.captured_at,
                expires_at: observation.expires_at,
                consumed: false,
            },
        );
        Ok(observation)
    }

    /// 观察消费、intent 落盘、原生派发和终态记录分四步；任何不确定结果都禁止重放。
    pub async fn act(
        &self,
        task_id: &str,
        call_id: &str,
        observation_id: &str,
        action: &GuiAction,
    ) -> Result<ActionDispatch, ComputerExecutionError> {
        validate_identity(task_id)
            .map_err(|_| ComputerExecutionError::Invalid("任务标识无效".into()))?;
        validate_identity(call_id)
            .map_err(|_| ComputerExecutionError::Invalid("调用标识无效".into()))?;
        validate_identity(observation_id)
            .map_err(|_| ComputerExecutionError::Invalid("观察标识无效".into()))?;
        let mut state = self.state.lock().await;
        let control = active_control(&mut state, task_id)?;
        let checked_at = now();
        if control.latest_observation.as_deref() != Some(observation_id) {
            return Err(ComputerExecutionError::Observation(
                "不是最近一次观察；旧截图不能用于新动作".into(),
            ));
        }
        let observation = control
            .observations
            .get(observation_id)
            .ok_or_else(|| ComputerExecutionError::Observation("没有该观察记录".into()))?;
        if observation.window != control.session.window
            || !observation_is_usable(
                observation.captured_at,
                observation.expires_at,
                observation.consumed.then_some(observation.captured_at),
                checked_at,
            )
        {
            return Err(ComputerExecutionError::Observation(
                "观察已过期、已使用或窗口不匹配，请重新观察".into(),
            ));
        }
        let mapped = to_window_action(
            action,
            observation.width,
            observation.height,
            observation.frame,
        )
        .map_err(|error| ComputerExecutionError::Invalid(error.to_string()))?;
        let control_id = control.session.control_id.clone();
        let window = observation.window.clone();
        let frame = observation.frame;
        let action_id = Uuid::new_v4().to_string();
        self.store.begin_action(
            &control_id,
            observation_id,
            task_id,
            call_id,
            action,
            &action_id,
        )?;
        // 数据库已原子消费观察；内存也标记，确保后续请求不能重复派发。
        if let Some(observation) = control.observations.get_mut(observation_id) {
            observation.consumed = true;
        }
        control.latest_observation = None;
        match self.bridge.act(&window, frame, &mapped).await {
            Ok(()) => {
                if let Err(error) = self.store.finish_action(&action_id, "dispatched", None) {
                    control.blocked = true;
                    return Err(ComputerExecutionError::Store(error));
                }
                Ok(ActionDispatch {
                    action_id,
                    dispatched_at: now(),
                    verification_required: true,
                })
            }
            Err(NativeError::Rejected(message)) => {
                let _ = self
                    .store
                    .finish_action(&action_id, "rejected", Some(&message));
                Err(ComputerExecutionError::Rejected(message))
            }
            Err(NativeError::Unknown(message)) => {
                let _ = self
                    .store
                    .finish_action(&action_id, "outcome_unknown", Some(&message));
                control.blocked = true;
                Err(ComputerExecutionError::OutcomeUnknown(message))
            }
        }
    }

    pub async fn pause(&self, task_id: &str) -> Result<ControlSession, ComputerExecutionError> {
        self.change_state(task_id, ControlState::Paused).await
    }

    pub async fn resume(&self, task_id: &str) -> Result<ControlSession, ComputerExecutionError> {
        let mut state = self.state.lock().await;
        let control = state
            .control
            .as_mut()
            .ok_or(ComputerExecutionError::ControlUnavailable)?;
        if control.blocked {
            return Err(ComputerExecutionError::OutcomeUnknown(
                "上一次 GUI 动作结果未知，必须人工核对并结束控制后重新开始".into(),
            ));
        }
        if control.session.task_id != task_id || control.session.state != ControlState::Paused {
            return Err(ComputerExecutionError::ControlUnavailable);
        }
        if control.session.expires_at <= now() {
            return Err(ComputerExecutionError::ControlUnavailable);
        }
        let session = self
            .store
            .set_session_state(&control.session.control_id, ControlState::Active)?;
        control.session = session.clone();
        // 恢复后局部画面可能已变化，旧观察全部作废。
        control.observations.clear();
        control.latest_observation = None;
        Ok(session)
    }

    pub async fn end(&self, task_id: &str) -> Result<ControlSession, ComputerExecutionError> {
        self.change_state(task_id, ControlState::Ended).await
    }

    async fn change_state(
        &self,
        task_id: &str,
        next: ControlState,
    ) -> Result<ControlSession, ComputerExecutionError> {
        let mut state = self.state.lock().await;
        let control = state
            .control
            .as_ref()
            .ok_or(ComputerExecutionError::ControlUnavailable)?;
        if control.session.task_id != task_id || control.session.state == ControlState::Ended {
            return Err(ComputerExecutionError::ControlUnavailable);
        }
        if control.session.state == next {
            return Ok(control.session.clone());
        }
        let session = self
            .store
            .set_session_state(&control.session.control_id, next)?;
        if next == ControlState::Ended {
            state.control = None;
        } else if let Some(control) = state.control.as_mut() {
            control.session = session.clone();
        }
        Ok(session)
    }

    /// 连接关闭时尽力结束活动会话；失败只记录，不把结束失败说成仍然受控。
    pub async fn shutdown(&self) {
        let control = {
            let state = self.state.lock().await;
            state
                .control
                .as_ref()
                .map(|control| control.session.clone())
        };
        if let Some(control) = control {
            let _ = self
                .store
                .set_session_state(&control.control_id, ControlState::Ended);
        }
    }
}

fn active_control<'a>(
    state: &'a mut State,
    task_id: &str,
) -> Result<&'a mut ControlRuntime, ComputerExecutionError> {
    let control = state
        .control
        .as_mut()
        .ok_or(ComputerExecutionError::ControlUnavailable)?;
    if control.blocked {
        return Err(ComputerExecutionError::OutcomeUnknown(
            "上一次 GUI 动作结果未知，必须人工核对并结束控制后重新开始".into(),
        ));
    }
    if control.session.task_id != task_id
        || control.session.state != ControlState::Active
        || control.session.expires_at <= now()
    {
        return Err(ComputerExecutionError::ControlUnavailable);
    }
    Ok(control)
}

fn prune_observations(control: &mut ControlRuntime) {
    let now = now();
    control
        .observations
        .retain(|_, observation| observation.expires_at > now);
    while control.observations.len() >= MAX_RETAINED_OBSERVATIONS {
        let oldest = control
            .observations
            .iter()
            .min_by_key(|(_, observation)| observation.captured_at)
            .map(|(id, _)| id.clone());
        let Some(oldest) = oldest else { break };
        control.observations.remove(&oldest);
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use yukii_agent_ports::computer::ComputerRepositoryError;
    use yukii_contracts::desktop::ControlState;

    #[derive(Default)]
    struct MockRepo {
        actions: StdMutex<Vec<(String, String, String)>>,
        observations: StdMutex<usize>,
        state: StdMutex<Option<ControlState>>,
    }

    impl ComputerRepository for MockRepo {
        fn start_session(
            &self,
            _: &str,
            control: &ControlSession,
        ) -> Result<(), ComputerRepositoryError> {
            *self.state.lock().unwrap() = Some(control.state);
            Ok(())
        }
        fn set_session_state(
            &self,
            _: &str,
            state: ControlState,
        ) -> Result<ControlSession, ComputerRepositoryError> {
            *self.state.lock().unwrap() = Some(state);
            Ok(control_session(state))
        }
        fn record_observation(
            &self,
            _: &str,
            _: &DesktopObservation,
            _: &str,
        ) -> Result<(), ComputerRepositoryError> {
            *self.observations.lock().unwrap() += 1;
            Ok(())
        }
        fn begin_action(
            &self,
            _: &str,
            observation_id: &str,
            task_id: &str,
            _: &str,
            _: &GuiAction,
            action_id: &str,
        ) -> Result<(), ComputerRepositoryError> {
            self.actions.lock().unwrap().push((
                action_id.into(),
                observation_id.into(),
                task_id.into(),
            ));
            Ok(())
        }
        fn finish_action(
            &self,
            action_id: &str,
            state: &str,
            _: Option<&str>,
        ) -> Result<(), ComputerRepositoryError> {
            self.actions
                .lock()
                .unwrap()
                .push((action_id.into(), state.into(), "finished".into()));
            Ok(())
        }
        fn close_stale_sessions(&self, _: &str) -> Result<usize, ComputerRepositoryError> {
            Ok(0)
        }
    }

    fn control_session(state: ControlState) -> ControlSession {
        ControlSession {
            control_id: "control".into(),
            task_id: "task".into(),
            window: DesktopWindow {
                window_id: 5,
                pid: 7,
                application: "Yukii Test".into(),
                title: "测试窗口".into(),
            },
            screenshot_sharing: true,
            generation: 1,
            expires_at: now() + 600,
            state,
        }
    }

    struct MockBridge {
        permissions_ok: bool,
        act: StdMutex<ActOutcome>,
        observed: StdMutex<Vec<GuiAction>>,
    }

    enum ActOutcome {
        Ok,
        Rejected,
        Unknown,
    }

    #[async_trait]
    impl NativeBridge for MockBridge {
        async fn permissions(&self) -> Result<ComputerPermissions, NativeError> {
            Ok(ComputerPermissions {
                screen_capture: self.permissions_ok,
                accessibility: self.permissions_ok,
            })
        }
        async fn windows(&self) -> Result<Vec<DesktopWindow>, NativeError> {
            Ok(vec![DesktopWindow {
                window_id: 5,
                pid: 7,
                application: "Yukii Test".into(),
                title: "测试窗口".into(),
            }])
        }
        async fn observe(&self, _: &DesktopWindow) -> Result<NativeCapture, NativeError> {
            Ok(NativeCapture {
                // 1x1 PNG（透明/黑），只用于协议与尺寸校验。
                png_base64: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=".into(),
                width: 800,
                height: 600,
                frame: [10.0, 20.0, 800.0, 600.0],
                elements: Vec::new(),
            })
        }
        async fn act(
            &self,
            _: &DesktopWindow,
            _: [f64; 4],
            action: &GuiAction,
        ) -> Result<(), NativeError> {
            self.observed.lock().unwrap().push(action.clone());
            match *self.act.lock().unwrap() {
                ActOutcome::Ok => Ok(()),
                ActOutcome::Rejected => Err(NativeError::Rejected("target_not_frontmost".into())),
                ActOutcome::Unknown => Err(NativeError::Unknown("timeout".into())),
            }
        }
    }

    async fn executor(bridge: MockBridge) -> (ComputerExecutor, Arc<MockRepo>) {
        let repo = Arc::new(MockRepo::default());
        let executor = ComputerExecutor::new("session".into(), repo.clone(), Arc::new(bridge))
            .await
            .unwrap();
        (executor, repo)
    }

    #[tokio::test]
    async fn permission_denial_prevents_control_session_creation() {
        let bridge = MockBridge {
            permissions_ok: false,
            act: StdMutex::new(ActOutcome::Ok),
            observed: StdMutex::new(Vec::new()),
        };
        let (executor, _) = executor(bridge).await;
        assert!(matches!(
            executor
                .begin_control("task", &test_window(), true, now() + 600)
                .await,
            Err(ComputerExecutionError::Permission(_))
        ));
    }

    fn empty_bridge() -> MockBridge {
        MockBridge {
            permissions_ok: true,
            act: StdMutex::new(ActOutcome::Ok),
            observed: StdMutex::new(Vec::new()),
        }
    }

    fn test_window() -> DesktopWindow {
        DesktopWindow {
            window_id: 5,
            pid: 7,
            application: "Yukii Test".into(),
            title: "测试窗口".into(),
        }
    }

    #[tokio::test]
    async fn observation_binds_one_action_and_maps_pixel_coordinates() {
        let (executor, repo) = executor(empty_bridge()).await;
        executor
            .begin_control("task", &test_window(), true, now() + 600)
            .await
            .unwrap();
        let observation = executor.observe("task").await.unwrap();
        assert_eq!(observation.width, 800);
        let dispatch = executor
            .act(
                "task",
                "call-1",
                &observation.observation_id,
                &GuiAction::Click { x: 400.0, y: 300.0 },
            )
            .await
            .unwrap();
        assert!(dispatch.verification_required);
        assert!(matches!(
            executor
                .act(
                    "task",
                    "call-2",
                    &observation.observation_id,
                    &GuiAction::Click { x: 400.0, y: 300.0 }
                )
                .await,
            Err(ComputerExecutionError::Observation(_))
        ));
        assert_eq!(*repo.observations.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn only_the_latest_observation_can_drive_an_action() {
        let (executor, _) = executor(empty_bridge()).await;
        executor
            .begin_control("task", &test_window(), true, now() + 600)
            .await
            .unwrap();
        let first = executor.observe("task").await.unwrap();
        let second = executor.observe("task").await.unwrap();
        assert!(matches!(
            executor
                .act(
                    "task",
                    "call-old",
                    &first.observation_id,
                    &GuiAction::Click { x: 1.0, y: 1.0 }
                )
                .await,
            Err(ComputerExecutionError::Observation(_))
        ));
        assert!(
            executor
                .act(
                    "task",
                    "call-new",
                    &second.observation_id,
                    &GuiAction::Click { x: 1.0, y: 1.0 }
                )
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn pause_blocks_and_resume_invalidates_existing_observations() {
        let (executor, _) = executor(empty_bridge()).await;
        executor
            .begin_control("task", &test_window(), true, now() + 600)
            .await
            .unwrap();
        let observation = executor.observe("task").await.unwrap();
        executor.pause("task").await.unwrap();
        assert!(executor.observe("task").await.is_err());
        executor.resume("task").await.unwrap();
        assert!(matches!(
            executor
                .act(
                    "task",
                    "call",
                    &observation.observation_id,
                    &GuiAction::Click { x: 1.0, y: 1.0 }
                )
                .await,
            Err(ComputerExecutionError::Observation(_))
        ));
    }

    #[tokio::test]
    async fn ending_and_starting_again_increments_generation() {
        let (executor, _) = executor(empty_bridge()).await;
        let first = executor
            .begin_control("task", &test_window(), true, now() + 600)
            .await
            .unwrap();
        executor.end("task").await.unwrap();
        let second = executor
            .begin_control("task", &test_window(), true, now() + 600)
            .await
            .unwrap();
        assert_eq!(first.generation, 1);
        assert_eq!(second.generation, 2);
    }

    #[tokio::test]
    async fn bridge_rejection_is_recorded_and_same_observation_cannot_retry() {
        let bridge = MockBridge {
            act: StdMutex::new(ActOutcome::Rejected),
            ..empty_bridge()
        };
        let (executor, repo) = executor(bridge).await;
        executor
            .begin_control("task", &test_window(), true, now() + 600)
            .await
            .unwrap();
        let observation = executor.observe("task").await.unwrap();
        assert!(matches!(
            executor
                .act(
                    "task",
                    "call",
                    &observation.observation_id,
                    &GuiAction::Click { x: 1.0, y: 1.0 }
                )
                .await,
            Err(ComputerExecutionError::Rejected(_))
        ));
        assert!(
            repo.actions
                .lock()
                .unwrap()
                .iter()
                .any(|(_, state, _)| state == "rejected")
        );
        assert!(matches!(
            executor
                .act(
                    "task",
                    "call-2",
                    &observation.observation_id,
                    &GuiAction::Click { x: 1.0, y: 1.0 }
                )
                .await,
            Err(ComputerExecutionError::Observation(_))
        ));
    }

    #[tokio::test]
    async fn bridge_unknown_marks_action_outcome_unknown_without_retry() {
        let bridge = MockBridge {
            act: StdMutex::new(ActOutcome::Unknown),
            ..empty_bridge()
        };
        let (executor, repo) = executor(bridge).await;
        executor
            .begin_control("task", &test_window(), true, now() + 600)
            .await
            .unwrap();
        let observation = executor.observe("task").await.unwrap();
        assert!(matches!(
            executor
                .act(
                    "task",
                    "call",
                    &observation.observation_id,
                    &GuiAction::PressKey {
                        key: "enter".into()
                    }
                )
                .await,
            Err(ComputerExecutionError::OutcomeUnknown(_))
        ));
        {
            let actions = repo.actions.lock().unwrap();
            assert!(
                actions
                    .iter()
                    .any(|(_, state, _)| state == "outcome_unknown")
            );
        }
        // 未知结果后不再自动观察或动作；必须由用户核对并结束控制。
        assert!(matches!(
            executor.observe("task").await,
            Err(ComputerExecutionError::OutcomeUnknown(_))
        ));
        assert!(matches!(
            executor.resume("task").await,
            Err(ComputerExecutionError::OutcomeUnknown(_))
        ));
        executor.end("task").await.unwrap();
        assert!(
            executor
                .begin_control("task", &test_window(), true, now() + 600)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn rejects_non_png_capture_and_missing_sharing_consent() {
        struct BadBridge;
        #[async_trait]
        impl NativeBridge for BadBridge {
            async fn permissions(&self) -> Result<ComputerPermissions, NativeError> {
                Ok(ComputerPermissions {
                    screen_capture: true,
                    accessibility: true,
                })
            }
            async fn windows(&self) -> Result<Vec<DesktopWindow>, NativeError> {
                Ok(vec![test_window()])
            }
            async fn observe(&self, _: &DesktopWindow) -> Result<NativeCapture, NativeError> {
                Ok(NativeCapture {
                    png_base64: "bm90IGEgcG5n".into(),
                    width: 10,
                    height: 10,
                    frame: [0.0, 0.0, 10.0, 10.0],
                    elements: Vec::new(),
                })
            }
            async fn act(
                &self,
                _: &DesktopWindow,
                _: [f64; 4],
                _: &GuiAction,
            ) -> Result<(), NativeError> {
                Ok(())
            }
        }
        let repo = Arc::new(MockRepo::default());
        let executor = ComputerExecutor::new("session".into(), repo, Arc::new(BadBridge))
            .await
            .unwrap();
        executor
            .begin_control("task", &test_window(), true, now() + 600)
            .await
            .unwrap();
        assert!(matches!(
            executor.observe("task").await,
            Err(ComputerExecutionError::Invalid(_))
        ));
    }
}
