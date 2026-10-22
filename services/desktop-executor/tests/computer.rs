//! 真实 Executor 子进程 + 私有 IPC + 假原生桥；不读取用户桌面、不申请系统权限。
use std::{
    fs,
    io::{BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};
use yukii_agent_infra::TaskStore;
use yukii_contracts::{CreateTask, ExecutionTarget, desktop::*};
use yukii_desktop_transport::{read_frame, write_frame};

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

struct Fixture {
    directory: PathBuf,
    database: PathBuf,
    bridge: PathBuf,
    task: String,
}

impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!("yukii-cu-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let database = directory.join("test.sqlite");
        let store = TaskStore::open(&database).unwrap();
        let task = store
            .create_local_once(
                CreateTask {
                    conversation_id: None,
                    prompt: "在测试窗口中完成操作".into(),
                    target: ExecutionTarget::LocalComputer,
                },
                "request",
                true,
            )
            .unwrap()
            .task
            .id;
        store.claim_next().unwrap();
        let bridge = directory.join("mock-bridge.sh");
        fs::write(
            &bridge,
            format!(
                r#"#!/bin/sh
input=$(cat)
case "$input" in
  *'"operation":"permissions"'*) printf '{{"screen_capture":true,"accessibility":true}}' ;;
  *'"operation":"windows"'*) printf '{{"windows":[{{"window_id":5,"pid":7,"application":"Yukii Test","title":"测试窗口"}}]}}' ;;
  *'"operation":"observe"'*) printf '{{"png_base64":"{PNG}","width":800,"height":600,"frame":[10,20,800,600]}}' ;;
  *)
    case "$MOCK_BRIDGE_MODE" in
      reject) printf '{{"error":"target_not_frontmost"}}'; exit 1 ;;
      unknown) exit 3 ;;
      *) printf '{{"dispatched":true,"verification_required":true}}' ;;
    esac ;;
esac
"#
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bridge, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self {
            directory,
            database,
            bridge,
            task,
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
    fn start(fixture: &Fixture, mode: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_yukii-desktop-executor-service"))
            .arg("--database")
            .arg(&fixture.database)
            .arg("--session")
            .arg("session")
            .arg("--bridge")
            .arg(&fixture.bridge)
            .env("MOCK_BRIDGE_MODE", mode)
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

    fn call(&mut self, command: DesktopCommand) -> DesktopReply {
        write_frame(
            self.input.as_mut().unwrap(),
            &DesktopRequest {
                version: DESKTOP_PROTOCOL_VERSION,
                request_id: "request-1".into(),
                command,
            },
        )
        .unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
        let response: DesktopResponse = read_frame(&mut self.output).unwrap().unwrap();
        assert_eq!(response.request_id, "request-1");
        response.reply
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn window() -> DesktopWindow {
    DesktopWindow {
        window_id: 5,
        pid: 7,
        application: "Yukii Test".into(),
        title: "测试窗口".into(),
    }
}

fn observation(reply: DesktopReply) -> DesktopObservation {
    match reply {
        DesktopReply::Observation { observation } => observation,
        other => panic!("期望观察结果，得到 {other:?}"),
    }
}

#[test]
fn real_executor_runs_observe_act_control_lifecycle_and_audits_without_retry() {
    let fixture = Fixture::new();
    let mut peer = Peer::start(&fixture, "ok");
    assert!(matches!(
        peer.call(DesktopCommand::Hello),
        DesktopReply::Hello {
            computer_use: true,
            ..
        }
    ));
    assert!(matches!(
        peer.call(DesktopCommand::ComputerPermissions),
        DesktopReply::Permissions {
            permissions: ComputerPermissions {
                screen_capture: true,
                accessibility: true
            }
        }
    ));
    let windows = match peer.call(DesktopCommand::ListWindows) {
        DesktopReply::Windows { windows } => windows,
        other => panic!("期望窗口列表，得到 {other:?}"),
    };
    assert_eq!(windows, vec![window()]);
    let control = match peer.call(DesktopCommand::BeginControl {
        task_id: fixture.task.clone(),
        window: window(),
        screenshot_sharing: true,
        expires_at: now() + 600,
    }) {
        DesktopReply::Control { control } => control,
        other => panic!("期望控制会话，得到 {other:?}"),
    };
    assert_eq!(control.state, ControlState::Active);
    assert!(control.screenshot_sharing);

    let first = observation(peer.call(DesktopCommand::Observe {
        task_id: fixture.task.clone(),
    }));
    assert_eq!(first.width, 800);
    assert!(first.png_base64.starts_with("iVBOR"));
    assert_eq!(first.frame, [10.0, 20.0, 800.0, 600.0]);
    let dispatch = match peer.call(DesktopCommand::Act {
        task_id: fixture.task.clone(),
        call_id: "call-1".into(),
        observation_id: first.observation_id.clone(),
        action: GuiAction::Click { x: 400.0, y: 300.0 },
    }) {
        DesktopReply::Action { dispatch } => dispatch,
        other => panic!("期望动作派发，得到 {other:?}"),
    };
    assert!(dispatch.verification_required);
    // 同一观察只能使用一次，不能因为重试逻辑重复点击。
    assert!(matches!(
        peer.call(DesktopCommand::Act {
            task_id: fixture.task.clone(),
            call_id: "call-2".into(),
            observation_id: first.observation_id.clone(),
            action: GuiAction::Click { x: 400.0, y: 300.0 },
        }),
        DesktopReply::Error { code, .. } if code == "computer_observation_unavailable"
    ));

    assert!(matches!(
        peer.call(DesktopCommand::PauseControl {
            task_id: fixture.task.clone(),
        }),
        DesktopReply::Control {
            control: ControlSession {
                state: ControlState::Paused,
                ..
            }
        }
    ));
    assert!(matches!(
        peer.call(DesktopCommand::Observe {
            task_id: fixture.task.clone(),
        }),
        DesktopReply::Error { code, .. } if code == "computer_control_unavailable"
    ));
    assert!(matches!(
        peer.call(DesktopCommand::ResumeControl {
            task_id: fixture.task.clone(),
        }),
        DesktopReply::Control {
            control: ControlSession {
                state: ControlState::Active,
                ..
            }
        }
    ));
    // 恢复后旧观察失效。
    assert!(matches!(
        peer.call(DesktopCommand::Act {
            task_id: fixture.task.clone(),
            call_id: "call-3".into(),
            observation_id: first.observation_id,
            action: GuiAction::Click { x: 1.0, y: 1.0 },
        }),
        DesktopReply::Error { code, .. } if code == "computer_observation_unavailable"
    ));
    let second = observation(peer.call(DesktopCommand::Observe {
        task_id: fixture.task.clone(),
    }));
    assert!(matches!(
        peer.call(DesktopCommand::Act {
            task_id: fixture.task.clone(),
            call_id: "call-4".into(),
            observation_id: second.observation_id,
            action: GuiAction::TypeText {
                text: "测试".into()
            },
        }),
        DesktopReply::Action { .. }
    ));
    assert!(matches!(
        peer.call(DesktopCommand::EndControl {
            task_id: fixture.task.clone(),
        }),
        DesktopReply::Control {
            control: ControlSession {
                state: ControlState::Ended,
                ..
            }
        }
    ));
    assert!(matches!(
        peer.call(DesktopCommand::Observe {
            task_id: fixture.task.clone(),
        }),
        DesktopReply::Error { code, .. } if code == "computer_control_unavailable"
    ));

    let connection = rusqlite::Connection::open(&fixture.database).unwrap();
    let sessions: i64 = connection
        .query_row("SELECT COUNT(*) FROM computer_sessions", [], |row| {
            row.get(0)
        })
        .unwrap();
    let observations: i64 = connection
        .query_row("SELECT COUNT(*) FROM computer_observations", [], |row| {
            row.get(0)
        })
        .unwrap();
    let actions: i64 = connection
        .query_row("SELECT COUNT(*) FROM computer_actions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!((sessions, observations, actions), (1, 2, 2));
    let states = connection
        .prepare("SELECT state FROM computer_actions ORDER BY created_at,rowid")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(states, vec!["dispatched", "dispatched"]);
}

#[test]
fn bridge_rejection_is_recorded_as_rejected_and_not_replayed() {
    let fixture = Fixture::new();
    let mut peer = Peer::start(&fixture, "reject");
    peer.call(DesktopCommand::BeginControl {
        task_id: fixture.task.clone(),
        window: window(),
        screenshot_sharing: true,
        expires_at: now() + 600,
    });
    let observation = observation(peer.call(DesktopCommand::Observe {
        task_id: fixture.task.clone(),
    }));
    assert!(matches!(
        peer.call(DesktopCommand::Act {
            task_id: fixture.task.clone(),
            call_id: "call-reject".into(),
            observation_id: observation.observation_id,
            action: GuiAction::Click { x: 5.0, y: 5.0 },
        }),
        DesktopReply::Error { code, .. } if code == "computer_rejected"
    ));
    let connection = rusqlite::Connection::open(&fixture.database).unwrap();
    let state: String = connection
        .query_row("SELECT state FROM computer_actions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(state, "rejected");
}

#[test]
fn ambiguous_bridge_failure_is_outcome_unknown_and_not_reexecuted() {
    let fixture = Fixture::new();
    let mut peer = Peer::start(&fixture, "unknown");
    peer.call(DesktopCommand::BeginControl {
        task_id: fixture.task.clone(),
        window: window(),
        screenshot_sharing: true,
        expires_at: now() + 600,
    });
    let observation = observation(peer.call(DesktopCommand::Observe {
        task_id: fixture.task.clone(),
    }));
    assert!(matches!(
        peer.call(DesktopCommand::Act {
            task_id: fixture.task.clone(),
            call_id: "call-unknown".into(),
            observation_id: observation.observation_id,
            action: GuiAction::PressKey { key: "enter".into() },
        }),
        DesktopReply::Error { code, .. } if code == "computer_outcome_unknown"
    ));
    let connection = rusqlite::Connection::open(&fixture.database).unwrap();
    let state: String = connection
        .query_row("SELECT state FROM computer_actions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(state, "outcome_unknown");
}
