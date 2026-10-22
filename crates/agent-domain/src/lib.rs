// 与网络、数据库和平台无关的任务规则。
use thiserror::Error;
use yukii_contracts::{ExecutionTarget, TaskStatus};
pub mod computer;
pub mod permissions;

pub const MAX_PROMPT_CHARS: usize = 12_000;

#[derive(Debug, Error)]
pub enum TaskError {
    #[error("请输入任务内容")]
    EmptyPrompt,
    #[error("任务内容不能超过 12000 个字符")]
    PromptTooLong,
    #[error("当前服务尚未接入本机执行能力")]
    UnsupportedTarget,
    #[error("当前任务状态不允许执行此操作")]
    InvalidTransition,
}

/// 在服务端检查目标能力；前端隐藏按钮不能代替权限边界。
/// 网页 HTTP 入口只能创建云端办公任务；本机目标必须走受信桌面宿主。
pub fn validate_task(prompt: &str, target: ExecutionTarget) -> Result<String, TaskError> {
    let prompt = validate_prompt(prompt)?;
    if target != ExecutionTarget::CloudOffice {
        return Err(TaskError::UnsupportedTarget);
    }
    Ok(prompt)
}

/// 仅供受信桌面宿主调用。Host 已负责窗口选择、用户同意与进程边界；
/// 这不代表网页 API 可以放宽，api-server 仍使用 `validate_task` 拒绝本机目标。
pub fn validate_desktop_task(prompt: &str, target: ExecutionTarget) -> Result<String, TaskError> {
    let prompt = validate_prompt(prompt)?;
    if target != ExecutionTarget::LocalComputer {
        return Err(TaskError::UnsupportedTarget);
    }
    Ok(prompt)
}

fn validate_prompt(prompt: &str) -> Result<String, TaskError> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err(TaskError::EmptyPrompt);
    }
    if prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(TaskError::PromptTooLong);
    }
    Ok(prompt.to_owned())
}

/// 明确列举合法状态变化，防止终态被恢复或跳过校验直接完成。
pub fn transition(from: TaskStatus, to: TaskStatus) -> Result<(), TaskError> {
    use TaskStatus::*;
    let allowed = matches!(
        (from, to),
        (Queued, Planning | Cancelled | Failed)
            | (Planning, Running | WaitingForUser | Cancelled | Failed)
            | (Running, WaitingForUser | Verifying | Cancelled | Failed)
            | (WaitingForUser, Planning | Running | Cancelled | Failed)
            | (Verifying, Running | Completed | Cancelled | Failed)
    );
    if allowed {
        Ok(())
    } else {
        Err(TaskError::InvalidTransition)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_local_target_and_blank_input() {
        assert!(matches!(
            validate_task("操作本机", ExecutionTarget::LocalComputer),
            Err(TaskError::UnsupportedTarget)
        ));
        assert!(matches!(
            validate_task(" \n ", ExecutionTarget::CloudOffice),
            Err(TaskError::EmptyPrompt)
        ));
        assert_eq!(
            validate_task("  写简报  ", ExecutionTarget::CloudOffice).unwrap(),
            "写简报"
        );
        // 本机目标只能由受信桌面宿主使用另一条显式入口创建。
        assert!(matches!(
            validate_desktop_task("操作本机", ExecutionTarget::CloudOffice),
            Err(TaskError::UnsupportedTarget)
        ));
        assert_eq!(
            validate_desktop_task("查看测试窗口", ExecutionTarget::LocalComputer).unwrap(),
            "查看测试窗口"
        );
    }

    #[test]
    fn terminal_states_cannot_restart_and_completion_requires_verification() {
        use TaskStatus::*;
        for terminal in [Completed, Failed, Cancelled] {
            assert!(transition(terminal, Running).is_err());
        }
        assert!(transition(Running, Completed).is_err());
        assert!(transition(Verifying, Completed).is_ok());
        assert!(transition(Queued, Cancelled).is_ok());
    }
}
