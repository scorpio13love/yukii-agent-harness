//! 显式运行：cargo test -p yukii-sandbox-docker --test docker_boundary -- --ignored。
//! 只使用测试标签清理容器；镜像须先由操作者准备，不在测试中自动拉取。
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::process::Command;
use yukii_agent_ports::sandbox::{Sandbox, SandboxRequest};
use yukii_sandbox_docker::{DockerConfig, DockerSandbox};

const IMAGE: &str =
    "python@sha256:2d9aefe2fef018a7eb2c13064c89c71929800fd2e5dccdbf52ea5da5bb8d929a";
fn sandbox(timeout: u64) -> (Arc<DockerSandbox>, String) {
    let owner = format!("test-{}", uuid::Uuid::new_v4());
    let sandbox = DockerSandbox::new(DockerConfig {
        enabled: true,
        runtime: None,
        timeout: Duration::from_secs(timeout),
        owner: owner.clone(),
    })
    .unwrap();
    (Arc::new(sandbox), owner)
}
fn request(code: &str) -> SandboxRequest {
    SandboxRequest {
        image: IMAGE.into(),
        command: vec![
            "/usr/local/bin/python3".into(),
            "-I".into(),
            "-c".into(),
            code.into(),
        ],
        input: json!({"text":"你好 office"}),
    }
}
async fn containers(owner: &str) -> String {
    let output = Command::new("docker")
        .args([
            "ps",
            "-aq",
            "--filter",
            &format!("label=io.yukii.owner={owner}"),
        ])
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test]
#[ignore = "requires Docker and the pinned Python image"]
async fn real_container_has_no_host_access_and_cleans_up_after_success_and_output_overflow() {
    let (sandbox, owner) = sandbox(15);
    let result = sandbox
        .execute(request(
            r#"
import json, os, socket, sys
data = json.load(sys.stdin)
try:
    open('/yukii-denied', 'w').write('no')
    root_readonly = False
except OSError:
    root_readonly = True
try:
    socket.create_connection(('1.1.1.1', 443), timeout=1)
    network_blocked = False
except OSError:
    network_blocked = True
print(json.dumps({'text': data['text'], 'uid': os.getuid(), 'root_readonly': root_readonly,
    'network_blocked': network_blocked, 'has_docker_socket': os.path.exists('/var/run/docker.sock'),
    'has_api_key': any('API_KEY' in key for key in os.environ)}))
"#,
        ))
        .await;
    // 先验证清理再断言结果，失败路径同样检查不能遗留容器。
    assert!(containers(&owner).await.trim().is_empty());
    let result = result.unwrap();
    assert_eq!(result["text"], "你好 office");
    assert_eq!(result["uid"], 65532);
    assert_eq!(result["root_readonly"], true);
    assert_eq!(result["network_blocked"], true);
    assert_eq!(result["has_api_key"], false);
    assert_eq!(result["has_docker_socket"], false);
    let overflow = sandbox.execute(request("print('x' * 100000)")).await;
    assert!(containers(&owner).await.trim().is_empty());
    assert!(overflow.unwrap_err().contains("输出超过"));
    sandbox.shutdown().await;
}

#[tokio::test]
#[ignore = "requires Docker and the pinned Python image"]
async fn timeout_and_dropped_caller_remove_containers_before_shutdown_returns() {
    let (sandbox, owner) = sandbox(1);
    let timeout = sandbox
        .execute(request("import time; time.sleep(30)"))
        .await;
    assert!(containers(&owner).await.trim().is_empty());
    assert!(timeout.unwrap_err().contains("超过时限"));
    let worker = sandbox.clone();
    let job =
        tokio::spawn(async move { worker.execute(request("import time; time.sleep(30)")).await });
    tokio::time::timeout(Duration::from_secs(10), async {
        while containers(&owner).await.trim().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    job.abort();
    let _ = job.await;
    tokio::time::timeout(Duration::from_secs(20), sandbox.shutdown())
        .await
        .unwrap();
    assert!(containers(&owner).await.trim().is_empty());
    assert!(sandbox.execute(request("print('{}')")).await.is_err());
}
