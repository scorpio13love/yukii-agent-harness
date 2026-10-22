//! 独立 Executor 组合根。只消费已由受信宿主批准的请求，不接收批准指令。
pub mod native;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::Path,
    sync::Arc,
};
use thiserror::Error;
use yukii_agent_domain::permissions::validate_identity;
use yukii_agent_infra::{DesktopReceipt, StoreError, TaskStore};
use yukii_contracts::desktop::*;
use yukii_desktop_executor::{
    ComputerExecutionError, ComputerExecutor, FileExecutionError, FileExecutor, FileOutcome,
    NativeError,
};
use yukii_desktop_transport::{FrameError, read_frame_value, write_frame};

#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("桌面会话标识无效")]
    Session,
    #[error("桌面协议字段无效：{0}")]
    Protocol(&'static str),
}

pub struct DesktopExecutorService {
    session_id: String,
    store: Arc<TaskStore>,
    files: FileExecutor,
    computer: Option<ComputerExecutor>,
}

impl DesktopExecutorService {
    /// computer 为 None 时 hello 明确报告 computer_use=false；不会伪装成可用。
    pub fn new(
        store: Arc<TaskStore>,
        session_id: String,
        computer: Option<ComputerExecutor>,
    ) -> Result<Self, ExecutorError> {
        validate_identity(&session_id).map_err(|_| ExecutorError::Session)?;
        Ok(Self {
            session_id,
            files: FileExecutor::new(store.clone()),
            store,
            computer,
        })
    }

    /// 生命周期与 IPC 绑定；协议错误/断连退出，stdout 只用于帧，不混入日志。
    pub fn serve(
        &mut self,
        reader: &mut impl Read,
        writer: &mut impl Write,
        runtime: &tokio::runtime::Runtime,
    ) -> Result<(), ExecutorError> {
        while let Some(value) = read_frame_value(reader)? {
            // 先验证命令字段集合，再反序列化；未知命令仍由 serde 明确拒绝。
            let command = value
                .get("command")
                .ok_or(ExecutorError::Protocol("command_missing"))?;
            if let Err(field) = validate_command_shape(command) {
                return Err(ExecutorError::Protocol(field));
            }
            let request: DesktopRequest = serde_json::from_value(value)?;
            let response = runtime.block_on(self.handle(request))?;
            write_frame(writer, &response)?;
        }
        // 断开或 EOF 时尽力结束活动桌面会话；写入失败不影响进程退出。
        if let Some(computer) = &self.computer {
            runtime.block_on(computer.shutdown());
        }
        Ok(())
    }

    async fn handle(&mut self, request: DesktopRequest) -> Result<DesktopResponse, ExecutorError> {
        let reply = if request.version != DESKTOP_PROTOCOL_VERSION {
            error("protocol_version", "桌面协议版本不兼容")
        } else if validate_identity(&request.request_id).is_err() {
            error("request_id", "请求标识无效")
        } else {
            match request.command {
                DesktopCommand::Hello => DesktopReply::Hello {
                    file_execution: true,
                    computer_use: self.computer.is_some(),
                },
                DesktopCommand::RegisterWorkspace { path } => {
                    if !Path::new(&path).is_absolute() {
                        error("workspace_path", "宿主必须提供绝对目录路径")
                    } else {
                        match self
                            .files
                            .register_workspace(&self.session_id, Path::new(&path))
                        {
                            Ok(workspace_id) => DesktopReply::Workspace { workspace_id },
                            Err(_) => error("workspace_unavailable", "无法打开指定工作目录"),
                        }
                    }
                }
                DesktopCommand::UnregisterWorkspace { workspace_id } => {
                    self.files.unregister_workspace(&workspace_id);
                    DesktopReply::Done
                }
                DesktopCommand::ExecuteFile { request } => self.execute_file(request)?,
                DesktopCommand::Acknowledge { permission_id } => {
                    match self
                        .store
                        .acknowledge_desktop_result(&permission_id, &self.session_id)
                    {
                        Ok(()) => DesktopReply::Done,
                        Err(StoreError::IdempotencyConflict | StoreError::NotFound) => {
                            error("receipt_unavailable", "没有可确认的结果")
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
                DesktopCommand::ComputerPermissions => match &self.computer {
                    Some(computer) => match computer.permissions().await {
                        Ok(permissions) => DesktopReply::Permissions { permissions },
                        Err(error) => computer_error(error),
                    },
                    None => computer_unavailable(),
                },
                DesktopCommand::ListWindows => match &self.computer {
                    Some(computer) => match computer.windows().await {
                        Ok(windows) => DesktopReply::Windows { windows },
                        Err(error) => computer_error(error),
                    },
                    None => computer_unavailable(),
                },
                DesktopCommand::BeginControl {
                    task_id,
                    window,
                    screenshot_sharing,
                    expires_at,
                } => match &self.computer {
                    Some(computer) => match computer
                        .begin_control(&task_id, &window, screenshot_sharing, expires_at)
                        .await
                    {
                        Ok(control) => DesktopReply::Control { control },
                        Err(error) => computer_error(error),
                    },
                    None => computer_unavailable(),
                },
                DesktopCommand::Observe { task_id } => match &self.computer {
                    Some(computer) => match computer.observe(&task_id).await {
                        Ok(observation) => DesktopReply::Observation { observation },
                        Err(error) => computer_error(error),
                    },
                    None => computer_unavailable(),
                },
                DesktopCommand::Act {
                    task_id,
                    call_id,
                    observation_id,
                    action,
                } => match &self.computer {
                    Some(computer) => match computer
                        .act(&task_id, &call_id, &observation_id, &action)
                        .await
                    {
                        Ok(dispatch) => DesktopReply::Action { dispatch },
                        Err(error) => computer_error(error),
                    },
                    None => computer_unavailable(),
                },
                DesktopCommand::PauseControl { task_id } => match &self.computer {
                    Some(computer) => match computer.pause(&task_id).await {
                        Ok(control) => DesktopReply::Control { control },
                        Err(error) => computer_error(error),
                    },
                    None => computer_unavailable(),
                },
                DesktopCommand::ResumeControl { task_id } => match &self.computer {
                    Some(computer) => match computer.resume(&task_id).await {
                        Ok(control) => DesktopReply::Control { control },
                        Err(error) => computer_error(error),
                    },
                    None => computer_unavailable(),
                },
                DesktopCommand::EndControl { task_id } => match &self.computer {
                    Some(computer) => match computer.end(&task_id).await {
                        Ok(control) => DesktopReply::Control { control },
                        Err(error) => computer_error(error),
                    },
                    None => computer_unavailable(),
                },
                DesktopCommand::ControlStatus => match &self.computer {
                    Some(computer) => DesktopReply::ControlStatus {
                        control: computer.status().await,
                    },
                    None => DesktopReply::ControlStatus { control: None },
                },
            }
        };
        Ok(DesktopResponse {
            version: DESKTOP_PROTOCOL_VERSION,
            request_id: request.request_id,
            reply,
        })
    }

    fn execute_file(&self, request: FileRequest) -> Result<DesktopReply, ExecutorError> {
        if request.binding.session_id != self.session_id {
            return Ok(error("session_mismatch", "请求不属于当前宿主会话"));
        }
        let digest = request_digest(&request)?;
        let receipt = match self.store.begin_desktop_execution(
            &request.permission_id,
            &request.binding,
            &digest,
        ) {
            Ok(receipt) => receipt,
            Err(StoreError::IdempotencyConflict | StoreError::NotFound) => {
                return Ok(error(
                    "request_conflict",
                    "审批不存在或请求与已保存身份不匹配",
                ));
            }
            Err(error) => return Err(error.into()),
        };
        match receipt {
            DesktopReceipt::Completed(result) => {
                return Ok(DesktopReply::Execution {
                    result,
                    replayed: true,
                });
            }
            DesktopReceipt::OutcomeUnknown => {
                return Ok(DesktopReply::Execution {
                    result: DesktopExecutionResult::OutcomeUnknown {
                        message: "已有执行意图但没有完成回执，请核对结果，未重新执行。".into(),
                    },
                    replayed: true,
                });
            }
            DesktopReceipt::New => {}
        }
        let result = match self.files.execute(
            &request.permission_id,
            &request.binding,
            request.content.as_deref(),
        ) {
            Ok(FileOutcome::Text(content)) => DesktopExecutionResult::Completed {
                result: FileResult::Text { content },
            },
            Ok(FileOutcome::Created { bytes, sha256 }) => DesktopExecutionResult::Completed {
                result: FileResult::Created { bytes, sha256 },
            },
            Err(FileExecutionError::WriteUncertain(_)) => DesktopExecutionResult::OutcomeUnknown {
                message: "文件可能已部分写入，请核对目标文件，禁止自动重试。".into(),
            },
            Err(failure) => DesktopExecutionResult::Rejected {
                code: "file_execution_rejected".into(),
                message: failure.to_string(),
            },
        };
        // 落盘失败直接结束连接：不能把尚未保存的结果宣称为可恢复交付。
        self.store.finish_desktop_execution(
            &request.permission_id,
            &self.session_id,
            &digest,
            &result,
        )?;
        Ok(DesktopReply::Execution {
            result,
            replayed: false,
        })
    }
}

pub fn request_digest(request: &FileRequest) -> Result<String, serde_json::Error> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(request)?)
    ))
}

fn error(code: &str, message: &str) -> DesktopReply {
    DesktopReply::Error {
        code: code.into(),
        message: message.into(),
    }
}

fn computer_unavailable() -> DesktopReply {
    error("computer_unavailable", "本执行进程未配置受控原生桥")
}

/// 存储失败发生在派发前后无法区分时，必须按“结果不确定”告知宿主，禁止自动重试。
fn computer_error(failure: ComputerExecutionError) -> DesktopReply {
    let (code, message) = match failure {
        ComputerExecutionError::Permission(message) => ("computer_permission", message),
        ComputerExecutionError::ControlUnavailable => (
            "computer_control_unavailable",
            "当前没有活动的桌面控制会话".into(),
        ),
        ComputerExecutionError::Observation(message) => {
            ("computer_observation_unavailable", message)
        }
        ComputerExecutionError::Invalid(message) => ("computer_invalid", message),
        ComputerExecutionError::Rejected(message) => ("computer_rejected", message),
        ComputerExecutionError::OutcomeUnknown(message) => ("computer_outcome_unknown", message),
        ComputerExecutionError::Store(_) => (
            "computer_storage_uncertain",
            "桌面审计存储失败，无法确认动作是否已派发；禁止自动重试".into(),
        ),
        ComputerExecutionError::Native(NativeError::Rejected(message)) => {
            ("computer_rejected", message)
        }
        ComputerExecutionError::Native(NativeError::Unknown(message)) => {
            ("computer_outcome_unknown", message)
        }
    };
    error(code, &message)
}
