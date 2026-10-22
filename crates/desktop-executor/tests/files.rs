#![cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};
use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use yukii_agent_infra::TaskStore;
use yukii_agent_ports::{model::ToolCall, permissions::PermissionRepository};
use yukii_contracts::{
    CreateTask, ExecutionTarget,
    permissions::{FileOperation, FileScope, PermissionBinding, PermissionDecision},
};
use yukii_desktop_executor::{
    FileExecutionError, FileExecutor, FileOutcome, MAX_FILE_BYTES, content_digest,
};

struct Fixture {
    directory: PathBuf,
    store: Arc<TaskStore>,
    executor: FileExecutor,
    workspace: String,
    task: String,
}

impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!("yukii-files-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        fs::create_dir(directory.join("work")).unwrap();
        let store = Arc::new(TaskStore::open(":memory:").unwrap());
        let task = store
            .create_once(
                CreateTask {
                    conversation_id: None,
                    prompt: "file executor test".into(),
                    target: ExecutionTarget::CloudOffice,
                },
                "test",
                true,
            )
            .unwrap()
            .task
            .id;
        store.claim_next().unwrap();
        let mut executor = FileExecutor::new(store.clone());
        let workspace = executor
            .register_workspace("session", &directory.join("work"))
            .unwrap();
        Self {
            directory,
            store,
            executor,
            workspace,
            task,
        }
    }

    fn approve(&self, path: &str, content: Option<&str>) -> (String, PermissionBinding) {
        let call_id = uuid::Uuid::new_v4().to_string();
        self.store
            .begin_tool(
                &self.task,
                &ToolCall {
                    id: call_id.clone(),
                    name: if content.is_some() {
                        "local_create"
                    } else {
                        "local_read"
                    }
                    .into(),
                    arguments: serde_json::json!({"path":path,"content":content}),
                },
                None,
                None,
                0,
            )
            .unwrap();
        let binding = PermissionBinding {
            session_id: "session".into(),
            task_id: self.task.clone(),
            call_id,
            scope: FileScope {
                workspace_id: self.workspace.clone(),
                relative_path: path.into(),
                operation: content
                    .map(|text| FileOperation::Create {
                        content_sha256: content_digest(text),
                    })
                    .unwrap_or(FileOperation::Read),
            },
        };
        let expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 600;
        let record = self
            .store
            .request_permission(binding.clone(), expires_at)
            .unwrap();
        self.store
            .decide_permission(&record.id, &binding, PermissionDecision::Approve)
            .unwrap();
        (record.id, binding)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn creates_real_utf8_file_reads_it_and_never_replays_or_overwrites() {
    let fixture = Fixture::new();
    let text = "# 汇总\n\n真实文件内容。";
    let (id, binding) = fixture.approve("report.md", Some(text));
    assert_eq!(
        fixture.executor.execute(&id, &binding, Some(text)).unwrap(),
        FileOutcome::Created {
            bytes: text.len(),
            sha256: content_digest(text)
        }
    );
    assert_eq!(
        fs::read_to_string(fixture.directory.join("work/report.md")).unwrap(),
        text
    );
    assert_eq!(
        fs::metadata(fixture.directory.join("work/report.md"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(fixture.executor.execute(&id, &binding, Some(text)).is_err());
    let (id, binding) = fixture.approve("report.md", Some("overwrite"));
    assert!(
        fixture
            .executor
            .execute(&id, &binding, Some("overwrite"))
            .is_err()
    );
    let (id, binding) = fixture.approve("report.md", None);
    assert_eq!(
        fixture.executor.execute(&id, &binding, None).unwrap(),
        FileOutcome::Text(text.into())
    );
}

#[test]
fn rejects_final_and_intermediate_symlinks_without_touching_outside_files() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.directory.join("outside")).unwrap();
    fs::write(fixture.directory.join("outside/secret"), "private").unwrap();
    symlink(
        fixture.directory.join("outside"),
        fixture.directory.join("work/link"),
    )
    .unwrap();
    symlink(
        fixture.directory.join("outside/secret"),
        fixture.directory.join("work/secret"),
    )
    .unwrap();
    for path in ["link/secret", "secret"] {
        let (id, binding) = fixture.approve(path, None);
        assert!(fixture.executor.execute(&id, &binding, None).is_err());
    }
    for path in ["link/new", "secret"] {
        let (id, binding) = fixture.approve(path, Some("changed"));
        assert!(
            fixture
                .executor
                .execute(&id, &binding, Some("changed"))
                .is_err()
        );
    }
    assert_eq!(
        fs::read_to_string(fixture.directory.join("outside/secret")).unwrap(),
        "private"
    );
    assert!(!fixture.directory.join("outside/new").exists());
}

#[test]
fn rejects_nonregular_hardlinked_non_utf8_and_oversized_files() {
    let fixture = Fixture::new();
    fs::write(fixture.directory.join("outside"), "private").unwrap();
    fs::hard_link(
        fixture.directory.join("outside"),
        fixture.directory.join("work/hardlink"),
    )
    .unwrap();
    fs::create_dir(fixture.directory.join("work/directory")).unwrap();
    fs::write(fixture.directory.join("work/binary"), [0xff, 0xfe]).unwrap();
    fs::write(
        fixture.directory.join("work/large"),
        vec![b'a'; MAX_FILE_BYTES + 1],
    )
    .unwrap();
    for path in ["hardlink", "directory", "binary", "large"] {
        let (id, binding) = fixture.approve(path, None);
        assert!(
            fixture.executor.execute(&id, &binding, None).is_err(),
            "{path}"
        );
    }
}

#[test]
fn changed_body_revocation_and_wrong_session_cannot_write() {
    let fixture = Fixture::new();
    let (id, binding) = fixture.approve("new", Some("approved"));
    assert!(matches!(
        fixture.executor.execute(&id, &binding, Some("changed")),
        Err(FileExecutionError::ContentMismatch)
    ));
    let mut wrong_session = binding.clone();
    wrong_session.session_id = "other".into();
    assert!(matches!(
        fixture
            .executor
            .execute(&id, &wrong_session, Some("approved")),
        Err(FileExecutionError::WorkspaceUnavailable)
    ));
    fixture.store.revoke_permission(&id, &binding).unwrap();
    assert!(
        fixture
            .executor
            .execute(&id, &binding, Some("approved"))
            .is_err()
    );
    assert!(!fixture.directory.join("work/new").exists());
}

#[test]
fn directory_handle_is_not_redirected_by_replacing_registered_path() {
    let fixture = Fixture::new();
    let (id, binding) = fixture.approve("report", Some("approved"));
    fs::rename(
        fixture.directory.join("work"),
        fixture.directory.join("original"),
    )
    .unwrap();
    fs::create_dir(fixture.directory.join("work")).unwrap();
    fixture
        .executor
        .execute(&id, &binding, Some("approved"))
        .unwrap();
    assert_eq!(
        fs::read_to_string(fixture.directory.join("original/report")).unwrap(),
        "approved"
    );
    assert!(!fixture.directory.join("work/report").exists());
}

#[test]
fn unregistering_workspace_or_restarting_executor_invalidates_workspace_reference() {
    let mut fixture = Fixture::new();
    let (id, binding) = fixture.approve("new", Some("approved"));
    let fresh = FileExecutor::new(fixture.store.clone());
    assert!(fresh.execute(&id, &binding, Some("approved")).is_err());
    fixture.executor.unregister_workspace(&fixture.workspace);
    assert!(
        fixture
            .executor
            .execute(&id, &binding, Some("approved"))
            .is_err()
    );
    assert!(!fixture.directory.join("work/new").exists());
}
