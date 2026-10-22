//! 工具注册表：Runtime 面向统一 Tool 接口，不感知内置工具或插件来源。
mod builtins;
use async_trait::async_trait;
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};
pub use yukii_agent_ports::tools::ToolEffect;
use yukii_agent_ports::{
    model::ToolDefinition,
    repository::RepositoryError,
    tools::{Tool, ToolCatalog, ToolContext},
};

#[derive(Clone)]
pub struct ToolGateway {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}
impl ToolGateway {
    pub fn office() -> Self {
        let mut tools = BTreeMap::new();
        for definition in builtins::definitions() {
            tools.insert(
                definition.name.clone(),
                Arc::new(BuiltinTool(definition)) as Arc<dyn Tool>,
            );
        }
        Self { tools }
    }
    /// 注册表对重复名字拒绝覆盖，防止插件冒充内置工具。
    pub fn from_tools(tools: Vec<Arc<dyn Tool>>) -> Result<Self, RepositoryError> {
        let mut registry = BTreeMap::new();
        for tool in tools {
            let name = tool.definition().name;
            if registry.insert(name, tool).is_some() {
                return Err(RepositoryError::InvalidRequest("工具名称冲突"));
            }
        }
        if registry.len() > 64 {
            return Err(RepositoryError::InvalidRequest(
                "一次任务最多开放 64 个工具",
            ));
        }
        Ok(Self { tools: registry })
    }
    pub fn restricted(allowed: impl IntoIterator<Item = String>) -> Self {
        let mut tools = Self::office().tools;
        let allowed: std::collections::BTreeSet<_> = allowed.into_iter().collect();
        tools.retain(|name, _| allowed.contains(name));
        Self { tools }
    }
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|tool| tool.definition()).collect()
    }
    pub async fn execute(
        &self,
        context: &ToolContext,
        name: &str,
        arguments: Value,
    ) -> Result<ToolEffect, String> {
        let tool = self.tools.get(name).ok_or("当前执行策略未授权该工具")?;
        if tool.execution_target() != context.target() {
            return Err("工具执行位置与当前授权上下文不匹配".into());
        }
        tool.execute(context, arguments).await
    }
}
#[async_trait]
impl ToolCatalog for ToolGateway {
    async fn tools(&self) -> Result<Vec<Arc<dyn Tool>>, RepositoryError> {
        Ok(self.tools.values().cloned().collect())
    }
}
/// 把多个 ToolCatalog 合并为一个；重名由后续 ToolGateway::from_tools 拒绝。
pub struct CompositeToolCatalog {
    catalogs: Vec<Arc<dyn ToolCatalog>>,
}

impl CompositeToolCatalog {
    pub fn new(catalogs: Vec<Arc<dyn ToolCatalog>>) -> Self {
        Self { catalogs }
    }
}

#[async_trait]
impl ToolCatalog for CompositeToolCatalog {
    async fn tools(&self) -> Result<Vec<Arc<dyn Tool>>, RepositoryError> {
        let mut tools = Vec::new();
        for catalog in &self.catalogs {
            tools.extend(catalog.tools().await?);
        }
        Ok(tools)
    }
}

struct BuiltinTool(ToolDefinition);
#[async_trait]
impl Tool for BuiltinTool {
    fn definition(&self) -> ToolDefinition {
        self.0.clone()
    }
    async fn execute(
        &self,
        _context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolEffect, String> {
        builtins::execute(&self.0.name, arguments)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use yukii_agent_ports::tools::ExecutionTarget;

    struct LocalOnlyTool(Arc<AtomicBool>);

    #[async_trait]
    impl Tool for LocalOnlyTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "local_write".into(),
                description: "测试本机工具边界".into(),
                schema: serde_json::json!({"type":"object"}),
            }
        }

        fn execution_target(&self) -> ExecutionTarget {
            ExecutionTarget::LocalComputer
        }

        async fn execute(&self, _: &ToolContext, _: Value) -> Result<ToolEffect, String> {
            self.0.store(true, Ordering::SeqCst);
            Ok(ToolEffect::Result(Value::Null))
        }
    }

    #[tokio::test]
    async fn office_context_cannot_invoke_registered_local_tool_or_forge_target_in_arguments() {
        let invoked = Arc::new(AtomicBool::new(false));
        let gateway =
            ToolGateway::from_tools(vec![Arc::new(LocalOnlyTool(invoked.clone()))]).unwrap();
        let result = gateway
            .execute(
                &ToolContext::office("task", "call"),
                "local_write",
                serde_json::json!({"target":"local_computer","approved":true}),
            )
            .await;
        assert!(result.is_err());
        assert!(!invoked.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn tool_presence_does_not_imply_permission() {
        let gateway = ToolGateway::restricted(["calculate".into()]);
        assert_eq!(gateway.definitions().len(), 1);
        assert!(
            gateway
                .execute(
                    &ToolContext::office("test", "call-1"),
                    "create_markdown",
                    serde_json::json!({"title":"x","content":"y"})
                )
                .await
                .is_err()
        );
    }
}
