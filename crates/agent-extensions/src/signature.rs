//! 发布者签名：只验证部署方配置的可信 Ed25519 公钥，不信任包内自报身份。
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::Value;
use std::collections::BTreeMap;
use yukii_contracts::extensions::PluginManifest;

/// 规范化 JSON：对象键排序，移除 signature 字段，保证签名覆盖稳定字节。
pub fn canonical_payload(manifest: &PluginManifest) -> Result<Vec<u8>, String> {
    let mut value = serde_json::to_value(manifest).map_err(|_| "manifest 序列化失败")?;
    if let Some(object) = value.as_object_mut() {
        object.remove("signature");
    }
    serde_json::to_vec(&canonicalize(value)).map_err(|_| "manifest 规范化失败".to_string())
}

fn canonicalize(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sorted = BTreeMap::new();
            for (key, value) in map {
                sorted.insert(key, canonicalize(value));
            }
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonicalize).collect()),
        other => other,
    }
}

fn decode_key(value: &str) -> Result<VerifyingKey, String> {
    let bytes = hex::decode(value).map_err(|_| "可信公钥不是 hex".to_string())?;
    VerifyingKey::from_bytes(
        bytes
            .as_slice()
            .try_into()
            .map_err(|_| "可信公钥长度错误".to_string())?,
    )
    .map_err(|_| "可信公钥无效".into())
}

pub fn verify(manifest: &PluginManifest, trusted: &BTreeMap<String, String>) -> Result<(), String> {
    let (Some(publisher), Some(signature)) = (&manifest.publisher, &manifest.signature) else {
        return Err("插件未签名".into());
    };
    let key = trusted.get(publisher).ok_or("发布者公钥未配置")?;
    let key = decode_key(key)?;
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| "签名不是 base64url".to_string())?;
    let signature = Signature::from_slice(&signature).map_err(|_| "签名长度错误".to_string())?;
    key.verify(&canonical_payload(manifest)?, &signature)
        .map_err(|_| "插件签名校验失败".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;

    fn manifest() -> PluginManifest {
        serde_json::from_value(json!({
            "schema_version":1,"id":"signed","version":"1.0.0","name":"Signed",
            "description":"signed test","permissions":[],"tools":[],
            "skills":[{"id":"s","description":"s","instructions":"i"}]
        }))
        .unwrap()
    }

    #[test]
    fn verifies_only_trusted_signature() {
        let mut manifest = manifest();
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let public = key.verifying_key();
        manifest.publisher = Some("alice".into());
        manifest.signature = Some(
            URL_SAFE_NO_PAD.encode(key.sign(&canonical_payload(&manifest).unwrap()).to_bytes()),
        );
        let trusted = BTreeMap::from([("alice".into(), hex::encode(public.to_bytes()))]);
        assert!(verify(&manifest, &trusted).is_ok());
        assert!(verify(&manifest, &BTreeMap::new()).is_err());
    }
}
