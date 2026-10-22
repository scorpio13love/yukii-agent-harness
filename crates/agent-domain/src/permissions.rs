//! 纯审批规则。目录句柄、实际文件解析和内容摘要计算由执行基础设施负责。
use thiserror::Error;
use yukii_contracts::permissions::{
    FileOperation, PermissionBinding, PermissionDecision, PermissionState,
};

pub const MAX_APPROVAL_LIFETIME_SECONDS: i64 = 900;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PermissionError {
    #[error("授权范围格式无效")]
    InvalidScope,
    #[error("授权已过期或有效期超出限制")]
    Expired,
    #[error("授权身份或操作与已保存请求不匹配")]
    BindingMismatch,
    #[error("授权状态不允许此操作")]
    InvalidState,
    #[error("授权记录不存在")]
    NotFound,
    #[error("工具调用已结束或发生变化")]
    InvocationUnavailable,
    #[error("授权存储不可用")]
    Storage,
}

pub fn validate_identity(value: &str) -> Result<(), PermissionError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
    {
        return Err(PermissionError::InvalidScope);
    }
    Ok(())
}

pub fn validate_binding(binding: &PermissionBinding) -> Result<(), PermissionError> {
    for value in [
        &binding.session_id,
        &binding.task_id,
        &binding.call_id,
        &binding.scope.workspace_id,
    ] {
        validate_identity(value)?;
    }
    let path = &binding.scope.relative_path;
    // 使用单一 POSIX 相对路径表示，拒绝可歧义化的组件；这不代替防符号链接访问。
    if path.is_empty()
        || path.len() > 4096
        || path
            .chars()
            .any(|c| c.is_control() || c == '\\' || c == ':')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(PermissionError::InvalidScope);
    }
    if let FileOperation::Create { content_sha256 } = &binding.scope.operation
        && (content_sha256.len() != 64
            || !content_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    {
        return Err(PermissionError::InvalidScope);
    }
    Ok(())
}

pub fn validate_expiration(expires_at: i64, now: i64) -> Result<(), PermissionError> {
    if expires_at <= now {
        return Err(PermissionError::Expired);
    }
    Ok(())
}

pub fn validate_new_expiration(expires_at: i64, now: i64) -> Result<(), PermissionError> {
    validate_expiration(expires_at, now)?;
    if expires_at.saturating_sub(now) > MAX_APPROVAL_LIFETIME_SECONDS {
        return Err(PermissionError::Expired);
    }
    Ok(())
}

/// 同一决策可幂等提交，但拒绝、撤销或领取后的授权不能重新激活。
pub fn decide_state(
    current: PermissionState,
    decision: PermissionDecision,
) -> Result<PermissionState, PermissionError> {
    let next = match decision {
        PermissionDecision::Approve => PermissionState::Approved,
        PermissionDecision::Deny => PermissionState::Denied,
    };
    if current == PermissionState::Pending || current == next {
        Ok(next)
    } else {
        Err(PermissionError::InvalidState)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yukii_contracts::permissions::FileScope;

    fn binding() -> PermissionBinding {
        PermissionBinding {
            session_id: "session".into(),
            task_id: "task".into(),
            call_id: "call".into(),
            scope: FileScope {
                workspace_id: "workspace".into(),
                relative_path: "资料/汇总.md".into(),
                operation: FileOperation::Read,
            },
        }
    }

    #[test]
    fn rejects_ambiguous_or_escaping_paths_and_accepts_unicode_names() {
        assert!(validate_binding(&binding()).is_ok());
        for path in [
            "",
            "/tmp/file",
            "../file",
            "a/../b",
            "./a",
            "a//b",
            "a/",
            "a\\b",
            "C:file",
            "a\0b",
        ] {
            let mut request = binding();
            request.scope.relative_path = path.into();
            assert_eq!(
                validate_binding(&request),
                Err(PermissionError::InvalidScope),
                "{path:?}"
            );
        }
    }

    #[test]
    fn creation_requires_canonical_digest_and_bounded_identity() {
        let mut request = binding();
        for digest in ["a".repeat(63), "A".repeat(64), "g".repeat(64)] {
            request.scope.operation = FileOperation::Create {
                content_sha256: digest,
            };
            assert!(validate_binding(&request).is_err());
        }
        request.scope.operation = FileOperation::Create {
            content_sha256: "0".repeat(64),
        };
        assert!(validate_binding(&request).is_ok());
        request.session_id = "x".repeat(129);
        assert!(validate_binding(&request).is_err());
    }

    #[test]
    fn expiration_is_exclusive_and_lifetime_does_not_overflow() {
        assert_eq!(validate_expiration(100, 100), Err(PermissionError::Expired));
        assert!(validate_new_expiration(1000, 100).is_ok());
        assert!(validate_new_expiration(1001, 100).is_err());
        assert!(validate_new_expiration(i64::MAX, 100).is_err());
        assert!(validate_new_expiration(i64::MAX, i64::MIN).is_err());
    }

    #[test]
    fn decisions_are_idempotent_but_never_resurrect_a_closed_grant() {
        use PermissionDecision::*;
        use PermissionState::*;
        assert_eq!(decide_state(Pending, Approve), Ok(Approved));
        assert_eq!(decide_state(Approved, Approve), Ok(Approved));
        assert_eq!(decide_state(Pending, Deny), Ok(Denied));
        assert_eq!(decide_state(Denied, Deny), Ok(Denied));
        for state in [Denied, Claimed, Revoked] {
            assert!(decide_state(state, Approve).is_err());
        }
        assert!(decide_state(Approved, Deny).is_err());
    }
}
