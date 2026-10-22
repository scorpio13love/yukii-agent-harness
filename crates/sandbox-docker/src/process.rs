use super::DockerConfig;
use serde_json::Value;
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use tokio_util::sync::CancellationToken;
use yukii_agent_ports::sandbox::SandboxRequest;

const OUTPUT_LIMIT: usize = 64 * 1024;

pub(crate) fn validate(request: &SandboxRequest) -> Result<(), String> {
    let (repository, digest) = request
        .image
        .split_once("@sha256:")
        .ok_or("插件镜像必须固定 sha256 摘要")?;
    if repository.is_empty()
        || repository.starts_with('-')
        || repository.len() > 200
        || !repository
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || b"/._:-".contains(&value))
        || digest.len() != 64
        || !digest.bytes().all(|value| value.is_ascii_hexdigit())
    {
        return Err("无效容器镜像摘要".into());
    }
    if request.command.is_empty()
        || request.command.len() > 16
        || !request.command[0].starts_with('/')
        || request
            .command
            .iter()
            .any(|value| value.contains('\0') || value.len() > 16000)
        || request.input.to_string().len() > 64 * 1024
    {
        return Err("无效的容器入口或输入超过限制".into());
    }
    Ok(())
}

fn docker() -> Command {
    let mut command = Command::new("docker");
    // 不把 API Key 或任意 YUKII_* 环境交给子进程。Docker 客户端只保留连接引擎所需环境。
    command.env_clear();
    for name in [
        "PATH",
        "HOME",
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "DOCKER_CONFIG",
        "XDG_RUNTIME_DIR",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.kill_on_drop(true);
    command
}

/// 命令来自已校验清单，所有隔离选项由宿主固定；不接受 volume、特权或网络配置。
fn create_arguments(config: &DockerConfig, request: &SandboxRequest, name: &str) -> Vec<String> {
    let mut args = [
        "create",
        "--pull=never",
        "--interactive",
        "--init",
        "--network=none",
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges:true",
        "--pids-limit=64",
        "--memory=256m",
        "--memory-swap=256m",
        "--cpus=1",
        "--user=65532:65532",
        "--tmpfs=/tmp:rw,noexec,nosuid,size=16777216,mode=1777",
        "--workdir=/tmp",
        "--restart=no",
        "--stop-timeout=1",
        "--log-driver=none",
        "--name",
        name,
        "--label",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    args.push(format!("io.yukii.owner={}", config.owner));
    if let Some(runtime) = &config.runtime {
        args.extend(["--runtime".into(), runtime.clone()]);
    }
    args.extend([
        "--entrypoint".into(),
        request.command[0].clone(),
        request.image.clone(),
    ]);
    args.extend(request.command[1..].iter().cloned());
    args
}

pub(crate) async fn control(args: &[String], timeout: Duration) -> Result<String, String> {
    let mut child = docker()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "无法启动 Docker 客户端")?;
    let stdout = child.stdout.take().ok_or("无法读取 Docker 响应")?;
    tokio::time::timeout(timeout, async {
        let (bytes, status) = tokio::try_join!(read_limited(stdout, 16 * 1024), async {
            child
                .wait()
                .await
                .map_err(|_| "Docker 进程等待失败".to_owned())
        })?;
        if !status.success() {
            return Err("Docker 操作失败，请检查引擎、镜像和运行时配置".into());
        }
        String::from_utf8(bytes).map_err(|_| "Docker 响应不是有效 UTF-8".into())
    })
    .await
    .map_err(|_| "Docker 控制操作超时".to_owned())?
}
pub(crate) async fn remove(name: &str) -> Result<(), String> {
    control(
        &["rm".into(), "--force".into(), name.into()],
        Duration::from_secs(5),
    )
    .await
    .map(|_| ())
}

pub(crate) async fn execute(
    config: DockerConfig,
    request: SandboxRequest,
    cancel: CancellationToken,
) -> Result<Value, String> {
    let name = format!("yukii-{}", uuid::Uuid::new_v4());
    let created = control(
        &create_arguments(&config, &request, &name),
        Duration::from_secs(10),
    )
    .await;
    if let Err(error) = created {
        // create 超时也尝试清理：Docker 服务端可能已创建成功但客户端未收到回复。
        let _ = remove(&name).await;
        return Err(error);
    }
    let result = tokio::select! {
        biased;
        _=cancel.cancelled()=>Err("沙箱调用已取消".into()),
        value=tokio::time::timeout(config.timeout,run_container(&name,&request.input))=>value.map_err(|_|"沙箱执行超过时限".to_owned()).and_then(|value|value),
    };
    if let Err(error) = remove(&name).await {
        tracing::error!(container=%name,"沙箱容器清理失败，重启时将按实例标签回收");
        return Err(format!("{error}；容器清理未完成"));
    }
    result
}

async fn run_container(name: &str, input: &Value) -> Result<Value, String> {
    let mut child = docker()
        .args(["start", "--attach", "--interactive", name])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| "启动容器失败")?;
    let mut stdin = child.stdin.take().ok_or("容器输入不可用")?;
    let stdout = child.stdout.take().ok_or("容器输出不可用")?;
    let stderr = child.stderr.take().ok_or("容器错误输出不可用")?;
    let input = format!("{input}\n");
    let (_, output, _, status) = tokio::try_join!(
        async {
            stdin
                .write_all(input.as_bytes())
                .await
                .map_err(|_| "写入容器输入失败".to_owned())?;
            drop(stdin);
            Ok(())
        },
        read_limited(stdout, OUTPUT_LIMIT),
        read_limited(stderr, 16 * 1024),
        async { child.wait().await.map_err(|_| "等待容器失败".to_owned()) }
    )?;
    if !status.success() {
        return Err("容器工具执行失败（不回显未经处理的 stderr）".into());
    }
    serde_json::from_slice(&output).map_err(|_| "容器工具必须在 stdout 返回单个 JSON 值".into())
}
async fn read_limited(reader: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| "读取容器输出失败")?;
    if bytes.len() > limit {
        return Err("容器输出超过大小上限".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sandbox_never_grants_host_files_network_or_mutable_images() {
        let mut request = SandboxRequest {
            image: format!("python@sha256:{}", "a".repeat(64)),
            command: vec![
                "/usr/local/bin/python3".into(),
                "-c".into(),
                "print(1)".into(),
            ],
            input: Value::Null,
        };
        validate(&request).unwrap();
        let args = create_arguments(
            &DockerConfig {
                enabled: true,
                runtime: Some("runsc".into()),
                timeout: Duration::from_secs(1),
                owner: "test".into(),
            },
            &request,
            "test-container",
        );
        for required in [
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges:true",
            "--pull=never",
            "--user=65532:65532",
        ] {
            assert!(args.iter().any(|value| value == required));
        }
        assert!(
            !args
                .iter()
                .any(|value| value == "--privileged" || value == "--volume" || value == "--mount")
        );
        request.image = "python:latest".into();
        assert!(validate(&request).is_err());
    }
}
