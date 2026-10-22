//! 宿主完整编排：窗口选择（显式）、会话同意、真实 Executor + 假原生桥、假视觉模型。
//! 不访问真实屏幕、不读取 .env、不产生模型费用；执行服务未构建时明确跳过。
use async_trait::async_trait;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use yukii_agent_core::runner::AgentLimits;
use yukii_agent_infra::TaskStore;
use yukii_agent_ports::model::{
    LanguageModel, Message, ModelError, ModelReply, ToolCall, ToolDefinition, Usage,
};
use yukii_contracts::TaskStatus;
use yukii_desktop_host::{
    approval::AllowAllActions,
    host::{Options, run_session},
    ipc::IpcControl,
};

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

fn executor_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/yukii-desktop-executor-service")
}

struct FakeModel {
    turn: AtomicUsize,
}

#[async_trait]
impl LanguageModel for FakeModel {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<ModelReply, ModelError> {
        assert!(system.contains("observe_screen"));
        assert!(tools.iter().any(|tool| tool.name == "click"));
        match self.turn.fetch_add(1, Ordering::SeqCst) {
            0 => Ok(ModelReply {
                text: String::new(),
                calls: vec![ToolCall {
                    id: "call-observe".into(),
                    name: "observe_screen".into(),
                    arguments: serde_json::json!({}),
                }],
                usage: Usage::default(),
            }),
            1 => {
                let observation_id = messages
                    .iter()
                    .rev()
                    .find_map(|message| match message {
                        Message::ToolResults(results) => results.iter().find_map(|result| {
                            let value: serde_json::Value =
                                serde_json::from_str(&result.content).ok()?;
                            value["observation_id"].as_str().map(str::to_owned)
                        }),
                        _ => None,
                    })
                    .expect("模型必须收到 observation_id");
                assert!(messages.iter().any(|message| match message {
                    Message::ToolResults(results) =>
                        results.iter().any(|result| result.images.len() == 1),
                    _ => false,
                }));
                Ok(ModelReply {
                    text: String::new(),
                    calls: vec![ToolCall {
                        id: "call-click".into(),
                        name: "click".into(),
                        arguments: serde_json::json!({
                            "observation_id": observation_id,
                            "x": 10.0,
                            "y": 20.0
                        }),
                    }],
                    usage: Usage::default(),
                })
            }
            _ => Ok(ModelReply {
                text: "已在测试窗口完成一次点击，并等待重新观察。".into(),
                calls: Vec::new(),
                usage: Usage::default(),
            }),
        }
    }
}

#[tokio::test]
async fn host_orchestrates_consent_executor_tools_and_model_without_desktop_access() {
    let executor = executor_path();
    if !executor.is_file() {
        eprintln!(
            "跳过：未找到 {}；先运行 ./tooling/cargo.sh build -p yukii-desktop-executor-service",
            executor.display()
        );
        return;
    }
    let directory = std::env::temp_dir().join(format!("yukii-host-session-{}", std::process::id()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir(&directory).unwrap();
    let database = directory.join("host.sqlite");
    let store = Arc::new(TaskStore::open(&database).unwrap());
    let bridge = directory.join("bridge.sh");
    fs::write(
        &bridge,
        format!(
            r#"#!/bin/sh
input=$(cat)
case "$input" in
  *'"operation":"permissions"'*) printf '{{"screen_capture":true,"accessibility":true}}' ;;
  *'"operation":"windows"'*) printf '{{"windows":[{{"window_id":5,"pid":7,"application":"Yukii Test","title":"测试窗口"}}]}}' ;;
  *'"operation":"observe"'*) printf '{{"png_base64":"{PNG}","width":800,"height":600,"frame":[10,20,800,600]}}' ;;
  *) printf '{{"dispatched":true,"verification_required":true}}' ;;
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
    let ipc = Arc::new(
        IpcControl::spawn(
            &executor,
            &database,
            "host-session",
            &bridge,
            Arc::new(AllowAllActions),
        )
        .unwrap(),
    );
    let options = Options {
        prompt: "在测试窗口点击坐标 10,20".into(),
        data_dir: directory.clone(),
        executor: executor.clone(),
        bridge: bridge.clone(),
        window_id: Some(5),
        pid: Some(7),
        application: None,
        window_title: None,
        control_minutes: 10,
        browser_url: None,
        browser_endpoint: None,
        browser_profile: None,
        browser_path: None,
        mcp_config: None,
        close_browser: false,
        preview_panel: None,
        no_preview: true,
        panel_select: false,
        window_picker: None,
        no_confirm_actions: true,
        yes: true,
        json_events: false,
        select_app: false,
    };
    run_session(
        &options,
        &store,
        Arc::new(FakeModel {
            turn: AtomicUsize::new(0),
        }),
        AgentLimits {
            timeout: Duration::from_secs(10),
            ..Default::default()
        },
        ipc.clone(),
    )
    .await
    .unwrap();
    ipc.shutdown().await;

    let tasks = store.list().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(
        tasks[0].status,
        TaskStatus::Completed,
        "{:?}",
        tasks[0].error
    );
    assert!(tasks[0].result.as_deref().unwrap().contains("测试窗口"));
    let connection = rusqlite::Connection::open(&database).unwrap();
    let session_state: String = connection
        .query_row("SELECT state FROM computer_sessions", [], |row| row.get(0))
        .unwrap();
    let action_state: String = connection
        .query_row("SELECT state FROM computer_actions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(session_state, "ended");
    assert_eq!(action_state, "dispatched");
    let _ = fs::remove_dir_all(&directory);
}
