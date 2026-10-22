use yukii_agent_infra::{StoreError, TaskStore};
use yukii_contracts::{CreateTask, ExecutionTarget};

fn message(conversation_id: Option<String>, prompt: &str) -> CreateTask {
    CreateTask {
        conversation_id,
        prompt: prompt.into(),
        target: ExecutionTarget::CloudOffice,
    }
}

#[test]
fn followup_keeps_identity_history_and_idempotency_after_restart() {
    let path = std::env::temp_dir().join(format!(
        "yukii-conversation-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = TaskStore::open(&path).unwrap();
    let first = store
        .create_once(message(None, "first"), "first", false)
        .unwrap()
        .task;
    store.cancel(&first.id).unwrap();
    let second = store
        .create_once(
            message(Some(first.conversation_id.clone()), "second"),
            "second",
            false,
        )
        .unwrap()
        .task;
    assert_ne!(first.id, second.id);
    assert_eq!(first.conversation_id, second.conversation_id);
    assert_eq!(second.conversation_title, "first");
    drop(store);
    let store = TaskStore::open(&path).unwrap();
    let replay = store
        .create_once(
            message(Some(first.conversation_id.clone()), "second"),
            "second",
            false,
        )
        .unwrap();
    assert!(replay.replayed); // 先去重再检查忙碌状态，丢失响应仍可恢复。
    assert_eq!(store.conversations().unwrap().len(), 1);
    assert_eq!(store.conversation(&first.conversation_id).unwrap().len(), 2);
    assert!(matches!(
        store.create_once(message(None, "second"), "second", false),
        Err(StoreError::IdempotencyConflict)
    ));
    assert!(matches!(
        store.create_once(message(Some("absent".into()), "x"), "absent", false),
        Err(StoreError::NotFound)
    ));
    drop(store);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn concurrent_followups_only_accept_one_and_do_not_create_orphan_events() {
    let store = std::sync::Arc::new(TaskStore::open(":memory:").unwrap());
    let first = store
        .create_once(message(None, "first"), "first", false)
        .unwrap()
        .task;
    store.cancel(&first.id).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles = (0..2)
        .map(|index| {
            let store = store.clone();
            let barrier = barrier.clone();
            let id = first.conversation_id.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.create_once(message(Some(id), "next"), &format!("next-{index}"), true)
            })
        })
        .collect::<Vec<_>>();
    let outcomes = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(outcomes.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|r| matches!(r, Err(StoreError::ConversationBusy)))
            .count(),
        1
    );
    assert_eq!(store.list().unwrap().len(), 2);
}
