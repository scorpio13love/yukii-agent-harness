use crate::{marketplace::Marketplace, schema};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use yukii_agent_ports::{
    extensions::PluginRepository,
    model::ToolDefinition,
    sandbox::{Sandbox, SandboxRequest},
    tools::{Tool, ToolContext, ToolEffect},
};
use yukii_contracts::extensions::{InstalledPlugin, PluginTool};

pub struct ContainerTool {
    plugin: InstalledPlugin,
    definition: PluginTool,
    repository: Arc<dyn PluginRepository>,
    sandbox: Arc<dyn Sandbox>,
}
impl ContainerTool {
    pub fn new(
        plugin: InstalledPlugin,
        definition: PluginTool,
        repository: Arc<dyn PluginRepository>,
        sandbox: Arc<dyn Sandbox>,
    ) -> Self {
        Self {
            plugin,
            definition,
            repository,
            sandbox,
        }
    }
}
#[async_trait]
impl Tool for ContainerTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: format!("ext__{}__{}", self.plugin.manifest.id, self.definition.name),
            description: self.definition.description.clone(),
            schema: self.definition.input_schema.clone(),
        }
    }
    async fn execute(
        &self,
        _context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolEffect, String> {
        schema::validate_value(&self.definition.input_schema, &arguments)?;
        let repository = self.repository.clone();
        let id = self.plugin.manifest.id.clone();
        let current = tokio::task::spawn_blocking(move || repository.plugin(&id))
            .await
            .map_err(|_| "插件权限查询失败")?
            .map_err(|error| error.to_string())?;
        // 运行中的任务持有固定版本；插件被禁用或更新后，下一次调用必须拒绝。
        if !current.enabled || current.sha256 != self.plugin.sha256 {
            return Err("插件已停用或版本已改变，请重新发起任务".into());
        }
        let image = self.plugin.manifest.image.clone().ok_or("插件镜像缺失")?;
        self.sandbox
            .execute(SandboxRequest {
                image,
                command: self.definition.command.clone(),
                input: arguments,
            })
            .await
            .map(ToolEffect::Result)
    }
}

enum ManagementKind {
    ListSkills,
    LoadSkill,
    SearchPlugins,
    DownloadPlugin,
}
struct ManagementTool {
    kind: ManagementKind,
    repository: Arc<dyn PluginRepository>,
    marketplace: Arc<Marketplace>,
}
pub fn management_tools(
    repository: Arc<dyn PluginRepository>,
    marketplace: Arc<Marketplace>,
) -> Vec<Arc<dyn Tool>> {
    [
        ManagementKind::ListSkills,
        ManagementKind::LoadSkill,
        ManagementKind::SearchPlugins,
        ManagementKind::DownloadPlugin,
    ]
    .into_iter()
    .map(|kind| {
        Arc::new(ManagementTool {
            kind,
            repository: repository.clone(),
            marketplace: marketplace.clone(),
        }) as Arc<dyn Tool>
    })
    .collect()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillInput {
    plugin_id: String,
    skill_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchInput {
    query: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DownloadInput {
    id: String,
    version: String,
    sha256: String,
}

#[async_trait]
impl Tool for ManagementTool {
    fn definition(&self) -> ToolDefinition {
        let (name, description, properties, required) = match self.kind {
            ManagementKind::ListSkills => (
                "list_skills",
                "列出已安装且启用的技能；技能文本不授予新的工具权限。",
                json!({}),
                json!([]),
            ),
            ManagementKind::LoadSkill => (
                "load_skill",
                "按精确插件和技能 ID 读取已启用技能说明，作为参考流程。",
                json!({"plugin_id":{"type":"string"},"skill_id":{"type":"string"}}),
                json!(["plugin_id", "skill_id"]),
            ),
            ManagementKind::SearchPlugins => (
                "search_plugins",
                "在管理员配置的插件市场中查找扩展；不能搜索任意 URL。",
                json!({"query":{"type":"string","maxLength":100}}),
                json!(["query"]),
            ),
            ManagementKind::DownloadPlugin => (
                "download_plugin",
                "下载市场中精确版本及摘要的插件，保存为禁用状态供用户审核；不会启用或执行包内容。",
                json!({"id":{"type":"string"},"version":{"type":"string"},"sha256":{"type":"string"}}),
                json!(["id", "version", "sha256"]),
            ),
        };
        ToolDefinition {
            name: name.into(),
            description: description.into(),
            schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
        }
    }
    async fn execute(
        &self,
        _context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolEffect, String> {
        let value = match self.kind {
            ManagementKind::ListSkills => {
                let _: Empty =
                    serde_json::from_value(arguments).map_err(|_| "list_skills 不接受额外参数")?;
                let repository = self.repository.clone();
                let plugins = tokio::task::spawn_blocking(move || repository.plugins())
                    .await
                    .map_err(|_| "技能读取线程失败")?
                    .map_err(|error| error.to_string())?;
                let skills: Vec<_> = plugins.iter()
                    .filter(|plugin| plugin.enabled)
                    .flat_map(|plugin| plugin.manifest.skills.iter().map(|skill| {
                        json!({"plugin_id":plugin.manifest.id,"skill_id":skill.id,"description":skill.description})
                    }))
                    .take(64)
                    .collect();
                json!({"skills":skills,"limit":64})
            }
            ManagementKind::LoadSkill => {
                let input: SkillInput =
                    serde_json::from_value(arguments).map_err(|_| "技能参数无效")?;
                let repository = self.repository.clone();
                let plugin =
                    tokio::task::spawn_blocking(move || repository.plugin(&input.plugin_id))
                        .await
                        .map_err(|_| "技能读取线程失败")?
                        .map_err(|error| error.to_string())?;
                if !plugin.enabled {
                    return Err("技能所在插件未启用".into());
                }
                let skill = plugin
                    .manifest
                    .skills
                    .iter()
                    .find(|skill| skill.id == input.skill_id)
                    .ok_or("技能不存在")?;
                json!({"plugin_id":plugin.manifest.id,"version":plugin.manifest.version,"skill":skill,"trust":"reference_only"})
            }
            ManagementKind::SearchPlugins => {
                let input: SearchInput =
                    serde_json::from_value(arguments).map_err(|_| "搜索参数无效")?;
                if input.query.len() > 400 {
                    return Err("搜索内容过长".into());
                }
                let query = input.query.to_lowercase();
                let entries: Vec<_> = self
                    .marketplace
                    .catalog()
                    .await?
                    .plugins
                    .into_iter()
                    .filter(|entry| {
                        format!("{} {} {}", entry.id, entry.name, entry.description)
                            .to_lowercase()
                            .contains(&query)
                    })
                    .take(20)
                    .collect();
                json!({"plugins":entries})
            }
            ManagementKind::DownloadPlugin => {
                let input: DownloadInput =
                    serde_json::from_value(arguments).map_err(|_| "下载参数无效")?;
                let preview = self.marketplace.preview(&input.id, &input.version).await?;
                if preview.sha256 != input.sha256 {
                    return Err("市场摘要已变更，请重新查询".into());
                }
                let repository = self.repository.clone();
                // Agent 无权更新已安装版本、批准新权限或启用插件；只暂存首次下载的包。
                let installed = tokio::task::spawn_blocking(move || {
                    repository.install_plugin(
                        &preview.manifest,
                        &preview.sha256,
                        &preview.source,
                        None,
                    )
                })
                .await
                .map_err(|_| "插件暂存线程失败")?
                .map_err(|error| error.to_string())?;
                json!({"id":installed.manifest.id,"version":installed.manifest.version,"enabled":installed.enabled,"message":"下载已完成；新增插件需用户在技能与插件页面审核启用"})
            }
        };
        Ok(ToolEffect::Result(value))
    }
}
