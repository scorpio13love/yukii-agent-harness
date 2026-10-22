//! 以可控向量验证排名、持久化与失败状态，真实供应商连通性另用显式 ignored 测试。
use async_trait::async_trait;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use yukii_agent_ports::{
    embedding::{EmbeddingError, EmbeddingModel, EmbeddingProfile, EmbeddingPurpose},
    knowledge::{KnowledgeRepository, VectorRepository},
};
use yukii_agent_rag::RagService;
use yukii_api_server::store::TaskStore;
use yukii_contracts::knowledge::ImportDocument;

struct FakeEmbedding {
    fail: AtomicBool,
    calls: AtomicUsize,
    id: String,
}
impl FakeEmbedding {
    fn new(id: &str) -> Self {
        Self {
            fail: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            id: id.into(),
        }
    }
}
#[async_trait]
impl EmbeddingModel for FakeEmbedding {
    fn profile(&self) -> EmbeddingProfile {
        EmbeddingProfile {
            id: self.id.clone(),
            model: "test-vector".into(),
            dimensions: 2,
        }
    }
    async fn embed(
        &self,
        texts: &[String],
        purpose: EmbeddingPurpose,
    ) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(EmbeddingError::Http(401));
        }
        Ok(texts
            .iter()
            .map(|text| {
                if matches!(purpose, EmbeddingPurpose::Query) || text.contains("酒店") {
                    vec![1.0, 0.0]
                } else {
                    vec![0.0, 1.0]
                }
            })
            .collect())
    }
}
async fn import(rag: &RagService, title: &str, content: &str) -> String {
    rag.import(ImportDocument {
        title: title.into(),
        content: content.into(),
    })
    .await
    .unwrap()
    .id
}

#[tokio::test]
async fn semantic_only_match_is_persisted_and_profiles_never_mix() {
    let path = std::env::temp_dir().join(format!("yukii-vector-{}.sqlite", uuid::Uuid::new_v4()));
    let model = Arc::new(FakeEmbedding::new("first"));
    let document;
    {
        let store = Arc::new(TaskStore::open(&path).unwrap());
        let rag = RagService::with_embedding(store.clone(), Some(model.clone()));
        document = import(&rag, "差旅政策", "酒店每日费用 520 元。").await;
        import(&rag, "研发", "程序代码定期备份。").await;
        assert_eq!(rag.index_status().await.unwrap().pending_chunks, 2);
        assert_eq!(rag.index_batch().await.unwrap(), 2);
        assert_eq!(rag.index_batch().await.unwrap(), 0);
        assert!(store.search("\"住宿\"", 6).unwrap().is_empty());
        let report = rag.search_report("住宿预算").await.unwrap();
        assert_eq!(report.mode, "hybrid_rrf");
        assert!(report.warnings.is_empty());
        assert_eq!(report.sources.len(), 1);
        assert_eq!(report.sources[0].document_id, document);
        assert_eq!(report.sources[0].citation, "K1");
        // 关键词与向量同时命中时，融合结果不能重复引用同一片段。
        assert_eq!(rag.search("酒店").await.unwrap().len(), 1);
    }
    {
        let store = Arc::new(TaskStore::open(&path).unwrap());
        let rag = RagService::with_embedding(store.clone(), Some(model));
        assert_eq!(rag.index_status().await.unwrap().indexed_chunks, 2);
        let changed =
            RagService::with_embedding(store.clone(), Some(Arc::new(FakeEmbedding::new("second"))));
        assert_eq!(changed.index_status().await.unwrap().indexed_chunks, 0);
        let report = changed.search_report("住宿预算").await.unwrap();
        assert_eq!(report.mode, "lexical_bm25");
        assert!(!report.warnings.is_empty());
        rag.delete(document).await.unwrap();
        assert_eq!(store.index_counts("first").unwrap().indexed, 1);
        assert!(rag.search("住宿预算").await.unwrap().is_empty());
    }
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn failed_batches_require_retry_and_queries_never_silently_fallback() {
    let store = Arc::new(TaskStore::open(":memory:").unwrap());
    let model = Arc::new(FakeEmbedding::new("first"));
    let rag = RagService::with_embedding(store.clone(), Some(model.clone()));
    import(&rag, "差旅政策", "酒店每日费用 520 元。").await;
    model.fail.store(true, Ordering::SeqCst);
    assert!(rag.index_batch().await.is_err());
    assert_eq!(rag.index_status().await.unwrap().failed_chunks, 1);
    assert_eq!(rag.index_batch().await.unwrap(), 0);
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert!(!rag.search_report("酒店").await.unwrap().warnings.is_empty());
    model.fail.store(false, Ordering::SeqCst);
    rag.retry_index().await.unwrap();
    assert_eq!(rag.index_batch().await.unwrap(), 1);
    model.fail.store(true, Ordering::SeqCst);
    assert!(rag.search_report("酒店").await.is_err());
}

#[tokio::test]
async fn deleted_chunks_are_not_resurrected_by_late_embedding_completion() {
    let store = Arc::new(TaskStore::open(":memory:").unwrap());
    let rag = RagService::new(store.clone());
    let id = import(&rag, "测试", "酒店").await;
    let chunks = store.pending_embeddings("first", 10).unwrap();
    rag.delete(id).await.unwrap();
    let profile = FakeEmbedding::new("first").profile();
    store
        .save_embeddings(&profile, &chunks, &[vec![1.0, 0.0]])
        .unwrap();
    store
        .fail_embeddings("first", &chunks, "test error")
        .unwrap();
    assert_eq!(store.index_counts("first").unwrap().total, 0);
    assert_eq!(store.index_counts("first").unwrap().failed, 0);
    assert!(store.vectors(&profile).unwrap().is_empty());
}

#[tokio::test]
async fn malformed_batch_never_partially_commits() {
    let store = Arc::new(TaskStore::open(":memory:").unwrap());
    let rag = RagService::new(store.clone());
    import(&rag, "一", "酒店").await;
    import(&rag, "二", "代码").await;
    let chunks = store.pending_embeddings("first", 10).unwrap();
    let profile = FakeEmbedding::new("first").profile();
    assert!(
        store
            .save_embeddings(&profile, &chunks, &[vec![1.0, 0.0], vec![f32::NAN, 1.0]])
            .is_err()
    );
    assert_eq!(store.index_counts("first").unwrap().indexed, 0);
}

#[tokio::test]
async fn background_indexer_resumes_pending_work_and_requires_explicit_retry() {
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    let store = Arc::new(TaskStore::open(":memory:").unwrap());
    // 模拟服务启动前已保存的资料，后台任务必须自行发现它。
    import(&RagService::new(store.clone()), "差旅", "酒店费用").await;
    let model = Arc::new(FakeEmbedding::new("worker"));
    model.fail.store(true, Ordering::SeqCst);
    let rag = Arc::new(RagService::with_embedding(store, Some(model.clone())));
    let stop = CancellationToken::new();
    let worker = tokio::spawn(rag.clone().serve_indexer(stop.clone()));
    tokio::time::timeout(Duration::from_secs(3), async {
        while !rag.index_status().await.unwrap().paused {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // 新导入唤醒也不能绕过暂停，否则配置错误会引发持续付费重试。
    import(&rag, "研发", "代码备份").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    model.fail.store(false, Ordering::SeqCst);
    rag.retry_index().await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while rag.index_status().await.unwrap().indexed_chunks != 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(1), worker)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
}
