use crate::schema;
use std::collections::BTreeSet;
use yukii_contracts::extensions::{PluginManifest, PluginPermission};

pub fn validate(manifest: &PluginManifest) -> Result<(), String> {
    if manifest.schema_version != 1
        || !identifier(&manifest.id, 24)
        || !version(&manifest.version)
        || manifest.name.trim().is_empty()
        || manifest.name.len() > 160
        || manifest.description.len() > 1000
    {
        return Err("插件标识、版本或清单版本无效".into());
    }
    if manifest
        .publisher
        .as_ref()
        .is_some_and(|value| value.trim().is_empty() || value.len() > 120)
        || manifest
            .signature
            .as_ref()
            .is_some_and(|value| value.len() > 512)
    {
        return Err("发布者或签名格式无效".into());
    }
    if manifest.skills.len() > 16
        || manifest.tools.len() > 16
        || manifest.skills.is_empty() && manifest.tools.is_empty()
    {
        return Err("插件必须提供 1–16 个技能或工具".into());
    }
    if manifest.permissions.len() > 1 {
        return Err("插件声明了重复或过多权限".into());
    }
    if !manifest.tools.is_empty() {
        if manifest.permissions != [PluginPermission::SandboxCompute] {
            return Err("容器工具必须明确声明 sandbox_compute 权限".into());
        }
        let image = manifest.image.as_deref().ok_or("容器工具缺少镜像")?;
        let (repository, digest) = image
            .split_once("@sha256:")
            .ok_or("镜像必须固定 sha256 摘要")?;
        if repository.is_empty()
            || repository.starts_with('-')
            || repository.len() > 200
            || !repository
                .bytes()
                .all(|value| value.is_ascii_alphanumeric() || b"/._:-".contains(&value))
            || !checksum(digest)
        {
            return Err("镜像摘要无效".into());
        }
    } else if manifest.image.is_some() || !manifest.permissions.is_empty() {
        return Err("纯技能包不得声明容器权限或镜像".into());
    }
    let mut names = BTreeSet::new();
    for tool in &manifest.tools {
        if !identifier(&tool.name, 24)
            || !names.insert(&tool.name)
            || tool.description.len() > 1000
            || tool.description.trim().is_empty()
            || tool.command.is_empty()
            || tool.command.len() > 16
            || !tool.command[0].starts_with('/')
            || tool
                .command
                .iter()
                .any(|value| value.contains('\0') || value.len() > 16000)
            || tool.input_schema["type"] != "object"
        {
            return Err("插件工具名称、入口或描述无效".into());
        }
        schema::validate_schema(&tool.input_schema, 0)?;
    }
    names.clear();
    for skill in &manifest.skills {
        if !identifier(&skill.id, 24)
            || !names.insert(&skill.id)
            || skill.description.is_empty()
            || skill.description.len() > 1000
            || skill.instructions.trim().is_empty()
            || skill.instructions.len() > 16000
        {
            return Err("技能标识、描述或正文无效".into());
        }
        if skill.tools.len() > 16 || skill.tools.iter().any(|tool| !identifier(tool, 24)) {
            return Err("技能工具 allowlist 无效".into());
        }
        if skill.references.len() > 16 {
            return Err("单个技能最多 16 个 references".into());
        }
        let mut reference_bytes = 0usize;
        for reference in &skill.references {
            if reference.path.is_empty()
                || reference.path.len() > 200
                || reference.path.contains('\0')
                || reference.path.starts_with('/')
                || reference
                    .path
                    .split('/')
                    .any(|part| part == ".." || part.is_empty())
                || reference.content.len() > 16000
            {
                return Err("技能 reference 路径或内容无效".into());
            }
            reference_bytes = reference_bytes.saturating_add(reference.content.len());
        }
        if reference_bytes > 64 * 1024 {
            return Err("技能 references 总大小超过 64 KiB".into());
        }
    }
    Ok(())
}
pub fn identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        && !value.contains("__")
}
pub fn version(value: &str) -> bool {
    let parts: Vec<_> = value.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 6
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
}
pub fn checksum(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manifest_rejects_unreviewable_permissions_entries_and_schemas() {
        let original: PluginManifest = serde_json::from_str(include_str!(
            "../../../examples/marketplace/packages/text_utilities-1.0.0.json"
        ))
        .unwrap();
        validate(&original).unwrap();
        let mut manifest = original.clone();
        manifest.permissions.clear();
        assert!(validate(&manifest).is_err());
        manifest = original.clone();
        manifest.image = Some("python:latest".into());
        assert!(validate(&manifest).is_err());
        manifest = original.clone();
        manifest.tools[0].input_schema["oneOf"] = serde_json::json!([]);
        assert!(validate(&manifest).is_err());
        manifest = original.clone();
        manifest.tools[0].command[0] = "--privileged".into();
        assert!(validate(&manifest).is_err());
        let mut json = serde_json::to_value(original).unwrap();
        json["permissions"] = serde_json::json!(["host_shell"]);
        assert!(serde_json::from_value::<PluginManifest>(json).is_err());
    }
}
