use crate::repository::RepositoryError;
use yukii_contracts::extensions::{InstalledPlugin, PluginManifest};

pub trait PluginRepository: Send + Sync {
    fn plugins(&self) -> Result<Vec<InstalledPlugin>, RepositoryError>;
    fn plugin(&self, id: &str) -> Result<InstalledPlugin, RepositoryError>;
    fn install_plugin(
        &self,
        manifest: &PluginManifest,
        sha256: &str,
        source: &str,
        expected_previous: Option<&str>,
    ) -> Result<InstalledPlugin, RepositoryError>;
    fn set_plugin_enabled(
        &self,
        id: &str,
        sha256: &str,
        enabled: bool,
    ) -> Result<InstalledPlugin, RepositoryError>;
}
