//! 知识库协议：返回实际检索模式、索引进度和降级说明。
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportDocument {
    pub title: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDocument {
    pub id: String,
    pub title: String,
    pub chunk_count: u32,
    pub created_at: i64,
}

/// 引用保存实际检索片段，避免以后更新或删除原文后无法审计旧回复。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeHit {
    pub document_id: String,
    pub chunk_id: String,
    pub title: String,
    pub text: String,
    pub citation: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeQuery {
    pub query: String,
}

#[derive(Debug, Serialize)]
pub struct KnowledgeSearch {
    pub mode: &'static str,
    pub sources: Vec<KnowledgeHit>,
    pub warnings: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct KnowledgeIndexStatus {
    pub paused: bool,
    pub configured: bool,
    pub model: Option<String>,
    pub dimensions: Option<usize>,
    pub total_chunks: usize,
    pub indexed_chunks: usize,
    pub pending_chunks: usize,
    pub failed_chunks: usize,
}
