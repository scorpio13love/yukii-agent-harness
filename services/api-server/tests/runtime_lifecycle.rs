use async_trait::async_trait;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use yukii_agent_ports::model::{
    LanguageModel, Message, ModelError, ModelReply, ToolDefinition, Usage,
};
use yukii_api_server::{executor::Executor, store::TaskStore};
use yukii_contracts::{CreateTask, ExecutionTarget, TaskStatus};

struct WaitingModel {
    entered: Arc<AtomicUsize>,
}
#[async_trait]
impl LanguageModel for WaitingModel {
    async fn complete(
        &self,
        _: &str,
        _: &[Message],
        _: &[ToolDefinition],
    ) -> Result<ModelReply, ModelError> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
}
struct FinalModel;
#[async_trait]
impl LanguageModel for FinalModel {
    async fn complete(
        &self,
        _: &str,
        _: &[Message],
        _: &[ToolDefinition],
    ) -> Result<ModelReply, ModelError> {
        Ok(ModelReply {
            text: "done".into(),
            calls: vec![],
            usage: Usage::default(),
        })
    }
}
async fn wait_for(mut check: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn shutdown_drains_active_jobs_and_restart_runs_only_pending_jobs() {
    let store = Arc::new(TaskStore::open(":memory:").unwrap());
    let tasks = (0..3)
        .map(|index| {
            store
                .create_once(
                    CreateTask {
                        conversation_id: None,
                        prompt: "work".into(),
                        target: ExecutionTarget::CloudOffice,
                    },
                    &index.to_string(),
                    true,
                )
                .unwrap()
                .task
        })
        .collect::<Vec<_>>();
    let entered = Arc::new(AtomicUsize::new(0));
    let executor = Executor::new(
        store.clone(),
        Some(Arc::new(WaitingModel {
            entered: entered.clone(),
        })),
        Default::default(),
    );
    let worker = tokio::spawn(executor.clone().serve());
    wait_for(|| entered.load(Ordering::SeqCst) == 2).await;
    assert_eq!(
        store.get(&tasks[2].id).unwrap().status,
        TaskStatus::Planning
    );
    executor.shutdown();
    tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .unwrap()
        .unwrap();
    assert!(!executor.accepting());
    assert!(executor.start(tasks[2].id.clone()).await.is_err());
    for task in &tasks[..2] {
        assert_eq!(store.get(&task.id).unwrap().status, TaskStatus::Cancelled);
    }
    assert_eq!(store.recover_interrupted().unwrap(), 0);
    let resumed = Executor::new(
        store.clone(),
        Some(Arc::new(FinalModel)),
        Default::default(),
    );
    let worker = tokio::spawn(resumed.clone().serve());
    wait_for(|| store.get(&tasks[2].id).unwrap().status == TaskStatus::Completed).await;
    resumed.shutdown();
    worker.await.unwrap();
    assert_eq!(
        entered.load(Ordering::SeqCst),
        2,
        "取消的旧任务不得重新请求模型"
    );
}

struct PanickingModel;
#[async_trait]
impl LanguageModel for PanickingModel {
    async fn complete(
        &self,
        _: &str,
        _: &[Message],
        _: &[ToolDefinition],
    ) -> Result<ModelReply, ModelError> {
        panic!("test worker failure")
    }
}
#[tokio::test]
async fn worker_panic_is_supervised_and_persisted_as_failed() {
    let store = Arc::new(TaskStore::open(":memory:").unwrap());
    let task = store
        .create_once(
            CreateTask {
                conversation_id: None,
                prompt: "work".into(),
                target: ExecutionTarget::CloudOffice,
            },
            "panic",
            true,
        )
        .unwrap()
        .task;
    let executor = Executor::new(
        store.clone(),
        Some(Arc::new(PanickingModel)),
        Default::default(),
    );
    let worker = tokio::spawn(executor.clone().serve());
    wait_for(|| store.get(&task.id).unwrap().status == TaskStatus::Failed).await;
    assert!(store.get(&task.id).unwrap().error.unwrap().contains("异常"));
    executor.shutdown();
    worker.await.unwrap();
}
