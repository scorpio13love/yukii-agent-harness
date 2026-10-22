use super::{RagService, text};
use std::collections::BTreeMap;
use yukii_agent_ports::{
    embedding::{EmbeddingPurpose, normalize},
    knowledge::StoredVector,
    repository::RepositoryError,
};
use yukii_contracts::knowledge::{KnowledgeHit, KnowledgeSearch};

impl RagService {
    pub async fn search_report(&self, query: &str) -> Result<KnowledgeSearch, RepositoryError> {
        if query.trim().is_empty() || query.chars().count() > 12000 {
            return Err(RepositoryError::InvalidRequest("检索内容需为 1–12000 字"));
        }
        let _permit = self
            .search_slots
            .acquire()
            .await
            .map_err(|_| RepositoryError::Backend)?;
        let expression = text::query_expression(query);
        let repository = self.repository.clone();
        let lexical = tokio::task::spawn_blocking(move || repository.search(&expression, 30))
            .await
            .map_err(|_| RepositoryError::Backend)??;
        let Some(model) = &self.embedding else {
            return Ok(report("lexical_bm25", lexical, vec![]));
        };
        let status = self.index_status().await?;
        let mut warnings = Vec::new();
        if status.indexed_chunks < status.total_chunks {
            warnings.push(format!(
                "向量索引未完整：已索引 {} / {}，失败 {}。本次检索可能遗漏相关资料。",
                status.indexed_chunks, status.total_chunks, status.failed_chunks
            ));
        }
        if status.indexed_chunks == 0 {
            if status.total_chunks > 0 {
                warnings.push("本次仅使用关键词检索；可在知识库查看向量索引状态。".into());
            }
            return Ok(report("lexical_bm25", lexical, warnings));
        }
        let profile = model.profile();
        let vectors = model
            .embed(&[query.to_owned()], EmbeddingPurpose::Query)
            .await?;
        if vectors.len() != 1 {
            return Err(yukii_agent_ports::embedding::EmbeddingError::Protocol.into());
        }
        let query_vector = normalize(&vectors[0], profile.dimensions)?;
        let repository = self.repository.clone();
        let semantic = tokio::task::spawn_blocking(move || {
            let stored = repository.vectors(&profile)?;
            Ok::<_, RepositoryError>(semantic_rank(stored, &query_vector))
        })
        .await
        .map_err(|_| RepositoryError::Backend)??;
        Ok(report("hybrid_rrf", fuse(lexical, semantic), warnings))
    }
}
fn semantic_rank(vectors: Vec<StoredVector>, query: &[f32]) -> Vec<KnowledgeHit> {
    let mut ranked = vectors
        .into_iter()
        .map(|stored| {
            let score = stored
                .vector
                .iter()
                .zip(query)
                .map(|(a, b)| f64::from(*a) * f64::from(*b))
                .sum::<f64>();
            (score, stored.source)
        })
        .filter(|(score, _)| *score >= 0.35)
        .collect::<Vec<_>>();
    ranked.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| a.1.chunk_id.cmp(&b.1.chunk_id))
    });
    ranked
        .into_iter()
        .take(30)
        .map(|(_, source)| source)
        .collect()
}
/// Reciprocal Rank Fusion 按名次融合不同量纲的分数，同一片段只出现一次。
fn fuse(lexical: Vec<KnowledgeHit>, semantic: Vec<KnowledgeHit>) -> Vec<KnowledgeHit> {
    let mut scores: BTreeMap<String, (f64, KnowledgeHit)> = BTreeMap::new();
    for list in [lexical, semantic] {
        for (index, source) in list.into_iter().enumerate() {
            let weight = 1.0 / (60.0 + index as f64 + 1.0);
            let entry = scores
                .entry(source.chunk_id.clone())
                .or_insert((0.0, source));
            entry.0 += weight;
        }
    }
    let mut ranked = scores.into_values().collect::<Vec<_>>();
    ranked.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| a.1.chunk_id.cmp(&b.1.chunk_id))
    });
    ranked.into_iter().map(|(_, source)| source).collect()
}
fn report(
    mode: &'static str,
    sources: Vec<KnowledgeHit>,
    warnings: Vec<String>,
) -> KnowledgeSearch {
    let sources = sources
        .into_iter()
        .take(6)
        .enumerate()
        .map(|(index, mut source)| {
            source.citation = format!("K{}", index + 1);
            source
        })
        .collect();
    KnowledgeSearch {
        mode,
        sources,
        warnings,
    }
}
