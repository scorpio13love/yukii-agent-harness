//! 插件安装、技能发现与工具适配。
pub mod manifest;
pub mod marketplace;
mod schema;
pub mod signature;
pub mod skill_context;
mod tools;
use async_trait::async_trait;
use marketplace::Marketplace;
use std::sync::Arc;
use yukii_agent_ports::{
    extensions::PluginRepository,
    repository::RepositoryError,
    sandbox::Sandbox,
    tools::{Tool, ToolCatalog},
};
use yukii_contracts::extensions::{InstallPlugin, InstalledPlugin, SetPluginEnabled};

pub use skill_context::SkillContextProvider;

/// 保留失败类别，让 HTTP 层区分输入错误、并发冲突、市场故障与存储故障。
#[derive(Debug, thiserror::Error)]
pub enum ExtensionError {
    #[error("{0}")]
    InvalidRequest(&'static str),
    #[error("{0}")]
    Marketplace(String),
    #[error("Docker 沙箱不可用，不能启用容器工具")]
    SandboxUnavailable,
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

pub struct ExtensionManager {
    repository: Arc<dyn PluginRepository>,
    marketplace: Arc<Marketplace>,
    sandbox: Arc<dyn Sandbox>,
    mutation: tokio::sync::Mutex<()>,
    trusted_publishers: std::collections::BTreeMap<String, String>,
}
impl ExtensionManager {
    pub fn new(
        repository: Arc<dyn PluginRepository>,
        marketplace: Arc<Marketplace>,
        sandbox: Arc<dyn Sandbox>,
    ) -> Self {
        Self {
            repository,
            marketplace,
            sandbox,
            mutation: tokio::sync::Mutex::new(()),
            trusted_publishers: std::collections::BTreeMap::new(),
        }
    }

    pub fn with_trusted_publishers(
        mut self,
        keys: std::collections::BTreeMap<String, String>,
    ) -> Self {
        self.trusted_publishers = keys;
        self
    }
    pub fn marketplace(&self) -> &Marketplace {
        &self.marketplace
    }
    pub async fn installed(&self) -> Result<Vec<InstalledPlugin>, RepositoryError> {
        let repository = self.repository.clone();
        tokio::task::spawn_blocking(move || repository.plugins())
            .await
            .map_err(|_| RepositoryError::Backend)?
    }
    /// 再次验证包摘要及权限，防止用户预览之后市场索引变化。安装不执行包内容。
    pub async fn install(&self, input: InstallPlugin) -> Result<InstalledPlugin, ExtensionError> {
        let _guard = self.mutation.lock().await;
        let preview = self
            .marketplace
            .preview(&input.id, &input.version)
            .await
            .map_err(ExtensionError::Marketplace)?;
        if preview.sha256 != input.sha256
            || preview.manifest.permissions != input.approved_permissions
        {
            return Err(ExtensionError::InvalidRequest(
                "包版本或权限已变化，请重新审核",
            ));
        }
        if preview.manifest.signature.is_some() {
            signature::verify(&preview.manifest, &self.trusted_publishers)
                .map_err(|_| ExtensionError::InvalidRequest("插件签名无效或发布者未受信"))?;
        }
        let repository = self.repository.clone();
        tokio::task::spawn_blocking(move || {
            repository.install_plugin(
                &preview.manifest,
                &preview.sha256,
                &preview.source,
                input.expected_previous_sha256.as_deref(),
            )
        })
        .await
        .map_err(|_| RepositoryError::Backend)?
        .map_err(ExtensionError::Repository)
    }
    pub async fn set_enabled(
        &self,
        id: String,
        input: SetPluginEnabled,
    ) -> Result<InstalledPlugin, ExtensionError> {
        let _guard = self.mutation.lock().await;
        let installed = self.installed().await?;
        let plugin = installed
            .iter()
            .find(|plugin| plugin.manifest.id == id)
            .ok_or(RepositoryError::NotFound)?;
        if input.enabled {
            if plugin.manifest.permissions != input.approved_permissions {
                return Err(ExtensionError::InvalidRequest(
                    "启用插件需要明确批准清单权限",
                ));
            }
            let count: usize = installed
                .iter()
                .filter(|plugin| plugin.enabled && plugin.manifest.id != id)
                .map(|plugin| plugin.manifest.tools.len())
                .sum();
            if count + plugin.manifest.tools.len() + 7 > 64 {
                return Err(ExtensionError::InvalidRequest(
                    "启用后工具数量超过 64 个上限",
                ));
            }
            if !plugin.manifest.tools.is_empty() && !self.sandbox.status().await.available {
                return Err(ExtensionError::SandboxUnavailable);
            }
        }
        let repository = self.repository.clone();
        tokio::task::spawn_blocking(move || {
            repository.set_plugin_enabled(&id, &input.sha256, input.enabled)
        })
        .await
        .map_err(|_| RepositoryError::Backend)?
        .map_err(ExtensionError::Repository)
    }
}
#[async_trait]
impl ToolCatalog for ExtensionManager {
    async fn tools(&self) -> Result<Vec<Arc<dyn Tool>>, RepositoryError> {
        let mut result = yukii_tool_gateway::ToolGateway::office().tools().await?;
        result.extend(tools::management_tools(
            self.repository.clone(),
            self.marketplace.clone(),
        ));
        for plugin in self
            .installed()
            .await?
            .into_iter()
            .filter(|plugin| plugin.enabled)
        {
            for definition in &plugin.manifest.tools {
                result.push(Arc::new(tools::ContainerTool::new(
                    plugin.clone(),
                    definition.clone(),
                    self.repository.clone(),
                    self.sandbox.clone(),
                )));
            }
        }
        Ok(result)
    }
}
