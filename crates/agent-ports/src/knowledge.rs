use crate::embedding::EmbeddingProfile;
use crate::repository::RepositoryError;
use yukii_contracts::knowledge::{KnowledgeDocument, KnowledgeHit};

pub struct EmbeddingChunk {
    pub id: String,
    pub title: String,
    pub text: String,
}
pub struct StoredVector {
    pub source: KnowledgeHit,
    pub vector: Vec<f32>,
}
#[derive(Default)]
pub struct IndexCounts {
    pub total: usize,
    pub indexed: usize,
    pub failed: usize,
}

pub struct IndexedChunk {
    pub text: String,
    pub terms: String,
}

/// 索引写入与原文元数据原子保存；查询条件由检索服务生成而非拼接用户 SQL。
pub trait VectorRepository: Send + Sync {
    fn index_counts(&self, profile: &str) -> Result<IndexCounts, RepositoryError>;
    fn pending_embeddings(
        &self,
        profile: &str,
        limit: usize,
    ) -> Result<Vec<EmbeddingChunk>, RepositoryError>;
    fn save_embeddings(
        &self,
        profile: &EmbeddingProfile,
        chunks: &[EmbeddingChunk],
        vectors: &[Vec<f32>],
    ) -> Result<(), RepositoryError>;
    fn fail_embeddings(
        &self,
        profile: &str,
        chunks: &[EmbeddingChunk],
        error: &str,
    ) -> Result<(), RepositoryError>;
    fn retry_embeddings(&self, profile: &str) -> Result<(), RepositoryError>;
    fn vectors(&self, profile: &EmbeddingProfile) -> Result<Vec<StoredVector>, RepositoryError>;
}

pub trait KnowledgeRepository: VectorRepository + Send + Sync {
    fn import_document(
        &self,
        title: &str,
        chunks: &[IndexedChunk],
        fingerprint: &str,
    ) -> Result<KnowledgeDocument, RepositoryError>;
    fn documents(&self) -> Result<Vec<KnowledgeDocument>, RepositoryError>;
    fn delete_document(&self, id: &str) -> Result<(), RepositoryError>;
    fn search(&self, expression: &str, limit: usize) -> Result<Vec<KnowledgeHit>, RepositoryError>;
}
