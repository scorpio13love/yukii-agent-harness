//! 宿主 IPC 客户端与真实 Executor 子进程的最小联调。
//! 若只运行本包测试而尚未构建执行服务，则明确跳过，不伪造通过。
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use yukii_agent_infra::TaskStore;
use yukii_contracts::{
    CreateTask, ExecutionTarget,
    desktop::{DesktopWindow, GuiAction},
};
use yukii_desktop_host::{approval::AllowAllActions, ipc::IpcControl};

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn executor_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/yukii-desktop-executor-service")
}

#[tokio::test]
async fn host_client_drives_real_executor_with_mock_bridge() {
    let executor = executor_path();
    if !executor.is_file() {
        eprintln!(
            "跳过：未找到 {}；先运行 ./tooling/cargo.sh build -p yukii-desktop-executor-service",
            executor.display()
        );
        return;
    }
    let directory = std::env::temp_dir().join(format!("yukii-host-ipc-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    let database = directory.join("host.sqlite");
    let store = TaskStore::open(&database).unwrap();
    store
        .create_local_once(
            CreateTask {
                conversation_id: None,
                prompt: "测试宿主 IPC".into(),
                target: ExecutionTarget::LocalComputer,
            },
            "host-ipc",
            true,
        )
        .unwrap();
    let task = store.claim_next().unwrap().unwrap();
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
    let ipc = IpcControl::spawn(
        &executor,
        &database,
        "host-session",
        &bridge,
        Arc::new(AllowAllActions),
    )
    .unwrap();
    ipc.hello().await.unwrap();
    let permissions = ipc.permissions().await.unwrap();
    assert!(permissions.screen_capture && permissions.accessibility);
    let windows = ipc.windows().await.unwrap();
    assert_eq!(
        windows,
        vec![DesktopWindow {
            window_id: 5,
            pid: 7,
            application: "Yukii Test".into(),
            title: "测试窗口".into(),
        }]
    );
    ipc.begin_control(&task.id, &windows[0], true, now() + 600)
        .await
        .unwrap();
    let observation = ipc.observe(&task.id).await.unwrap();
    assert_eq!(observation.width, 800);
    let dispatch = ipc
        .act_dispatch(
            &task.id,
            "call-1",
            &observation.observation_id,
            &GuiAction::Click { x: 10.0, y: 20.0 },
        )
        .await
        .unwrap();
    assert!(dispatch.verification_required);
    ipc.end_control().await.unwrap();
    ipc.shutdown().await;
    let _ = fs::remove_dir_all(&directory);
}
