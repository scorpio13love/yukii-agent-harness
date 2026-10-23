use std::sync::Arc;
use yukii_agent_infra::TaskStore;
use yukii_contracts::{CreateTask, ExecutionTarget};

#[test]
fn file_change_records_baseline_diff_and_counts() {
    let store = Arc::new(TaskStore::open(":memory:").unwrap());
    let task = store
        .create(CreateTask {
            conversation_id: None,
            prompt: "edit a file".into(),
            target: ExecutionTarget::CloudOffice,
        })
        .unwrap();
    store
        .record_file_change(
            &task.id,
            &task.conversation_id,
            "src/main.rs",
            "fn main() {\n    old();\n}\n",
            "fn main() {\n    new();\n}\n",
            "modify",
        )
        .unwrap();
    let changes = store.task_file_changes(&task.id).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["path"], "src/main.rs");
    assert_eq!(changes[0]["added"], 1);
    assert_eq!(changes[0]["removed"], 1);
    let diff = store
        .task_file_diff(&task.id, "src/main.rs")
        .unwrap()
        .unwrap();
    assert_eq!(diff["lines"].as_array().unwrap().len(), 4);
    assert!(
        diff["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line["kind"] == "added" && line["text"] == "    new();")
    );
}
