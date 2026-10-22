//! 只在服务端读取配置；不修改进程环境，不向客户端或日志暴露密钥。
use reqwest::Url;
use std::{collections::HashMap, path::Path};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiFormat {
    OpenAi,
    Anthropic,
}

/// 故意不实现 Debug，防止调试输出泄露 API Key。
#[derive(Clone)]
pub struct ModelConfig {
    pub format: ApiFormat,
    pub endpoint: Url,
    pub api_key: String,
    pub model: String,
    pub anthropic_version: String,
    pub openai_token_field: String,
    pub max_output_tokens: u32,
    pub request_timeout_seconds: u64,
}

#[derive(Debug, Error)]
#[error("{0}")]
pub struct ConfigError(pub String);

pub struct Settings {
    values: HashMap<String, String>,
}
impl Settings {
    /// 系统环境变量优先于 .env；不使用 set_var，避免异步进程中的环境竞争。
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let mut values = HashMap::new();
        if path.exists() {
            let entries =
                dotenvy::from_path_iter(path).map_err(|_| ConfigError("无法读取 .env".into()))?;
            for entry in entries {
                let (key, value) =
                    entry.map_err(|_| ConfigError(".env 语法错误，请检查引号与等号".into()))?;
                values.insert(key, value);
            }
        }
        values.extend(std::env::vars().filter(|(key, _)| key.starts_with("YUKII_")));
        Ok(Self { values })
    }

    pub fn get(&self, key: &str) -> &str {
        self.values.get(key).map(|s| s.trim()).unwrap_or("")
    }

    pub fn number(&self, key: &str, default: u64, min: u64, max: u64) -> Result<u64, ConfigError> {
        let raw = self.get(key);
        if raw.is_empty() {
            return Ok(default);
        }
        let value = raw
            .parse::<u64>()
            .map_err(|_| ConfigError(format!("{key} 必须是正整数")))?;
        if !(min..=max).contains(&value) {
            return Err(ConfigError(format!("{key} 必须在 {min} 到 {max} 之间")));
        }
        Ok(value)
    }

    /// 显式声明当前聊天模型支持图像输入。默认关闭，避免把截图发给纯文本模型。
    pub fn vision_input(&self) -> Result<bool, ConfigError> {
        match self.get("YUKII_VISION_INPUT") {
            "" | "false" => Ok(false),
            "true" => Ok(true),
            _ => Err(ConfigError(
                "YUKII_VISION_INPUT 仅支持 true 或 false".into(),
            )),
        }
    }

    /// 密钥或模型未填写时允许启动工作台，但不会宣称 Agent 可执行。
    pub fn model(&self) -> Result<Option<ModelConfig>, ConfigError> {
        let format = match self.get("YUKII_API_FORMAT") {
            "" | "openai" => ApiFormat::OpenAi,
            "anthropic" => ApiFormat::Anthropic,
            _ => {
                return Err(ConfigError(
                    "YUKII_API_FORMAT 仅支持 openai 或 anthropic".into(),
                ));
            }
        };
        if self.get("YUKII_API_KEY").is_empty() || self.get("YUKII_MODEL").is_empty() {
            return Ok(None);
        }
        let default_base = match format {
            ApiFormat::OpenAi => "https://api.openai.com/v1",
            ApiFormat::Anthropic => "https://api.anthropic.com/v1",
        };
        let base = if self.get("YUKII_API_BASE_URL").is_empty() {
            default_base
        } else {
            self.get("YUKII_API_BASE_URL")
        };
        let path = match format {
            ApiFormat::OpenAi => "chat/completions",
            ApiFormat::Anthropic => "messages",
        };
        let address = if self.get("YUKII_API_ENDPOINT").is_empty() {
            format!("{}/{path}", base.trim_end_matches('/'))
        } else {
            self.get("YUKII_API_ENDPOINT").to_owned()
        };
        let endpoint =
            Url::parse(&address).map_err(|_| ConfigError("API 地址必须是完整 URL".into()))?;
        let loopback = matches!(
            endpoint.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]")
        );
        if endpoint.scheme() != "https" && !(endpoint.scheme() == "http" && loopback) {
            return Err(ConfigError(
                "远程 API 必须使用 HTTPS；本地回环服务可使用 HTTP".into(),
            ));
        }
        if !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(ConfigError(
                "API 地址不能包含用户名、密码、查询参数或片段".into(),
            ));
        }
        let token_field = match self.get("YUKII_OPENAI_TOKEN_FIELD") {
            "" | "max_completion_tokens" => "max_completion_tokens",
            "max_tokens" => "max_tokens",
            _ => {
                return Err(ConfigError(
                    "YUKII_OPENAI_TOKEN_FIELD 仅支持 max_completion_tokens 或 max_tokens".into(),
                ));
            }
        };
        Ok(Some(ModelConfig {
            format,
            endpoint,
            api_key: self.get("YUKII_API_KEY").to_owned(),
            model: self.get("YUKII_MODEL").to_owned(),
            anthropic_version: match self.get("YUKII_ANTHROPIC_VERSION") {
                "" => "2023-06-01",
                value => value,
            }
            .into(),
            openai_token_field: token_field.into(),
            max_output_tokens: self.number("YUKII_MAX_OUTPUT_TOKENS", 4096, 64, 32768)? as u32,
            request_timeout_seconds: self.number("YUKII_REQUEST_TIMEOUT_SECONDS", 90, 1, 600)?,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings(entries: &[(&str, &str)]) -> Settings {
        Settings {
            values: entries
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
    #[test]
    fn joins_endpoints_and_rejects_insecure_remote_url() {
        let mut config = settings(&[
            ("YUKII_API_FORMAT", "anthropic"),
            ("YUKII_API_KEY", "test"),
            ("YUKII_MODEL", "test-model"),
            ("YUKII_API_BASE_URL", "https://gateway.example/proxy/v1/"),
        ]);
        assert_eq!(
            config.model().unwrap().unwrap().endpoint.as_str(),
            "https://gateway.example/proxy/v1/messages"
        );
        config.values.insert(
            "YUKII_API_BASE_URL".into(),
            "http://gateway.example/v1".into(),
        );
        assert!(config.model().is_err());
        assert!(settings(&[]).model().unwrap().is_none());
        assert!(settings(&[("YUKII_API_FORMAT", "wrong")]).model().is_err());
        assert!(!settings(&[]).vision_input().unwrap());
        assert!(
            settings(&[("YUKII_VISION_INPUT", "true")])
                .vision_input()
                .unwrap()
        );
        assert!(
            settings(&[("YUKII_VISION_INPUT", "yes")])
                .vision_input()
                .is_err()
        );
    }
    #[test]
    fn embedding_config_uses_separate_key_and_rejects_partial_or_insecure_values() {
        assert!(settings(&[]).embedding().unwrap().is_none());
        assert!(
            settings(&[("YUKII_EMBEDDING_MODEL_ID", "vector")])
                .embedding()
                .is_err()
        );
        let mut settings = settings(&[
            ("YUKII_EMBEDDING_API", "vector-test-key"),
            ("YUKII_EMBEDDING_MODEL_ID", "qwen3.7-text-embedding-flash"),
            (
                "YUKII_EMBEDDING_OPENAI_BASE_URL",
                "https://example.com/compatible-mode/v1/",
            ),
            ("YUKII_API_KEY", "chat-test-key"),
        ]);
        let config = settings.embedding().unwrap().unwrap();
        assert_eq!(config.api_key, "vector-test-key");
        assert_eq!(
            config.endpoint.as_str(),
            "https://example.com/compatible-mode/v1/embeddings"
        );
        assert_eq!(config.dimensions, 1024);
        settings.values.insert(
            "YUKII_EMBEDDING_ENDPOINT".into(),
            "https://user:secret@example.com/embeddings".into(),
        );
        assert!(settings.embedding().is_err());
        settings.values.insert(
            "YUKII_EMBEDDING_ENDPOINT".into(),
            "http://example.com/embeddings".into(),
        );
        assert!(settings.embedding().is_err());
    }
}
