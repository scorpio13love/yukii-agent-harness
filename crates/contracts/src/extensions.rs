//! 插件包只声明能力；安装不执行脚本，技能文本不能授予宿主权限。
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginPermission {
    SandboxCompute,
}

/// 只读参考文档；随技能加载返回，不是可执行脚本，也不授予工具权限。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillReference {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillDefinition {
    pub id: String,
    pub description: String,
    pub instructions: String,
    /// 技能声明的工具 allowlist；空列表表示不额外限制。
    #[serde(default)]
    pub tools: Vec<String>,
    /// 例如 references/checklist.md；缺失时按空列表读取。
    #[serde(default)]
    pub references: Vec<SkillReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    /// 容器内的绝对入口和固定参数；用户输入通过 JSON stdin 传入，不能拼接命令。
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    pub name: String,
    pub description: String,
    pub permissions: Vec<PluginPermission>,
    /// 只接受 image@sha256:...；安装器不会自动 pull 或执行镜像。
    pub image: Option<String>,
    pub tools: Vec<PluginTool>,
    pub skills: Vec<SkillDefinition>,
    /// 发布者标识；Ed25519 公钥由部署方配置，不信任包内自报。
    #[serde(default)]
    pub publisher: Option<String>,
    /// Ed25519 签名（base64url no padding），覆盖去掉 signature 的规范化 manifest。
    #[serde(default)]
    pub signature: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPlugin {
    pub manifest: PluginManifest,
    pub sha256: String,
    pub source: String,
    pub enabled: bool,
    pub installed_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketplaceEntry {
    pub id: String,
    pub version: String,
    pub name: String,
    pub description: String,
    pub package: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketplaceCatalog {
    pub schema_version: u32,
    pub plugins: Vec<MarketplaceEntry>,
}

#[derive(Debug, Serialize)]
pub struct PluginPreview {
    pub manifest: PluginManifest,
    pub sha256: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallPlugin {
    pub id: String,
    pub version: String,
    pub sha256: String,
    pub approved_permissions: Vec<PluginPermission>,
    /// 更新必须指向用户已经审核的旧版本；防止并发安装覆盖他人的操作。
    pub expected_previous_sha256: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetPluginEnabled {
    #[serde(default)]
    pub approved_permissions: Vec<PluginPermission>,
    pub enabled: bool,
    pub sha256: String,
}
