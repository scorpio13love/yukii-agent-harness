//! 向量模型端口。文本与未来多模态输入分别演进，不伪装当前文本能力。
use async_trait::async_trait;
use thiserror::Error;

#[derive(Clone, Debug)]
pub struct EmbeddingProfile {
    /// 包含端点、模型、维度和预处理版本的标识；密钥不参与计算。
    pub id: String,
    pub model: String,
    pub dimensions: usize,
}

#[derive(Clone, Copy)]
pub enum EmbeddingPurpose {
    Document,
    Query,
}

#[derive(Debug, Error)]
pub enum EmbeddingError {
    #[error("向量请求超时")]
    Timeout,
    #[error("无法连接向量服务")]
    Transport,
    #[error("向量服务返回 HTTP {0}，请检查模型、密钥、地域与额度")]
    Http(u16),
    #[error("向量响应数量、索引、维度或数值不合法")]
    Protocol,
    #[error("向量输入超过当前批次或长度限制")]
    Input,
}

#[async_trait]
pub trait EmbeddingModel: Send + Sync {
    fn profile(&self) -> EmbeddingProfile;
    async fn embed(
        &self,
        texts: &[String],
        purpose: EmbeddingPurpose,
    ) -> Result<Vec<Vec<f32>>, EmbeddingError>;
}

/// 校验后归一化，使数据库保存和余弦检索使用相同约定；拒绝 NaN/Inf 和零向量。
pub fn normalize(vector: &[f32], dimensions: usize) -> Result<Vec<f32>, EmbeddingError> {
    if vector.len() != dimensions
        || dimensions == 0
        || dimensions > 4096
        || vector.iter().any(|v| !v.is_finite())
    {
        return Err(EmbeddingError::Protocol);
    }
    let norm = vector
        .iter()
        .map(|v| f64::from(*v).powi(2))
        .sum::<f64>()
        .sqrt();
    if norm <= f64::EPSILON {
        return Err(EmbeddingError::Protocol);
    }
    Ok(vector
        .iter()
        .map(|v| (f64::from(*v) / norm) as f32)
        .collect())
}
