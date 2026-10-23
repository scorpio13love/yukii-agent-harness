use rusqlite::Connection;
use yukii_agent_infra::{StoreError, TaskStore};
use yukii_agent_ports::{
    model::{ToolCall, ToolResult},
    repository::ToolCompletion,
};
use yukii_contracts::{CreateTask, ExecutionTarget, invocation::ExecutionState};

fn running_task(store: &TaskStore) -> String {
    let task = store
        .create_once(
            CreateTask {
                conversation_id: None,
                prompt: "汇总资料".into(),
                target: ExecutionTarget::CloudOffice,
            },
            "request",
            true,
        )
        .unwrap()
        .task;
    store.claim_next().unwrap().unwrap();
    task.id
}

fn call() -> ToolCall {
    ToolCall {
        id: "call-1".into(),
        name: "create_markdown".into(),
        arguments: serde_json::json!({"title":"汇总","content":"正文"}),
    }
}

fn document() -> ToolCompletion {
    ToolCompletion::Document {
        title: "汇总".into(),
        content: "正文".into(),
    }
}

#[test]
fn document_receipt_and_event_survive_reopen_and_replay_without_duplicate_artifacts() {
    let path = std::env::temp_dir().join(format!("invocations-{}.sqlite", uuid::Uuid::new_v4()));
    let store = TaskStore::open(&path).unwrap();
    let task_id = running_task(&store);
    let call = call();
    store.begin_tool(&task_id, &call, None, None, 0).unwrap();
    let invocation = store.invocation(&task_id, &call.id).unwrap();
    assert_eq!(invocation.state, ExecutionState::Running);
    let events_before = store.events(&task_id).unwrap().len();
    let result = store.complete_tool(&task_id, &call, document()).unwrap();
    assert_eq!(store.events(&task_id).unwrap().len(), events_before + 1);
    drop(store);

    let store = TaskStore::open(&path).unwrap();
    let saved = store.invocation(&task_id, &call.id).unwrap();
    assert_eq!(saved.execution_id, invocation.execution_id);
    assert_eq!(saved.state, ExecutionState::Succeeded);
    assert_eq!(
        store
            .begin_tool(&task_id, &call, None, None, 0)
            .unwrap()
            .unwrap()
            .content,
        result.content
    );
    assert!(store.complete_tool(&task_id, &call, document()).is_err());
    let artifacts = store.artifacts(&task_id).unwrap();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(store.artifact_content(&artifacts[0].id).unwrap(), "正文");
    let value: serde_json::Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(value["artifact_id"], artifacts[0].id);
    assert_eq!(store.events(&task_id).unwrap().len(), events_before + 1);
    drop(store);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn event_failure_rolls_back_artifact_result_and_execution_state() {
    let path = std::env::temp_dir().join(format!("invocations-{}.sqlite", uuid::Uuid::new_v4()));
    let store = TaskStore::open(&path).unwrap();
    let task_id = running_task(&store);
    let call = call();
    store.begin_tool(&task_id, &call, None, None, 0).unwrap();
    let events_before = store.events(&task_id).unwrap().len();
    let connection = Connection::open(&path).unwrap();
    // 在事务最后一项写入注入故障，确保已执行的 INSERT/UPDATE 一并回滚。
    connection
        .execute_batch(
            "CREATE TRIGGER fail_completion BEFORE INSERT ON events
         BEGIN SELECT RAISE(ABORT, 'injected event failure'); END;",
        )
        .unwrap();
    assert!(store.complete_tool(&task_id, &call, document()).is_err());
    assert!(store.artifacts(&task_id).unwrap().is_empty());
    assert_eq!(store.events(&task_id).unwrap().len(), events_before);
    assert_eq!(
        store.invocation(&task_id, &call.id).unwrap().state,
        ExecutionState::Running
    );
    let result: Option<String> = connection
        .query_row("SELECT result FROM tool_calls", [], |row| row.get(0))
        .unwrap();
    assert!(result.is_none());
    assert!(store.begin_tool(&task_id, &call, None, None, 0).is_err());
    drop(connection);
    drop(store);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn altered_or_unregistered_completion_cannot_create_artifacts() {
    let store = TaskStore::open(":memory:").unwrap();
    let task_id = running_task(&store);
    let call = call();
    assert!(store.complete_tool(&task_id, &call, document()).is_err());
    store.begin_tool(&task_id, &call, None, None, 0).unwrap();
    let changed = ToolCall {
        arguments: serde_json::json!({"content":"替换内容"}),
        ..call.clone()
    };
    assert!(matches!(
        store.complete_tool(&task_id, &changed, document()),
        Err(StoreError::IdempotencyConflict)
    ));
    assert!(store.artifacts(&task_id).unwrap().is_empty());
    let mismatched = ToolResult {
        call_id: "another-call".into(),
        content: "ok".into(),
        is_error: false,
        images: Vec::new(),
    };
    assert!(matches!(
        store.finish_tool(&task_id, &call, &mismatched),
        Err(StoreError::IdempotencyConflict)
    ));
}

#[test]
fn cancellation_closes_unknown_execution_without_claiming_side_effects_were_undone() {
    let store = TaskStore::open(":memory:").unwrap();
    let task_id = running_task(&store);
    let call = call();
    store.begin_tool(&task_id, &call, None, None, 0).unwrap();
    store.cancel(&task_id).unwrap();
    assert_eq!(
        store.invocation(&task_id, &call.id).unwrap().state,
        ExecutionState::OutcomeUnknown
    );
    assert!(store.complete_tool(&task_id, &call, document()).is_err());
    assert!(store.artifacts(&task_id).unwrap().is_empty());
}

#[test]
fn recovery_marks_only_unfinished_calls_unknown_and_preserves_error_receipts() {
    let store = TaskStore::open(":memory:").unwrap();
    let task_id = running_task(&store);
    let call = call();
    store.begin_tool(&task_id, &call, None, None, 0).unwrap();
    let failed = ToolCall {
        id: "failed-call".into(),
        ..call.clone()
    };
    store.begin_tool(&task_id, &failed, None, None, 0).unwrap();
    store
        .finish_tool(
            &task_id,
            &failed,
            &ToolResult {
                call_id: failed.id.clone(),
                content: "权限拒绝".into(),
                is_error: true,
                images: Vec::new(),
            },
        )
        .unwrap();
    let events_before = store.events(&task_id).unwrap().len();
    assert_eq!(store.recover_interrupted().unwrap(), 1);
    assert_eq!(
        store.invocation(&task_id, &call.id).unwrap().state,
        ExecutionState::OutcomeUnknown
    );
    assert_eq!(
        store.invocation(&task_id, &failed.id).unwrap().state,
        ExecutionState::Failed
    );
    assert_eq!(store.events(&task_id).unwrap().len(), events_before + 1);
    assert_eq!(store.recover_interrupted().unwrap(), 0);
    assert!(store.begin_tool(&task_id, &call, None, None, 0).is_err());
}

#[test]
fn migration_preserves_legacy_results_and_does_not_restart_unfinished_calls() {
    let path = std::env::temp_dir().join(format!("invocations-{}.sqlite", uuid::Uuid::new_v4()));
    let connection = Connection::open(&path).unwrap();
    for sql in [
        include_str!("../migrations/0001_tasks.sql"),
        include_str!("../migrations/0002_execution.sql"),
        include_str!("../migrations/0003_knowledge_plugins.sql"),
        include_str!("../migrations/0004_embeddings.sql"),
    ] {
        connection.execute_batch(sql).unwrap();
    }
    connection
        .execute_batch(
            "CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY);
         INSERT INTO schema_migrations VALUES(1),(2),(3),(4);",
        )
        .unwrap();
    connection.execute(
        "INSERT INTO tasks(id,payload,created_at) VALUES('legacy',?1,1)",
        [r#"{"id":"legacy","prompt":"work","target":"cloud_office","status":"running","created_at":1}"#],
    ).unwrap();
    for (id, result) in [
        ("unknown", None),
        (
            "ok",
            Some(r#"{"call_id":"ok","content":"saved","is_error":false}"#),
        ),
        (
            "error",
            Some(r#"{"call_id":"error","content":"error","is_error":true}"#),
        ),
    ] {
        connection.execute(
            "INSERT INTO tool_calls(task_id,call_id,name,arguments,result) VALUES('legacy',?1,'tool','{}',?2)",
            rusqlite::params![id, result],
        ).unwrap();
    }
    drop(connection);
    let store = TaskStore::open(&path).unwrap();
    for (id, state) in [
        ("unknown", ExecutionState::OutcomeUnknown),
        ("ok", ExecutionState::Succeeded),
        ("error", ExecutionState::Failed),
    ] {
        assert_eq!(store.invocation("legacy", id).unwrap().state, state);
    }
    assert_ne!(
        store.invocation("legacy", "ok").unwrap().execution_id,
        store.invocation("legacy", "error").unwrap().execution_id
    );
    let unknown = ToolCall {
        id: "unknown".into(),
        name: "tool".into(),
        arguments: serde_json::json!({}),
    };
    assert!(store.begin_tool("legacy", &unknown, None, None, 0).is_err());
    let completed = ToolCall {
        id: "ok".into(),
        ..unknown
    };
    assert_eq!(
        store
            .begin_tool("legacy", &completed, None, None, 0)
            .unwrap()
            .unwrap()
            .content,
        "saved"
    );
    drop(store);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn artifact_is_also_written_to_the_session_workspace() {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let path = std::env::temp_dir().join(format!("invocations-{}.sqlite", uuid::Uuid::new_v4()));
    let workspace = std::env::temp_dir().join(format!("invocations-ws-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&workspace).unwrap();
    let store = TaskStore::open(&path).unwrap();
    let task_id = running_task(&store);
    let task = store.get(&task_id).unwrap();
    store
        .set_session_workspace(
            &task.conversation_id,
            workspace.to_str().unwrap(),
            "test",
            "session",
        )
        .unwrap();
    let call = ToolCall {
        id: "artifact-1".into(),
        name: "create_docx".into(),
        arguments: serde_json::json!({}),
    };
    store.begin_tool(&task_id, &call, None, None, 0).unwrap();
    store
        .complete_tool(
            &task_id,
            &call,
            yukii_agent_ports::repository::ToolCompletion::Artifact {
                title: "报告".into(),
                filename: "report.txt".into(),
                media_type: "text/plain".into(),
                base64: STANDARD.encode("hello"),
            },
        )
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(workspace.join("report.txt")).unwrap(),
        "hello"
    );
    drop(store);
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_dir_all(workspace);
}
