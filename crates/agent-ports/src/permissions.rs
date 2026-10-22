//! 仅供受信宿主组合根调用。实现者必须持久化决策，不能把模型字段视为批准。
pub use yukii_agent_domain::permissions::PermissionError;
use yukii_contracts::permissions::{
    ExecutionPermit, PermissionBinding, PermissionDecision, PermissionRecord,
};

/// 同步 I/O 端口：异步调用方通过阻塞线程池访问。每次操作使用实现方时钟。
pub trait PermissionRepository: Send + Sync {
    fn request_permission(
        &self,
        binding: PermissionBinding,
        expires_at: i64,
    ) -> Result<PermissionRecord, PermissionError>;
    fn decide_permission(
        &self,
        id: &str,
        binding: &PermissionBinding,
        decision: PermissionDecision,
    ) -> Result<PermissionRecord, PermissionError>;
    fn revoke_permission(
        &self,
        id: &str,
        binding: &PermissionBinding,
    ) -> Result<(), PermissionError>;
    /// 原子领取批准；同一批准只可领取一次，断连不能通过重新领取重跑。
    fn claim_permission(
        &self,
        id: &str,
        binding: &PermissionBinding,
    ) -> Result<ExecutionPermit, PermissionError>;
    /// 执行前重新查询，不使用已缓存批准。撤销不能回滚已经发生的外部写入。
    fn validate_permit(&self, permit: &ExecutionPermit) -> Result<(), PermissionError>;
}
