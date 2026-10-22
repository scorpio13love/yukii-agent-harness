//! Docker 隔离执行适配器。
mod process;
use async_trait::async_trait;
use serde_json::Value;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Semaphore;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use yukii_agent_ports::sandbox::{Sandbox, SandboxRequest, SandboxStatus};

#[derive(Clone)]
pub struct DockerConfig {
    pub enabled: bool,
    pub runtime: Option<String>,
    pub timeout: Duration,
    /// 稳定的数据目录标识；重启只清理本实例创建的遗留容器。
    pub owner: String,
}
pub struct DockerSandbox {
    config: DockerConfig,
    tasks: TaskTracker,
    slots: Arc<Semaphore>,
    closing: AtomicBool,
}
impl DockerSandbox {
    pub fn new(config: DockerConfig) -> Result<Self, String> {
        if config
            .runtime
            .as_deref()
            .is_some_and(|runtime| !matches!(runtime, "runc" | "runsc"))
        {
            return Err("沙箱 runtime 仅支持 runc 或 runsc".into());
        }
        if config.owner.is_empty()
            || !config
                .owner
                .bytes()
                .all(|value| value.is_ascii_alphanumeric() || value == b'-')
            || config.timeout.is_zero()
            || config.timeout > Duration::from_secs(120)
        {
            return Err("无效沙箱配置".into());
        }
        Ok(Self {
            config,
            tasks: TaskTracker::new(),
            slots: Arc::new(Semaphore::new(2)),
            closing: AtomicBool::new(false),
        })
    }
    pub async fn cleanup_orphans(&self) -> Result<(), String> {
        if !self.config.enabled {
            return Ok(());
        }
        let output = process::control(
            &[
                "ps".into(),
                "-aq".into(),
                "--filter".into(),
                format!("label=io.yukii.owner={}", self.config.owner),
            ],
            Duration::from_secs(5),
        )
        .await?;
        for id in output.split_whitespace() {
            if id.len() > 64 || !id.bytes().all(|value| value.is_ascii_hexdigit()) {
                return Err("Docker 返回了无效容器 ID".into());
            }
            process::remove(id).await?;
        }
        Ok(())
    }
}
struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[async_trait]
impl Sandbox for DockerSandbox {
    async fn status(&self) -> SandboxStatus {
        let configured = self.config.enabled;
        let available = configured
            && !self.closing.load(Ordering::Acquire)
            && process::control(
                &[
                    "info".into(),
                    "--format".into(),
                    "{{.ServerVersion}}".into(),
                ],
                Duration::from_secs(3),
            )
            .await
            .is_ok();
        SandboxStatus {
            configured,
            available,
            backend: self
                .config
                .runtime
                .clone()
                .unwrap_or_else(|| "docker".into()),
            message: if available {
                "Docker 引擎可用；插件镜像仍需预先准备"
            } else if configured {
                "Docker 引擎不可用，容器工具不会回退到宿主执行"
            } else {
                "容器沙箱已禁用"
            }
            .into(),
        }
    }
    async fn execute(&self, request: SandboxRequest) -> Result<Value, String> {
        if !self.config.enabled || self.closing.load(Ordering::Acquire) {
            return Err("沙箱未启用或正在关闭".into());
        }
        process::validate(&request)?;
        let token = CancellationToken::new();
        let guard = CancelOnDrop(token.clone());
        let config = self.config.clone();
        let slots = self.slots.clone();
        // 外层 Agent 超时会丢弃 Future；独立的受跟踪任务仍负责杀容器和清理。
        let job = self.tasks.spawn(async move {
            let _permit = tokio::select! {
                _ = token.cancelled() => return Err("沙箱调用已取消".into()),
                value = slots.acquire_owned() => value.map_err(|_| "沙箱正在关闭")?,
            };
            process::execute(config, request, token).await
        });
        let result = job.await.map_err(|_| "沙箱监督任务异常".to_owned())?;
        drop(guard);
        result
    }
    async fn shutdown(&self) {
        self.closing.store(true, Ordering::Release);
        self.slots.close();
        self.tasks.close();
        self.tasks.wait().await;
    }
}
