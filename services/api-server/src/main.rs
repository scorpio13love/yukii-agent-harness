use chrono::{TimeZone, Utc};
use cron::Schedule as CronSchedule;
use sha2::{Digest, Sha256};
use std::str::FromStr;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use yukii_agent_core::runner::AgentLimits;
use yukii_agent_extensions::{ExtensionManager, marketplace::Marketplace};
use yukii_agent_memory::MemoryService;
use yukii_agent_ports::sandbox::Sandbox;
use yukii_agent_ports::tools::ToolCatalog;
use yukii_agent_rag::RagService;
use yukii_agent_runtime::Components;
use yukii_api_server::{AppState, configured_router, executor::Executor, store::TaskStore};
use yukii_contracts::CreateTask;
use yukii_mcp_client::{McpConfig, McpToolCatalog};
use yukii_model_gateway::{HttpModel, LanguageModel, config::Settings};
use yukii_office_tools::OfficeTools;
use yukii_sandbox_docker::{DockerConfig, DockerSandbox};
use yukii_tool_gateway::CompositeToolCatalog;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("yukii_api_server=info,yukii_agent_runtime=info,yukii_agent_infra=info,yukii_sandbox_docker=info,yukii_agent_rag=info")
        .init();
    let settings = Settings::load(Path::new(".env"))?;
    // 先占用端口，防止第二个实例在启动失败前改写正在运行的任务。
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8765").await?;

    let directory = PathBuf::from(match settings.get("YUKII_DATA_DIR") {
        "" => ".data",
        value => value,
    });
    std::fs::create_dir_all(&directory)?;
    let _instance = yukii_agent_infra::InstanceLock::acquire(directory.join("service.lock"))?;
    let store = Arc::new(TaskStore::open(directory.join("tasks.sqlite"))?);
    let recovered = store.recover_interrupted()?;
    let recovered_schedules = store.recover_schedules()?;
    tracing::info!(recovered_schedules, "已恢复计划任务领取状态");
    tracing::info!(recovered, "已检查中断任务");
    let model: Option<Arc<dyn LanguageModel>> = settings
        .model()?
        .map(|config| HttpModel::new(config).map(|model| Arc::new(model) as Arc<dyn LanguageModel>))
        .transpose()?;
    let limits = AgentLimits {
        max_turns: settings.number("YUKII_AGENT_MAX_TURNS", 80, 1, 80)? as u32,
        max_tool_calls: settings.number("YUKII_AGENT_MAX_TOOL_CALLS", 200, 1, 200)? as u32,
        max_total_tokens: settings.number(
            "YUKII_AGENT_MAX_TOTAL_TOKENS",
            1_000_000,
            100,
            1_000_000,
        )?,
        timeout: Duration::from_secs(settings.number(
            "YUKII_AGENT_TIMEOUT_SECONDS",
            300,
            5,
            1800,
        )?),
    };
    let embedding = settings
        .embedding()?
        .map(yukii_model_gateway::embedding::HttpEmbedding::new)
        .transpose()?
        .map(|model| Arc::new(model) as Arc<dyn yukii_agent_ports::embedding::EmbeddingModel>);
    let rag = Arc::new(RagService::with_embedding(store.clone(), embedding));
    let sandbox_backend = settings.get("YUKII_SANDBOX_BACKEND");
    if !matches!(sandbox_backend, "" | "docker" | "disabled") {
        return Err("YUKII_SANDBOX_BACKEND 仅支持 docker 或 disabled".into());
    }
    let sandbox = Arc::new(DockerSandbox::new(DockerConfig {
        enabled: sandbox_backend != "disabled",
        runtime: match settings.get("YUKII_DOCKER_RUNTIME") {
            "" => None,
            value => Some(value.into()),
        },
        timeout: Duration::from_secs(settings.number(
            "YUKII_SANDBOX_TIMEOUT_SECONDS",
            30,
            1,
            120,
        )?),
        owner: format!(
            "{:x}",
            Sha256::digest(directory.canonicalize()?.to_string_lossy().as_bytes())
        ),
    })?);
    if sandbox.status().await.available {
        sandbox.cleanup_orphans().await?;
    }
    let marketplace = Arc::new(Marketplace::new(
        PathBuf::from(match settings.get("YUKII_PLUGIN_MARKETPLACE_DIR") {
            "" => "examples/marketplace",
            value => value,
        }),
        match settings.get("YUKII_PLUGIN_MARKETPLACE_URL") {
            "" => None,
            value => Some(value),
        },
    )?);
    let trusted_keys = yukii_model_gateway::config::Settings::load(Path::new(".env"))
        .ok()
        .and_then(|settings| {
            settings
                .get("YUKII_PLUGIN_TRUSTED_KEYS")
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| {
                    item.split_once('=')
                        .map(|(publisher, key)| (publisher.to_owned(), key.to_owned()))
                })
                .collect::<Option<std::collections::BTreeMap<_, _>>>()
        })
        .unwrap_or_default();
    let extensions = Arc::new(
        ExtensionManager::new(store.clone(), marketplace, sandbox.clone())
            .with_trusted_publishers(trusted_keys),
    );
    let mcp = match settings.get("YUKII_MCP_CONFIG") {
        "" => None,
        path => {
            let config = McpConfig::load(path)?;
            Some(Arc::new(
                McpToolCatalog::connect_with_state(
                    config.servers,
                    Some(directory.join("mcp-state.json")),
                )
                .await?,
            ))
        }
    };
    let memory = Arc::new(MemoryService::open(directory.join("memory.json"))?);
    let skills = Arc::new(yukii_agent_extensions::SkillContextProvider::new(
        extensions.clone(),
    ));
    let schedule_tools = Arc::new(yukii_api_server::schedule_tools::ScheduleTools::new(
        store.clone(),
    ));
    let dev_tools = Arc::new(yukii_api_server::dev_tools::DevTools::new(store.clone()));
    let session_tools = Arc::new(yukii_api_server::session_tools::SessionTools::new(
        store.clone(),
    ));
    let mut catalogs: Vec<Arc<dyn ToolCatalog>> = vec![
        schedule_tools,
        session_tools,
        dev_tools,
        Arc::new(yukii_api_server::browser_tools::BrowserTools::new()),
        extensions.clone() as Arc<dyn ToolCatalog>,
        Arc::new(OfficeTools::from_env()),
        memory.clone(),
    ];
    if let Some(mcp) = &mcp {
        catalogs.push(mcp.clone());
    }
    if let Some(web_search) = yukii_api_server::web_search::WebSearchCatalog::from_env()? {
        catalogs.push(Arc::new(web_search));
    }
    let tools: Arc<dyn ToolCatalog> = Arc::new(CompositeToolCatalog::new(catalogs));
    let executor = Executor::with_components(
        store.clone(),
        Components {
            model: model.clone(),
            tools: tools.clone(),
            contexts: vec![rag.clone(), memory.clone(), skills],
            system_prompt: None,
            context_window_tokens: settings.number(
                "YUKII_CONTEXT_WINDOW_TOKENS",
                128_000,
                1024,
                10_000_000,
            )? as usize,
            max_output_tokens: settings.number("YUKII_MAX_OUTPUT_TOKENS", 8_192, 64, 32768)?
                as usize,
            compaction_headroom_tokens: settings.number(
                "YUKII_COMPACT_HEADROOM_TOKENS",
                16_384,
                0,
                1_000_000,
            )? as usize,
        },
        limits,
    );
    let office = Arc::new(yukii_api_server::office::OfficeServices {
        rag: rag.clone(),
        extensions,
        sandbox: sandbox.clone(),
    });
    tracing::info!(
        agent_configured = executor.available(),
        "模型配置检查完成（未发起请求）"
    );
    // 配置和组件全部校验成功后再启动索引，避免启动失败时向供应商发送付费请求。
    let index_stop = tokio_util::sync::CancellationToken::new();
    let indexer = tokio::spawn(rag.serve_indexer(index_stop.clone()));
    let worker = tokio::spawn(executor.clone().serve());
    // 一次性计划任务：领取到期行，创建普通持久任务，由既有 worker 执行。
    let schedule_store = store.clone();
    let schedule_stop = index_stop.clone();
    let scheduler = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = schedule_stop.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_secs() as i64)
                .unwrap_or(0);
            let Ok(Some(schedule)) = schedule_store.claim_due_schedule(now) else {
                continue;
            };
            let id = schedule["id"].as_str().unwrap_or_default().to_owned();
            let prompt = schedule["prompt"].as_str().unwrap_or_default().to_owned();
            let run_at = schedule["run_at"].as_i64().unwrap_or(now);
            let conversation_id = schedule["conversation_id"]
                .as_str()
                .map(ToOwned::to_owned)
                .or_else(|| Some("00000000-0000-0000-0000-000000000001".into()));
            let repeat_seconds = schedule["repeat_seconds"].as_i64().unwrap_or(0);
            let attempts = schedule["attempts"].as_i64().unwrap_or(0);
            let max_attempts = schedule["max_attempts"].as_i64().unwrap_or(3);
            if schedule["target"].as_str() != Some("cloud_office") {
                let _ = schedule_store.fail_schedule(&id, "不支持的计划 target");
                continue;
            }
            let key = format!("schedule:{id}:{run_at}");
            match schedule_store.create_once(
                CreateTask {
                    conversation_id: conversation_id.clone(),
                    prompt: prompt.clone(),
                    target: yukii_contracts::ExecutionTarget::CloudOffice,
                },
                &key,
                true,
            ) {
                Ok(created) => {
                    let _ = schedule_store.finish_schedule(&id, &created.task.id);
                    if let Ok(Some(expression)) = schedule_store.schedule_cron(&id)
                        && let Ok(schedule) = CronSchedule::from_str(&expression)
                        && let Some(base) = Utc.timestamp_opt(run_at, 0).single()
                        && let Some(next) = schedule.after(&base).next()
                        && let Ok(created_next) = schedule_store.create_schedule(
                            conversation_id.as_deref(),
                            &prompt,
                            "cloud_office",
                            next.timestamp(),
                            0,
                            max_attempts,
                        )
                        && let Some(next_id) = created_next["id"].as_str()
                    {
                        let _ = schedule_store.set_schedule_cron(next_id, &expression);
                    }
                    if repeat_seconds >= 60 {
                        let _ = schedule_store.create_schedule(
                            conversation_id.as_deref(),
                            &prompt,
                            "cloud_office",
                            run_at + repeat_seconds,
                            repeat_seconds,
                            max_attempts,
                        );
                    }
                    tracing::info!(schedule_id=%id, task_id=%created.task.id, "计划任务已入队");
                }
                Err(error) => {
                    if attempts + 1 < max_attempts {
                        let _ = schedule_store.retry_schedule(&id, now + 60);
                        tracing::warn!(schedule_id=%id, %error, attempts, "计划任务失败，60 秒后重试");
                    } else {
                        let _ = schedule_store.fail_schedule(&id, &error.to_string());
                        tracing::warn!(schedule_id=%id, %error, "计划任务达到最大尝试次数");
                    }
                }
            }
        }
    });
    let state = AppState {
        store,
        executor: executor.clone(),
        office: Some(office),
        mcp,
        uploads: directory.join("uploads"),
        model: model.clone(),
        memory: Some(memory.clone()),
        tools: Some(tools.clone()),
        web_dist: std::env::var("YUKII_WEB_DIST")
            .ok()
            .map(std::path::PathBuf::from),
    };
    tracing::info!("API 已启动：http://127.0.0.1:8765（单实例、回环访问）");
    axum::serve(listener, configured_router(state))
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            executor.shutdown();
            index_stop.cancel();
        })
        .await?;
    worker.await?;
    scheduler.await?;
    indexer.await?;
    sandbox.shutdown().await;
    Ok(())
}

/// SIGTERM 与 Ctrl+C 都先停止领取任务，再等待已领取任务保存取消终态。
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
            }
            Err(error) => {
                tracing::error!(%error,"无法监听 SIGTERM");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
