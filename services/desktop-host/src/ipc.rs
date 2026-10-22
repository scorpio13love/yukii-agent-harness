//! 受信宿主与独立 Executor 的私有 stdio 客户端。
//! 每次调用有界等待；超时只终止连接，绝不对动作自动重试。
use crate::approval::ActionApprover;
use async_trait::async_trait;
use std::{
    collections::HashSet,
    io::BufReader,
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::Mutex as AsyncMutex, time::timeout};
use uuid::Uuid;
use yukii_agent_ports::tools::{DesktopControl, DesktopObservationResult};
use yukii_contracts::desktop::*;
use yukii_desktop_transport::{read_frame, write_frame};

const IPC_TIMEOUT: Duration = Duration::from_secs(45);

struct PeerIo {
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

pub struct IpcControl {
    peer: Arc<Mutex<PeerIo>>,
    child: Arc<Mutex<Option<Child>>>,
    approver: Arc<dyn ActionApprover>,
    control: AsyncMutex<Option<ControlSession>>,
    /// 用户拒绝过的观察不再重复弹窗；必须重新观察取得新的 ID。
    denied_observations: Mutex<HashSet<String>>,
    /// 只允许对最近一次观察审批和派发，避免宿主在旧截图上提示用户。
    latest_observation: Mutex<Option<String>>,
}

impl IpcControl {
    /// 只接受绝对可执行路径；会话 ID 由宿主生成，不能由模型提供。
    pub fn spawn(
        executor: &Path,
        database: &Path,
        session_id: &str,
        bridge: &Path,
        approver: Arc<dyn ActionApprover>,
    ) -> Result<Self, String> {
        if !executor.is_absolute() || !executor.is_file() {
            return Err("Executor 必须是绝对文件路径".into());
        }
        if !database.is_absolute() || !database.is_file() {
            return Err("数据库必须是已有绝对路径".into());
        }
        if !bridge.is_absolute() || !bridge.is_file() {
            return Err("原生桥必须是绝对文件路径".into());
        }
        let mut command = Command::new(executor);
        // 执行进程和原生桥只需要操作系统环境；不把宿主可能持有的 YUKII_* 密钥传下去。
        for (key, _) in std::env::vars() {
            if key.starts_with("YUKII_") {
                command.env_remove(key);
            }
        }
        let mut child = command
            .arg("--database")
            .arg(database)
            .arg("--session")
            .arg(session_id)
            .arg("--bridge")
            .arg(bridge)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("无法启动桌面执行服务：{error}"))?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| "Executor 输入管道不可用".to_string())?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| "Executor 输出管道不可用".to_string())?;
        Ok(Self {
            peer: Arc::new(Mutex::new(PeerIo {
                input,
                output: BufReader::new(output),
            })),
            child: Arc::new(Mutex::new(Some(child))),
            approver,
            control: AsyncMutex::new(None),
            denied_observations: Mutex::new(HashSet::new()),
            latest_observation: Mutex::new(None),
        })
    }

    async fn call(&self, command: DesktopCommand) -> Result<DesktopReply, String> {
        let request_id = Uuid::new_v4().to_string();
        let request = DesktopRequest {
            version: DESKTOP_PROTOCOL_VERSION,
            request_id: request_id.clone(),
            command,
        };
        let peer = self.peer.clone();
        let result = timeout(
            IPC_TIMEOUT,
            tokio::task::spawn_blocking(move || -> Result<DesktopResponse, String> {
                let mut peer = peer.lock().map_err(|_| "IPC 状态锁不可用".to_string())?;
                write_frame(&mut peer.input, &request)
                    .map_err(|_| "无法写入 Executor".to_string())?;
                let response: Option<DesktopResponse> =
                    read_frame(&mut peer.output).map_err(|_| "Executor 连接已断开".to_string())?;
                response.ok_or_else(|| "Executor 已关闭连接".to_string())
            }),
        )
        .await;
        let response = match result {
            Ok(Ok(Ok(response))) => response,
            _ => {
                self.kill();
                return Err("Executor 未在时限内响应；不会自动重试".into());
            }
        };
        if response.version != DESKTOP_PROTOCOL_VERSION || response.request_id != request_id {
            self.kill();
            return Err("Executor 响应版本或关联 ID 不匹配".into());
        }
        Ok(response.reply)
    }

    pub async fn hello(&self) -> Result<(), String> {
        match self.call(DesktopCommand::Hello).await? {
            DesktopReply::Hello { computer_use, .. } if computer_use => Ok(()),
            DesktopReply::Hello { .. } => Err("Executor 未配置 Computer Use 原生桥".into()),
            reply => Err(unexpected(reply)),
        }
    }

    pub async fn permissions(&self) -> Result<ComputerPermissions, String> {
        match self.call(DesktopCommand::ComputerPermissions).await? {
            DesktopReply::Permissions { permissions } => Ok(permissions),
            reply => Err(unexpected(reply)),
        }
    }

    pub async fn windows(&self) -> Result<Vec<DesktopWindow>, String> {
        match self.call(DesktopCommand::ListWindows).await? {
            DesktopReply::Windows { windows } => Ok(windows),
            reply => Err(unexpected(reply)),
        }
    }

    pub async fn begin_control(
        &self,
        task_id: &str,
        window: &DesktopWindow,
        screenshot_sharing: bool,
        expires_at: i64,
    ) -> Result<ControlSession, String> {
        let reply = self
            .call(DesktopCommand::BeginControl {
                task_id: task_id.into(),
                window: window.clone(),
                screenshot_sharing,
                expires_at,
            })
            .await?;
        match reply {
            DesktopReply::Control { control } => {
                *self.control.lock().await = Some(control.clone());
                Ok(control)
            }
            reply => Err(unexpected(reply)),
        }
    }

    pub async fn observe(&self, task_id: &str) -> Result<DesktopObservation, String> {
        match self
            .call(DesktopCommand::Observe {
                task_id: task_id.into(),
            })
            .await?
        {
            DesktopReply::Observation { observation } => {
                if let Ok(mut latest) = self.latest_observation.lock() {
                    *latest = Some(observation.observation_id.clone());
                }
                Ok(observation)
            }
            reply => Err(unexpected(reply)),
        }
    }

    /// 审批发生在发送 Act 之前；拒绝或审批失败都不会触发执行端动作。
    pub async fn act_dispatch(
        &self,
        task_id: &str,
        call_id: &str,
        observation_id: &str,
        action: &GuiAction,
    ) -> Result<ActionDispatch, String> {
        let latest = self
            .latest_observation
            .lock()
            .map_err(|_| "观察状态锁不可用".to_string())?
            .clone();
        if latest.as_deref() != Some(observation_id) {
            return Err("必须使用最近一次观察；旧截图不能用于新动作".into());
        }
        if self
            .denied_observations
            .lock()
            .map_err(|_| "审批状态锁不可用".to_string())?
            .contains(observation_id)
        {
            return Err("用户已拒绝该观察的动作，必须重新观察后才能继续".into());
        }
        let approved = self.approver.approve(observation_id, action).await?;
        // 无论批准、拒绝还是执行端拒绝，这次观察都不再可用于后续动作；必须重新观察。
        if let Ok(mut latest) = self.latest_observation.lock() {
            *latest = None;
        }
        if !approved {
            if let Ok(mut denied) = self.denied_observations.lock() {
                denied.insert(observation_id.to_owned());
            }
            return Err("用户拒绝了本次动作".into());
        }
        match self
            .call(DesktopCommand::Act {
                task_id: task_id.into(),
                call_id: call_id.into(),
                observation_id: observation_id.into(),
                action: action.clone(),
            })
            .await?
        {
            DesktopReply::Action { dispatch } => Ok(dispatch),
            reply => Err(unexpected(reply)),
        }
    }

    pub async fn end_control(&self) -> Result<(), String> {
        let control = self.control.lock().await.clone();
        let Some(control) = control else {
            return Ok(());
        };
        let result = match self
            .call(DesktopCommand::EndControl {
                task_id: control.task_id,
            })
            .await?
        {
            DesktopReply::Control { .. } | DesktopReply::Done => Ok(()),
            reply => Err(unexpected(reply)),
        };
        *self.control.lock().await = None;
        result
    }

    pub async fn pause_control(&self, task_id: &str) -> Result<(), String> {
        match self
            .call(DesktopCommand::PauseControl {
                task_id: task_id.into(),
            })
            .await?
        {
            DesktopReply::Control { .. } => Ok(()),
            reply => Err(unexpected(reply)),
        }
    }

    pub async fn resume_control(&self, task_id: &str) -> Result<(), String> {
        match self
            .call(DesktopCommand::ResumeControl {
                task_id: task_id.into(),
            })
            .await?
        {
            DesktopReply::Control { .. } => Ok(()),
            reply => Err(unexpected(reply)),
        }
    }

    /// 结束控制并终止子进程；不把“连接已关闭”当作已结束控制。
    pub async fn shutdown(&self) {
        let _ = self.end_control().await;
        self.kill();
    }

    fn kill(&self) {
        if let Ok(mut guard) = self.child.lock()
            && let Some(mut child) = guard.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for IpcControl {
    fn drop(&mut self) {
        self.kill();
    }
}

#[async_trait]
impl DesktopControl for IpcControl {
    async fn observe(&self, task_id: &str) -> Result<DesktopObservationResult, String> {
        let observation = self.observe(task_id).await?;
        Ok(DesktopObservationResult {
            observation_id: observation.observation_id,
            png_base64: observation.png_base64,
            width: observation.width,
            height: observation.height,
            elements: observation.elements,
        })
    }

    async fn act(
        &self,
        task_id: &str,
        call_id: &str,
        observation_id: &str,
        action: GuiAction,
    ) -> Result<String, String> {
        let dispatch = self
            .act_dispatch(task_id, call_id, observation_id, &action)
            .await?;
        Ok(serde_json::json!({
            "action_id": dispatch.action_id,
            "dispatched_at": dispatch.dispatched_at,
            "verification_required": dispatch.verification_required
        })
        .to_string())
    }
}

fn unexpected(reply: DesktopReply) -> String {
    match reply {
        DesktopReply::Error { code, message } => format!("{message}（{code}）"),
        _ => "Executor 返回了当前请求不期望的响应".into(),
    }
}
