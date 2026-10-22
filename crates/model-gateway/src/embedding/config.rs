use crate::config::{ConfigError, Settings};
use reqwest::Url;

#[derive(Clone, Copy, Debug)]
pub enum EmbeddingFormat {
    OpenAi,
    DashScope,
}
/// 不实现 Debug，避免调试日志打印凭据。
pub struct EmbeddingConfig {
    pub format: EmbeddingFormat,
    pub endpoint: Url,
    pub model: String,
    pub api_key: String,
    pub dimensions: usize,
    pub timeout_seconds: u64,
}
impl Settings {
    /// 兼容用户已有 EMBEDDING_API / MODEL_ID 变量；不借用聊天模型密钥。
    pub fn embedding(&self) -> Result<Option<EmbeddingConfig>, ConfigError> {
        let key = match self.get("YUKII_EMBEDDING_API_KEY") {
            "" => self.get("YUKII_EMBEDDING_API"),
            value => value,
        };
        let model = self.get("YUKII_EMBEDDING_MODEL_ID");
        if key.is_empty() && model.is_empty() {
            return Ok(None);
        }
        if key.is_empty() || model.is_empty() {
            return Err(ConfigError(
                "向量模型需要同时配置 API 密钥与 MODEL_ID".into(),
            ));
        }
        let format = match self.get("YUKII_EMBEDDING_FORMAT") {
            "" | "openai" => EmbeddingFormat::OpenAi,
            "dashscope" => EmbeddingFormat::DashScope,
            _ => {
                return Err(ConfigError(
                    "YUKII_EMBEDDING_FORMAT 仅支持 openai 或 dashscope".into(),
                ));
            }
        };
        let (base_key, path) = match format {
            EmbeddingFormat::OpenAi => ("YUKII_EMBEDDING_OPENAI_BASE_URL", "embeddings"),
            EmbeddingFormat::DashScope => (
                "YUKII_EMBEDDING_DASHSCOPE_BASE_URL",
                "services/embeddings/text-embedding/text-embedding",
            ),
        };
        let address = if !self.get("YUKII_EMBEDDING_ENDPOINT").is_empty() {
            self.get("YUKII_EMBEDDING_ENDPOINT").to_owned()
        } else {
            let base = self.get(base_key);
            if base.is_empty() {
                return Err(ConfigError(format!(
                    "请配置 {base_key} 或 YUKII_EMBEDDING_ENDPOINT"
                )));
            }
            format!("{}/{path}", base.trim_end_matches('/'))
        };
        let endpoint = Url::parse(&address).map_err(|_| ConfigError("向量 API 地址无效".into()))?;
        let local = matches!(
            endpoint.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]")
        );
        if (endpoint.scheme() != "https" && !(endpoint.scheme() == "http" && local))
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(ConfigError(
                "向量 API 必须是 HTTPS（回环可 HTTP），不能含凭据或查询参数".into(),
            ));
        }
        Ok(Some(EmbeddingConfig {
            format,
            endpoint,
            api_key: key.into(),
            model: model.into(),
            dimensions: self.number("YUKII_EMBEDDING_DIMENSIONS", 1024, 1, 4096)? as usize,
            timeout_seconds: self.number("YUKII_EMBEDDING_TIMEOUT_SECONDS", 60, 1, 300)?,
        }))
    }
}
