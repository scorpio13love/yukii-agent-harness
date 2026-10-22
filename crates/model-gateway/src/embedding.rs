//! 百炼文本向量网关：OpenAI 兼容与 DashScope 原生协议，不自动重试付费请求。
mod config;
use async_trait::async_trait;
pub use config::{EmbeddingConfig, EmbeddingFormat};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;
use yukii_agent_ports::embedding::{
    EmbeddingError, EmbeddingModel, EmbeddingProfile, EmbeddingPurpose, normalize,
};

pub struct HttpEmbedding {
    config: EmbeddingConfig,
    client: reqwest::Client,
    slots: tokio::sync::Semaphore,
    profile: EmbeddingProfile,
}
impl HttpEmbedding {
    pub fn new(config: EmbeddingConfig) -> Result<Self, EmbeddingError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| EmbeddingError::Transport)?;
        let identity = json!([
            "text-title-chunk-v1",
            config.endpoint.as_str(),
            config.model,
            config.dimensions,
            format!("{:?}", config.format)
        ])
        .to_string();
        let profile = EmbeddingProfile {
            id: format!("{:x}", Sha256::digest(identity)),
            model: config.model.clone(),
            dimensions: config.dimensions,
        };
        Ok(Self {
            config,
            client,
            slots: tokio::sync::Semaphore::new(2),
            profile,
        })
    }
}
#[async_trait]
impl EmbeddingModel for HttpEmbedding {
    fn profile(&self) -> EmbeddingProfile {
        self.profile.clone()
    }
    async fn embed(
        &self,
        texts: &[String],
        purpose: EmbeddingPurpose,
    ) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty()
            || texts.len() > 10
            || texts
                .iter()
                .any(|s| s.trim().is_empty() || s.chars().count() > 12000)
        {
            return Err(EmbeddingError::Input);
        }
        let _permit = self
            .slots
            .acquire()
            .await
            .map_err(|_| EmbeddingError::Transport)?;
        let text_type = match purpose {
            EmbeddingPurpose::Document => "document",
            EmbeddingPurpose::Query => "query",
        };
        let body = match self.config.format {
            EmbeddingFormat::OpenAi => json!({
                "model": self.config.model,
                "input": texts,
                "dimensions": self.config.dimensions,
                "encoding_format": "float",
            }),
            EmbeddingFormat::DashScope => json!({
                "model": self.config.model,
                "input": {"texts": texts},
                "parameters": {
                    "dimension": self.config.dimensions,
                    "output_type": "dense",
                    "text_type": text_type,
                },
            }),
        };
        let mut response = self
            .client
            .post(self.config.endpoint.clone())
            .bearer_auth(&self.config.api_key)
            .json(&body)
            .send()
            .await
            .map_err(transport)?;
        if !response.status().is_success() {
            return Err(EmbeddingError::Http(response.status().as_u16()));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport)? {
            if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
                return Err(EmbeddingError::Protocol);
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| EmbeddingError::Protocol)?;
        decode(
            &value,
            self.config.format,
            texts.len(),
            self.config.dimensions,
        )
    }
}
fn transport(error: reqwest::Error) -> EmbeddingError {
    if error.is_timeout() {
        EmbeddingError::Timeout
    } else {
        EmbeddingError::Transport
    }
}
/// 按响应中的输入索引重新排序，禁止重复、缺项或维度变化污染持久化索引。
fn decode(
    value: &Value,
    format: EmbeddingFormat,
    count: usize,
    dimensions: usize,
) -> Result<Vec<Vec<f32>>, EmbeddingError> {
    let (items, index) = match format {
        EmbeddingFormat::OpenAi => (&value["data"], "index"),
        EmbeddingFormat::DashScope => (&value["output"]["embeddings"], "text_index"),
    };
    let items = items
        .as_array()
        .filter(|items| items.len() == count)
        .ok_or(EmbeddingError::Protocol)?;
    let mut ordered: Vec<Option<Vec<f32>>> = vec![None; count];
    for item in items {
        let index = item[index]
            .as_u64()
            .filter(|index| *index < count as u64)
            .ok_or(EmbeddingError::Protocol)? as usize;
        if ordered[index].is_some() {
            return Err(EmbeddingError::Protocol);
        }
        let vector: Vec<f32> = serde_json::from_value(item["embedding"].clone())
            .map_err(|_| EmbeddingError::Protocol)?;
        ordered[index] = Some(normalize(&vector, dimensions)?);
    }
    ordered
        .into_iter()
        .map(|vector| vector.ok_or(EmbeddingError::Protocol))
        .collect()
}
