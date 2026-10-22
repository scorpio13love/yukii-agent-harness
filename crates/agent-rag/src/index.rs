use super::RagService;
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use yukii_agent_ports::{
    embedding::{EmbeddingError, EmbeddingPurpose, normalize},
    repository::RepositoryError,
};
use yukii_contracts::knowledge::KnowledgeIndexStatus;

impl RagService {
    pub async fn index_status(&self) -> Result<KnowledgeIndexStatus, RepositoryError> {
        let profile = self.embedding.as_ref().map(|model| model.profile());
        let id = profile.as_ref().map(|p| p.id.clone()).unwrap_or_default();
        let repository = self.repository.clone();
        let counts = tokio::task::spawn_blocking(move || repository.index_counts(&id))
            .await
            .map_err(|_| RepositoryError::Backend)??;
        Ok(KnowledgeIndexStatus {
            paused: self.index_paused.load(std::sync::atomic::Ordering::Acquire)
                || counts.failed > 0,
            configured: profile.is_some(),
            model: profile.as_ref().map(|p| p.model.clone()),
            dimensions: profile.as_ref().map(|p| p.dimensions),
            total_chunks: counts.total,
            indexed_chunks: counts.indexed,
            failed_chunks: counts.failed,
            pending_chunks: counts.total.saturating_sub(counts.indexed + counts.failed),
        })
    }
    /// 重试是用户显式动作，只清除当前配置的失败记录，不丢弃已经成功的向量。
    pub async fn retry_index(&self) -> Result<KnowledgeIndexStatus, RepositoryError> {
        let _guard = self.index_lock.lock().await;
        let profile = self
            .embedding
            .as_ref()
            .ok_or(RepositoryError::InvalidRequest("尚未配置向量模型"))?
            .profile();
        let repository = self.repository.clone();
        tokio::task::spawn_blocking(move || repository.retry_embeddings(&profile.id))
            .await
            .map_err(|_| RepositoryError::Backend)??;
        self.index_paused
            .store(false, std::sync::atomic::Ordering::Release);
        self.wake.notify_one();
        self.index_status().await
    }
    /// 缺少当前配置向量的片段就是持久待处理队列；每批原子提交，重启只处理未提交片段。
    pub async fn index_batch(&self) -> Result<usize, RepositoryError> {
        let _guard = self.index_lock.lock().await;
        if self.index_paused.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(0);
        }
        let result = self.index_pending_batch().await;
        // 在释放索引锁之前完成暂停转换，避免迟到失败覆盖用户刚提交的重试。
        if result.is_err() {
            self.index_paused
                .store(true, std::sync::atomic::Ordering::Release);
        }
        result
    }
    async fn index_pending_batch(&self) -> Result<usize, RepositoryError> {
        let Some(model) = &self.embedding else {
            return Ok(0);
        };
        let profile = model.profile();
        let repository = self.repository.clone();
        let id = profile.id.clone();
        let chunks = tokio::task::spawn_blocking(move || repository.pending_embeddings(&id, 10))
            .await
            .map_err(|_| RepositoryError::Backend)??;
        if chunks.is_empty() {
            return Ok(0);
        }
        let inputs = chunks
            .iter()
            .map(|c| format!("{}\n{}", c.title, c.text))
            .collect::<Vec<_>>();
        let result = model
            .embed(&inputs, EmbeddingPurpose::Document)
            .await
            .and_then(|vectors| {
                if vectors.len() != chunks.len() {
                    return Err(EmbeddingError::Protocol);
                }
                vectors
                    .iter()
                    .map(|vector| normalize(vector, profile.dimensions))
                    .collect::<Result<Vec<_>, _>>()
            });
        let repository = self.repository.clone();
        let count = chunks.len();
        match result {
            Ok(vectors) => tokio::task::spawn_blocking(move || {
                repository.save_embeddings(&profile, &chunks, &vectors)
            })
            .await
            .map_err(|_| RepositoryError::Backend)??,
            Err(error) => {
                let message = error.to_string();
                tokio::task::spawn_blocking(move || {
                    repository.fail_embeddings(&profile.id, &chunks, &message)
                })
                .await
                .map_err(|_| RepositoryError::Backend)??;
                return Err(error.into());
            }
        }
        Ok(count)
    }
    /// 一旦出现模型失败即暂停该配置的索引，等待明确重试，避免认证错误引发付费重试风暴。
    pub async fn serve_indexer(self: Arc<Self>, stop: CancellationToken) {
        if self.embedding.is_none() {
            return;
        }
        loop {
            let result = tokio::select! {
                biased;
                _=stop.cancelled()=>return,
                result=async {
                    if self.index_status().await?.paused { return Ok(0); }
                    self.index_batch().await
                }=>result,
            };
            match result {
                Ok(count) if count > 0 => continue,
                Err(error) => {
                    tracing::warn!(%error,"向量索引暂停，等待显式重试");
                }
                _ => {}
            }
            tokio::select! {
                _=stop.cancelled()=>return,
                _=self.wake.notified()=>{},
                _=tokio::time::sleep(Duration::from_secs(2))=>{},
            }
        }
    }
}
