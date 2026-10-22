//! 显式的真实百炼验收；仅读取配置，发送本文件中的虚构资料，不打印密钥或完整向量。
use std::{path::Path, sync::Arc};
use yukii_agent_ports::{embedding::EmbeddingModel, knowledge::KnowledgeRepository};
use yukii_agent_rag::RagService;
use yukii_api_server::store::TaskStore;
use yukii_contracts::knowledge::ImportDocument;
use yukii_model_gateway::{config::Settings, embedding::HttpEmbedding};

#[tokio::test]
#[ignore = "uses configured paid embedding API; run explicitly"]
async fn configured_embedding_recalls_synonymous_text() {
    let settings = Settings::load(Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../.env"
    )))
    .unwrap();
    let model = Arc::new(
        HttpEmbedding::new(
            settings
                .embedding()
                .unwrap()
                .expect("embedding configuration required"),
        )
        .unwrap(),
    );
    let profile = model.profile();
    let store = Arc::new(TaskStore::open(":memory:").unwrap());
    let rag = RagService::with_embedding(store.clone(), Some(model));
    let mut expected = String::new();
    for (title, content) in [
        (
            "语义验收 A（虚构）",
            "员工出差入住酒店，单晚费用不得超过 520 元。",
        ),
        (
            "语义验收 B（虚构）",
            "公司食堂午餐供应时间为十一点三十分至十三点。",
        ),
        ("语义验收 C（虚构）", "研发代码每周五进行静态检查和备份。"),
    ] {
        let doc = rag
            .import(ImportDocument {
                title: title.into(),
                content: content.into(),
            })
            .await
            .unwrap();
        if expected.is_empty() {
            expected = doc.id;
        }
    }
    assert_eq!(rag.index_batch().await.unwrap(), 3);
    assert!(
        store
            .search("\"住宿\" OR \"预算\" OR \"商务\" OR \"旅行\"", 6)
            .unwrap()
            .is_empty()
    );
    let report = rag
        .search_report("商务旅行住宿预算的最高金额是多少？")
        .await
        .unwrap();
    assert_eq!(report.mode, "hybrid_rrf");
    assert_eq!(report.sources.first().unwrap().document_id, expected);
    assert!(report.sources[0].text.contains("520"));
    println!(
        "真实向量验收通过：模型 {}，{} 维，已索引 3 条虚构资料，同义查询首位命中预期资料。",
        profile.model, profile.dimensions
    );
}
