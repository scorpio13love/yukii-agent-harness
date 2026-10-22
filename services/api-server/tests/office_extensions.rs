//! 以真实 SQLite 和打包市场验证组件边界；普通测试不依赖 Docker 或真实模型密钥。
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use yukii_agent_extensions::{ExtensionManager, marketplace::Marketplace};
use yukii_agent_ports::{
    context::{ContextProvider, PreparedContext},
    model::{LanguageModel, Message, ModelError, ModelReply, ToolDefinition, Usage},
    repository::RepositoryError,
    sandbox::{Sandbox, SandboxRequest, SandboxStatus},
    tools::{ToolCatalog, ToolContext, ToolEffect},
};
use yukii_agent_rag::RagService;
use yukii_agent_runtime::{Components, Executor};
use yukii_api_server::store::TaskStore;
use yukii_contracts::{
    CreateTask, ExecutionTarget, TaskStatus,
    extensions::{InstallPlugin, SetPluginEnabled},
    knowledge::ImportDocument,
};

fn marketplace() -> Arc<Marketplace> {
    Arc::new(
        Marketplace::new(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/marketplace"),
            None,
        )
        .unwrap(),
    )
}
#[derive(Default)]
struct RecordingSandbox(Mutex<Vec<Value>>);
#[async_trait]
impl Sandbox for RecordingSandbox {
    async fn status(&self) -> SandboxStatus {
        SandboxStatus {
            configured: true,
            available: true,
            backend: "test".into(),
            message: "test".into(),
        }
    }
    async fn execute(&self, request: SandboxRequest) -> Result<Value, String> {
        self.0.lock().unwrap().push(request.input);
        Ok(json!({"executed": true}))
    }
    async fn shutdown(&self) {}
}

#[tokio::test]
async fn plugin_requires_approval_and_revokes_cached_tools_on_disable() {
    let store = Arc::new(TaskStore::open(":memory:").unwrap());
    let sandbox = Arc::new(RecordingSandbox::default());
    let manager = ExtensionManager::new(store, marketplace(), sandbox.clone());
    let preview = manager
        .marketplace()
        .preview("text_utilities", "1.0.0")
        .await
        .unwrap();
    let input = || InstallPlugin {
        id: preview.manifest.id.clone(),
        version: preview.manifest.version.clone(),
        sha256: preview.sha256.clone(),
        approved_permissions: preview.manifest.permissions.clone(),
        expected_previous_sha256: None,
    };
    let mut rejected = input();
    rejected.approved_permissions.clear();
    assert!(manager.install(rejected).await.is_err());
    assert!(manager.installed().await.unwrap().is_empty());
    let installed = manager.install(input()).await.unwrap();
    assert!(!installed.enabled);
    assert!(
        manager
            .tools()
            .await
            .unwrap()
            .iter()
            .all(|tool| !tool.definition().name.starts_with("ext__"))
    );
    let enabled = || SetPluginEnabled {
        enabled: true,
        sha256: preview.sha256.clone(),
        approved_permissions: preview.manifest.permissions.clone(),
    };
    let mut wrong_hash = enabled();
    wrong_hash.sha256 = "0".repeat(64);
    assert!(
        manager
            .set_enabled("text_utilities".into(), wrong_hash)
            .await
            .is_err()
    );
    manager
        .set_enabled("text_utilities".into(), enabled())
        .await
        .unwrap();
    let tool = manager
        .tools()
        .await
        .unwrap()
        .into_iter()
        .find(|tool| tool.definition().name == "ext__text_utilities__text_stats")
        .unwrap();
    let context = ToolContext::office("test", "call-1");
    assert!(
        tool.execute(&context, json!({"text":"你好", "extra":true}))
            .await
            .is_err()
    );
    assert!(tool.execute(&context, json!({"text":123})).await.is_err());
    assert!(
        tool.execute(&context, json!({"text":"x".repeat(10001)}))
            .await
            .is_err()
    );
    assert!(sandbox.0.lock().unwrap().is_empty());
    assert!(matches!(
        tool.execute(&context, json!({"text":"你好"}))
            .await
            .unwrap(),
        ToolEffect::Result(_)
    ));
    manager
        .set_enabled(
            "text_utilities".into(),
            SetPluginEnabled {
                enabled: false,
                sha256: preview.sha256,
                approved_permissions: vec![],
            },
        )
        .await
        .unwrap();
    assert!(
        tool.execute(&context, json!({"text":"late"}))
            .await
            .is_err()
    );
    assert_eq!(sandbox.0.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn agent_downloads_disabled_skill_and_cannot_load_until_enabled() {
    let manager = ExtensionManager::new(
        Arc::new(TaskStore::open(":memory:").unwrap()),
        marketplace(),
        Arc::new(RecordingSandbox::default()),
    );
    let preview = manager
        .marketplace()
        .preview("office_writing", "1.0.0")
        .await
        .unwrap();
    let tools = manager.tools().await.unwrap();
    let download = tools
        .iter()
        .find(|tool| tool.definition().name == "download_plugin")
        .unwrap();
    let load = tools
        .iter()
        .find(|tool| tool.definition().name == "load_skill")
        .unwrap();
    let context = ToolContext::office("test", "call-1");
    download
        .execute(
            &context,
            json!({"id":"office_writing","version":"1.0.0","sha256":preview.sha256}),
        )
        .await
        .unwrap();
    assert!(!manager.installed().await.unwrap()[0].enabled);
    let args = json!({"plugin_id":"office_writing","skill_id":"weekly_brief"});
    assert!(load.execute(&context, args.clone()).await.is_err());
    manager
        .set_enabled(
            "office_writing".into(),
            SetPluginEnabled {
                enabled: true,
                sha256: preview.sha256,
                approved_permissions: vec![],
            },
        )
        .await
        .unwrap();
    let ToolEffect::Result(result) = load.execute(&context, args).await.unwrap() else {
        panic!("expected skill")
    };
    assert_eq!(result["trust"], "reference_only");
    assert!(
        result["skill"]["instructions"]
            .as_str()
            .unwrap()
            .contains("事实")
    );
}

#[tokio::test]
async fn chinese_retrieval_is_deduplicated_and_delete_cleans_index() {
    let store = Arc::new(TaskStore::open(":memory:").unwrap());
    let rag = RagService::new(store);
    let input = || ImportDocument {
        title: "差旅制度".into(),
        content: "差旅住宿报销上限为每天 520 元。".into(),
    };
    let document = rag.import(input()).await.unwrap();
    assert_eq!(rag.import(input()).await.unwrap().id, document.id);
    assert_eq!(rag.documents().await.unwrap().len(), 1);
    let hits = rag.search("住宿报销上限").await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].citation, "K1");
    assert!(hits[0].text.contains("520"));
    // FTS 特殊语法作为普通文字处理，不允许查询构造错误或 SQL 注入。
    rag.search("\" OR * NOT (住宿) '").await.unwrap();
    assert!(rag.search("火星基地").await.unwrap().is_empty());
    rag.delete(document.id.clone()).await.unwrap();
    assert!(rag.search("住宿").await.unwrap().is_empty());
    assert!(rag.documents().await.unwrap().is_empty());
    assert!(rag.delete(document.id).await.is_err());
}

struct InspectingModel;
#[async_trait]
impl LanguageModel for InspectingModel {
    async fn complete(
        &self,
        _system: &str,
        messages: &[Message],
        _: &[ToolDefinition],
    ) -> Result<ModelReply, ModelError> {
        let serialized = format!("{messages:?}");
        assert!(serialized.contains("520"));
        assert!(serialized.contains("K1"));
        assert!(serialized.contains("测试记忆"));
        Ok(ModelReply {
            text: "住宿上限为 520 元 [K1]。".into(),
            calls: vec![],
            usage: Usage::default(),
        })
    }
}
#[derive(Default)]
struct TestMemory(Mutex<Vec<String>>);
#[async_trait]
impl ContextProvider for TestMemory {
    fn name(&self) -> &str {
        "test_memory"
    }
    async fn prepare(&self, _: &str, _: &str) -> Result<PreparedContext, RepositoryError> {
        Ok(PreparedContext {
            content: "测试记忆：偏好简洁回复".into(),
            sources: vec![],
            tool_allowlist: vec![],
        })
    }
    async fn remember(&self, _: &str, _: &str, answer: &str) -> Result<(), RepositoryError> {
        self.0.lock().unwrap().push(answer.into());
        Ok(())
    }
}
#[tokio::test]
async fn runtime_composes_rag_and_memory_and_preserves_citations_after_deletion() {
    let store = Arc::new(TaskStore::open(":memory:").unwrap());
    let rag = Arc::new(RagService::new(store.clone()));
    let document = rag
        .import(ImportDocument {
            title: "差旅制度".into(),
            content: "住宿报销上限 520 元。".into(),
        })
        .await
        .unwrap();
    let memory = Arc::new(TestMemory::default());
    let manager = Arc::new(ExtensionManager::new(
        store.clone(),
        marketplace(),
        Arc::new(RecordingSandbox::default()),
    ));
    let executor = Executor::with_components(
        store.clone(),
        Components {
            model: Some(Arc::new(InspectingModel)),
            tools: manager,
            contexts: vec![rag.clone(), memory.clone()],
            system_prompt: None,
            context_window_tokens: 128_000,
            max_output_tokens: 8_192,
            compaction_headroom_tokens: 16_384,
        },
        Default::default(),
    );
    let task = store
        .create_once(
            CreateTask {
                conversation_id: None,
                prompt: "住宿报销上限？".into(),
                target: ExecutionTarget::CloudOffice,
            },
            "rag-test",
            true,
        )
        .unwrap()
        .task;
    let worker = tokio::spawn(executor.clone().serve());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current = store.get(&task.id).unwrap();
            if matches!(current.status, TaskStatus::Completed | TaskStatus::Failed) {
                assert_eq!(current.status, TaskStatus::Completed, "{:?}", current.error);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    executor.shutdown();
    worker.await.unwrap();
    rag.delete(document.id).await.unwrap();
    assert_eq!(memory.0.lock().unwrap().len(), 1);
    assert_eq!(store.get(&task.id).unwrap().sources[0].citation, "K1");
    assert!(store.get(&task.id).unwrap().sources[0].text.contains("520"));
}
