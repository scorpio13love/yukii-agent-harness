use std::sync::Arc;
use yukii_agent_infra::{StoreError, TaskStore};
use yukii_agent_ports::model::{ToolCall, ToolResult};
use yukii_contracts::{CreateTask, ExecutionTarget, TaskStatus};

fn input(prompt: &str) -> CreateTask {
    CreateTask {
        conversation_id: None,
        prompt: prompt.into(),
        target: ExecutionTarget::CloudOffice,
    }
}

#[test]
fn duplicate_creation_is_persistent_and_changed_payload_conflicts() {
    let path = std::env::temp_dir().join(format!("yukii-{}.sqlite", uuid::Uuid::new_v4()));
    let store = TaskStore::open(&path).unwrap();
    let first = store
        .create_once(input("write"), "request-1", true)
        .unwrap();
    drop(store);
    let store = TaskStore::open(&path).unwrap();
    let replay = store
        .create_once(input("write"), "request-1", true)
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(first.task.id, replay.task.id);
    assert_eq!(store.events(&first.task.id).unwrap().len(), 2);
    assert!(matches!(
        store.create_once(input("changed"), "request-1", true),
        Err(StoreError::IdempotencyConflict)
    ));
    assert_eq!(store.list().unwrap().len(), 1);
    drop(store);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn queue_capacity_rolls_back_creation_and_releases_on_cancel() {
    let store = TaskStore::open(":memory:").unwrap();
    for index in 0..100 {
        store
            .create_once(input("work"), &format!("r-{index}"), true)
            .unwrap();
    }
    assert!(matches!(
        store.create_once(input("new"), "overflow", true),
        Err(StoreError::QueueFull)
    ));
    let active = store.claim_next().unwrap().unwrap();
    store.cancel(&active.id).unwrap();
    let accepted = store.create_once(input("new"), "overflow", true).unwrap();
    assert!(!accepted.replayed, "拒绝时不得留下任务或占用幂等键");
}

#[test]
fn separate_connections_cannot_claim_the_same_task() {
    let path = std::env::temp_dir().join(format!("yukii-{}.sqlite", uuid::Uuid::new_v4()));
    let first = TaskStore::open(&path).unwrap();
    first.create_once(input("work"), "one", true).unwrap();
    let second = TaskStore::open(&path).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let handles = [first, second]
        .into_iter()
        .map(|store| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.claim_next().unwrap()
            })
        })
        .collect::<Vec<_>>();
    let claimed = handles
        .into_iter()
        .filter_map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(claimed.len(), 1);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn recovery_preserves_pending_work_but_never_replays_claimed_work() {
    let store = TaskStore::open(":memory:").unwrap();
    let first = store.create_once(input("first"), "1", true).unwrap().task;
    let second = store.create_once(input("second"), "2", true).unwrap().task;
    assert_eq!(store.claim_next().unwrap().unwrap().id, first.id);
    assert_eq!(store.recover_interrupted().unwrap(), 1);
    assert_eq!(store.get(&first.id).unwrap().status, TaskStatus::Failed);
    assert_eq!(store.claim_next().unwrap().unwrap().id, second.id);
    assert!(store.claim_next().unwrap().is_none());
}

#[test]
fn tool_receipts_reuse_results_and_reject_uncertain_or_changed_calls() {
    let store = TaskStore::open(":memory:").unwrap();
    let task = store.create_once(input("write"), "one", true).unwrap().task;
    store.claim_next().unwrap();
    let call = ToolCall {
        id: "document-1".into(),
        name: "create_markdown".into(),
        arguments: serde_json::json!({"title":"t","content":"c"}),
    };
    assert!(
        store
            .begin_tool(&task.id, &call, None, None, 0)
            .unwrap()
            .is_none()
    );
    assert!(
        store.begin_tool(&task.id, &call, None, None, 0).is_err(),
        "未知结果的调用不得重放"
    );
    let result = ToolResult {
        call_id: call.id.clone(),
        content: "saved".into(),
        is_error: false,
        images: Vec::new(),
    };
    store.finish_tool(&task.id, &call, &result).unwrap();
    assert_eq!(
        store
            .begin_tool(&task.id, &call, None, None, 0)
            .unwrap()
            .unwrap()
            .content,
        "saved"
    );
    let changed = ToolCall {
        arguments: serde_json::json!({"title":"changed"}),
        ..call
    };
    assert!(matches!(
        store.begin_tool(&task.id, &changed, None, None, 0),
        Err(StoreError::IdempotencyConflict)
    ));
    store.cancel(&task.id).unwrap();
    assert!(store.finish_tool(&task.id, &changed, &result).is_err());
}

#[test]
fn unversioned_database_migrates_and_newer_schema_is_rejected() {
    let path = std::env::temp_dir().join(format!("yukii-{}.sqlite", uuid::Uuid::new_v4()));
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(include_str!("../migrations/0001_tasks.sql"))
        .unwrap();
    connection.execute("INSERT INTO tasks(id,payload,created_at) VALUES('old',?1,1)",[r#"{"id":"old","prompt":"legacy","target":"cloud_office","status":"queued","created_at":1}"#]).unwrap();
    drop(connection);
    let store = TaskStore::open(&path).unwrap();
    assert_eq!(store.get("old").unwrap().input_tokens, 0);
    store.enqueue("old").unwrap();
    assert_eq!(store.claim_next().unwrap().unwrap().id, "old");
    drop(store);
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute("INSERT INTO schema_migrations(version) VALUES(999)", [])
        .unwrap();
    drop(connection);
    assert!(matches!(
        TaskStore::open(&path),
        Err(StoreError::SchemaTooNew)
    ));
    std::fs::remove_file(path).unwrap();
}
