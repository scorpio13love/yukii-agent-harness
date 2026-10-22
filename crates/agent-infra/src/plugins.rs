use crate::TaskStore;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use yukii_agent_ports::{extensions::PluginRepository, repository::RepositoryError};
use yukii_contracts::extensions::{InstalledPlugin, PluginManifest};

fn db(_: rusqlite::Error) -> RepositoryError {
    RepositoryError::Backend
}
fn read_plugin(connection: &Connection, id: &str) -> Result<InstalledPlugin, RepositoryError> {
    let row: Option<(String, String, String, bool, i64)> = connection
        .query_row(
            "SELECT manifest,sha256,source,enabled,installed_at FROM installed_plugins WHERE id=?1",
            [id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(db)?;
    let (manifest, sha256, source, enabled, installed_at) = row.ok_or(RepositoryError::NotFound)?;
    Ok(InstalledPlugin {
        manifest: serde_json::from_str(&manifest).map_err(|_| RepositoryError::Backend)?,
        sha256,
        source,
        enabled,
        installed_at,
    })
}
impl PluginRepository for TaskStore {
    fn plugins(&self) -> Result<Vec<InstalledPlugin>, RepositoryError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| RepositoryError::Backend)?;
        let mut statement = connection
            .prepare("SELECT id FROM installed_plugins ORDER BY id")
            .map_err(db)?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(db)?;
        ids.map(|id| read_plugin(&connection, &id.map_err(db)?))
            .collect()
    }
    fn plugin(&self, id: &str) -> Result<InstalledPlugin, RepositoryError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| RepositoryError::Backend)?;
        read_plugin(&connection, id)
    }
    fn install_plugin(
        &self,
        manifest: &PluginManifest,
        sha256: &str,
        source: &str,
        expected_previous: Option<&str>,
    ) -> Result<InstalledPlugin, RepositoryError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| RepositoryError::Backend)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db)?;
        match read_plugin(&transaction, &manifest.id) {
            Ok(previous) => {
                if previous.sha256 == sha256 {
                    return Ok(previous);
                }
                if expected_previous != Some(previous.sha256.as_str()) {
                    return Err(RepositoryError::IdempotencyConflict);
                }
            }
            Err(RepositoryError::NotFound) => {
                if expected_previous.is_some() {
                    return Err(RepositoryError::IdempotencyConflict);
                }
            }
            Err(error) => return Err(error),
        }
        let count: i64 = transaction
            .query_row("SELECT COUNT(*) FROM installed_plugins", [], |row| {
                row.get(0)
            })
            .map_err(db)?;
        if count >= 100 && expected_previous.is_none() {
            return Err(RepositoryError::InvalidRequest(
                "当前实例最多安装 100 个插件",
            ));
        }
        let installed_at = crate::sqlite::now();
        // 安装和更新都置为禁用，校验通过不等于用户已经授权启用。
        let serialized = serde_json::to_string(manifest).map_err(|_| RepositoryError::Backend)?;
        transaction
            .execute(
                "INSERT INTO installed_plugins(id,manifest,sha256,source,enabled,installed_at)
             VALUES(?1,?2,?3,?4,0,?5) ON CONFLICT(id) DO UPDATE SET
             manifest=excluded.manifest,sha256=excluded.sha256,source=excluded.source,
             enabled=0,installed_at=excluded.installed_at",
                params![manifest.id, serialized, sha256, source, installed_at],
            )
            .map_err(db)?;
        transaction.execute(
            "INSERT INTO plugin_audit(plugin_id,sha256,operation,created_at) VALUES(?1,?2,'install',?3)",
            params![manifest.id, sha256, installed_at],
        ).map_err(db)?;
        transaction.commit().map_err(db)?;
        Ok(InstalledPlugin {
            manifest: manifest.clone(),
            sha256: sha256.into(),
            source: source.into(),
            enabled: false,
            installed_at,
        })
    }
    fn set_plugin_enabled(
        &self,
        id: &str,
        sha256: &str,
        enabled: bool,
    ) -> Result<InstalledPlugin, RepositoryError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| RepositoryError::Backend)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db)?;
        let mut plugin = read_plugin(&transaction, id)?;
        if plugin.sha256 != sha256 {
            return Err(RepositoryError::IdempotencyConflict);
        }
        if plugin.enabled == enabled {
            return Ok(plugin);
        }
        transaction
            .execute(
                "UPDATE installed_plugins SET enabled=?1 WHERE id=?2",
                params![enabled, id],
            )
            .map_err(db)?;
        transaction.execute(
            "INSERT INTO plugin_audit(plugin_id,sha256,operation,created_at) VALUES(?1,?2,?3,?4)",
            params![id, sha256, if enabled { "enable" } else { "disable" }, crate::sqlite::now()],
        ).map_err(db)?;
        transaction.commit().map_err(db)?;
        plugin.enabled = enabled;
        Ok(plugin)
    }
}
