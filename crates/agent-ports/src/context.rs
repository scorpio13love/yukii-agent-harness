use crate::repository::RepositoryError;
use async_trait::async_trait;
use yukii_contracts::knowledge::KnowledgeHit;

#[derive(Default)]
pub struct PreparedContext {
    pub content: String,
    pub sources: Vec<KnowledgeHit>,
    /// 技能声明的工具 allowlist；多个组件取并集，空表示不额外限制。
    pub tool_allowlist: Vec<String>,
}

/// RAG、跨任务记忆或其他上下文组件统一挂在此处，Runtime 不依赖其存储格式。
/// 返回内容属于低可信参考资料，不能替换系统规则或新增工具权限。
#[async_trait]
pub trait ContextProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn prepare(
        &self,
        task_id: &str,
        prompt: &str,
    ) -> Result<PreparedContext, RepositoryError>;
    /// 默认不写记忆；有持久化需求的组件实现此钩子，并自行保证按 task_id 幂等。
    async fn remember(
        &self,
        _task_id: &str,
        _prompt: &str,
        _answer: &str,
    ) -> Result<(), RepositoryError> {
        Ok(())
    }
}
