//! 启动真实 Executor 子进程与专属管道；不读取 .env，不操作用户文件。
use std::{
    fs,
    io::{BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use yukii_agent_infra::TaskStore;
use yukii_agent_ports::{model::ToolCall, permissions::PermissionRepository};
use yukii_contracts::{CreateTask, ExecutionTarget, desktop::*, permissions::*};
use yukii_desktop_executor::content_digest;
use yukii_desktop_executor_service::request_digest;
use yukii_desktop_transport::{MAX_FRAME_BYTES, read_frame, write_frame};

struct Fixture {
    directory: PathBuf,
    database: PathBuf,
    store: TaskStore,
    task: String,
}
impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!("yukii-ipc-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        fs::create_dir(directory.join("work")).unwrap();
        let database = directory.join("test.sqlite");
        let store = TaskStore::open(&database).unwrap();
        let task = store
            .create_once(
                CreateTask {
                    conversation_id: None,
                    prompt: "executor IPC test".into(),
                    target: ExecutionTarget::CloudOffice,
                },
                "request",
                true,
            )
            .unwrap()
            .task
            .id;
        store.claim_next().unwrap();
        Self {
            directory,
            database,
            store,
            task,
        }
    }

    fn request(&self, workspace: String, content: &str) -> FileRequest {
        let call_id = uuid::Uuid::new_v4().to_string();
        self.store
            .begin_tool(
                &self.task,
                &ToolCall {
                    id: call_id.clone(),
                    name: "local_create".into(),
                    arguments: serde_json::json!({"path":"report.md","content":content}),
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
                workspace_id: workspace,
                relative_path: "report.md".into(),
                operation: FileOperation::Create {
                    content_sha256: content_digest(content),
                },
            },
        };
        let expires = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 600;
        let record = self
            .store
            .request_permission(binding.clone(), expires)
            .unwrap();
        self.store
            .decide_permission(&record.id, &binding, PermissionDecision::Approve)
            .unwrap();
        FileRequest {
            permission_id: record.id,
            binding,
            content: Some(content.into()),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

struct Peer {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
}
impl Peer {
    fn start(database: &Path, session: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_yukii-desktop-executor-service"))
            .env_clear()
            .arg("--database")
            .arg(database)
            .arg("--session")
            .arg(session)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            input: Some(input),
            output,
        }
    }
    fn send(&mut self, command: DesktopCommand) {
        write_frame(
            self.input.as_mut().unwrap(),
            &DesktopRequest {
                version: DESKTOP_PROTOCOL_VERSION,
                request_id: "request-1".into(),
                command,
            },
        )
        .unwrap();
    }
    fn call(&mut self, command: DesktopCommand) -> DesktopReply {
        self.send(command);
        let response: DesktopResponse = read_frame(&mut self.output).unwrap().unwrap();
        assert_eq!(response.request_id, "request-1");
        assert_eq!(response.version, DESKTOP_PROTOCOL_VERSION);
        response.reply
    }
    fn register(&mut self, fixture: &Fixture) -> String {
        match self.call(DesktopCommand::RegisterWorkspace {
            path: fixture.directory.join("work").to_str().unwrap().into(),
        }) {
            DesktopReply::Workspace { workspace_id } => workspace_id,
            other => panic!("{other:?}"),
        }
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn actual_process_persists_before_delivery_and_replays_after_disconnect_without_workspace() {
    let fixture = Fixture::new();
    let mut peer = Peer::start(&fixture.database, "session");
    assert!(matches!(
        peer.call(DesktopCommand::Hello),
        DesktopReply::Hello {
            file_execution: true,
            computer_use: false
        }
    ));
    let workspace = peer.register(&fixture);
    let request = fixture.request(workspace, "# 真实子进程写入\n");
    peer.send(DesktopCommand::ExecuteFile {
        request: request.clone(),
    });
    let connection = rusqlite::Connection::open(&fixture.database).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let complete: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM desktop_receipts WHERE result IS NOT NULL)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        if complete {
            break;
        }
        assert!(Instant::now() < deadline, "子进程结果未在期限内持久化");
        std::thread::sleep(Duration::from_millis(10));
    }
    // 丢弃未消费的响应，模拟父子进程断连；重启后没有注册旧 workspace。
    drop(peer);
    let mut peer = Peer::start(&fixture.database, "session");
    assert!(matches!(
        peer.call(DesktopCommand::ExecuteFile {
            request: request.clone()
        }),
        DesktopReply::Execution {
            result: DesktopExecutionResult::Completed { .. },
            replayed: true
        }
    ));
    assert_eq!(
        fs::read_to_string(fixture.directory.join("work/report.md")).unwrap(),
        "# 真实子进程写入\n"
    );
    let mut changed = request.clone();
    changed.content = Some("different".into());
    assert!(matches!(
        peer.call(DesktopCommand::ExecuteFile { request: changed }),
        DesktopReply::Error { .. }
    ));
    let mut other = Peer::start(&fixture.database, "other-session");
    assert!(matches!(
        other.call(DesktopCommand::ExecuteFile {
            request: request.clone()
        }),
        DesktopReply::Error { .. }
    ));
    assert!(matches!(
        other.call(DesktopCommand::Acknowledge {
            permission_id: request.permission_id.clone()
        }),
        DesktopReply::Error { .. }
    ));
    assert!(matches!(
        peer.call(DesktopCommand::Acknowledge {
            permission_id: request.permission_id.clone()
        }),
        DesktopReply::Done
    ));
    let count = fixture.store.events(&fixture.task).unwrap().len();
    assert!(matches!(
        peer.call(DesktopCommand::Acknowledge {
            permission_id: request.permission_id
        }),
        DesktopReply::Done
    ));
    assert_eq!(fixture.store.events(&fixture.task).unwrap().len(), count);
}

#[test]
fn committed_intent_without_result_never_reexecutes_after_restart() {
    let fixture = Fixture::new();
    let mut peer = Peer::start(&fixture.database, "session");
    let workspace = peer.register(&fixture);
    let request = fixture.request(workspace, "must not write");
    fixture
        .store
        .begin_desktop_execution(
            &request.permission_id,
            &request.binding,
            &request_digest(&request).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        peer.call(DesktopCommand::ExecuteFile {
            request: request.clone()
        }),
        DesktopReply::Execution {
            result: DesktopExecutionResult::OutcomeUnknown { .. },
            replayed: true
        }
    ));
    assert!(!fixture.directory.join("work/report.md").exists());
    assert!(matches!(
        peer.call(DesktopCommand::Acknowledge {
            permission_id: request.permission_id
        }),
        DesktopReply::Error { .. }
    ));
}

#[test]
fn failure_to_commit_result_after_real_write_is_reported_as_unknown_on_reconnect() {
    let fixture = Fixture::new();
    let mut peer = Peer::start(&fixture.database, "session");
    let workspace = peer.register(&fixture);
    let request = fixture.request(workspace, "write happened");
    let connection = rusqlite::Connection::open(&fixture.database).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_result BEFORE UPDATE OF result ON desktop_receipts BEGIN SELECT RAISE(ABORT,'injected result failure'); END;").unwrap();
    peer.send(DesktopCommand::ExecuteFile {
        request: request.clone(),
    });
    assert!(
        read_frame::<DesktopResponse>(&mut peer.output)
            .unwrap()
            .is_none()
    );
    assert!(!peer.child.wait().unwrap().success());
    assert_eq!(
        fs::read_to_string(fixture.directory.join("work/report.md")).unwrap(),
        "write happened"
    );
    let result: Option<String> = connection
        .query_row("SELECT result FROM desktop_receipts", [], |row| row.get(0))
        .unwrap();
    assert!(result.is_none());
    let mut restarted = Peer::start(&fixture.database, "session");
    assert!(matches!(
        restarted.call(DesktopCommand::ExecuteFile { request }),
        DesktopReply::Execution {
            result: DesktopExecutionResult::OutcomeUnknown { .. },
            replayed: true
        }
    ));
}

#[test]
fn revoked_permission_never_writes_and_failure_receipt_is_replayable() {
    let fixture = Fixture::new();
    let mut peer = Peer::start(&fixture.database, "session");
    let workspace = peer.register(&fixture);
    let request = fixture.request(workspace, "denied");
    fixture
        .store
        .revoke_permission(&request.permission_id, &request.binding)
        .unwrap();
    assert!(matches!(
        peer.call(DesktopCommand::ExecuteFile {
            request: request.clone()
        }),
        DesktopReply::Execution {
            result: DesktopExecutionResult::Rejected { .. },
            replayed: false
        }
    ));
    assert!(matches!(
        peer.call(DesktopCommand::ExecuteFile { request }),
        DesktopReply::Execution {
            result: DesktopExecutionResult::Rejected { .. },
            replayed: true
        }
    ));
    assert!(!fixture.directory.join("work/report.md").exists());
}

#[test]
fn incompatible_version_unknown_command_and_oversized_frame_are_rejected() {
    let fixture = Fixture::new();
    let mut peer = Peer::start(&fixture.database, "session");
    write_frame(
        peer.input.as_mut().unwrap(),
        &DesktopRequest {
            version: 999,
            request_id: "bad-version".into(),
            command: DesktopCommand::Hello,
        },
    )
    .unwrap();
    let response: DesktopResponse = read_frame(&mut peer.output).unwrap().unwrap();
    assert!(matches!(response.reply, DesktopReply::Error { .. }));
    write_frame(peer.input.as_mut().unwrap(),&serde_json::json!({"version":1,"request_id":"attempt","command":{"type":"approve","permission_id":"fake"}})).unwrap();
    assert!(
        read_frame::<DesktopResponse>(&mut peer.output)
            .unwrap()
            .is_none()
    );
    assert!(!peer.child.wait().unwrap().success());
    let mut peer = Peer::start(&fixture.database, "session");
    peer.input
        .as_mut()
        .unwrap()
        .write_all(&((MAX_FRAME_BYTES as u32) + 1).to_be_bytes())
        .unwrap();
    assert!(
        read_frame::<DesktopResponse>(&mut peer.output)
            .unwrap()
            .is_none()
    );
    assert!(!peer.child.wait().unwrap().success());
}

#[test]
fn clean_eof_terminates_executor_without_touching_task_lifecycle() {
    let fixture = Fixture::new();
    let mut peer = Peer::start(&fixture.database, "session");
    assert!(matches!(
        peer.call(DesktopCommand::Hello),
        DesktopReply::Hello { .. }
    ));
    drop(peer.input.take());
    assert!(
        read_frame::<DesktopResponse>(&mut peer.output)
            .unwrap()
            .is_none()
    );
    assert!(peer.child.wait().unwrap().success());
    assert_eq!(
        fixture.store.get(&fixture.task).unwrap().status,
        yukii_contracts::TaskStatus::Running
    );
}
