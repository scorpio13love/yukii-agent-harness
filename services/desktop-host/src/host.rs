//! 受信桌面宿主：选择窗口、取得用户同意、管理 Executor 生命周期并运行本机 Agent。
//! 它不是网页 API 的一部分；浏览器无法创建本机任务或访问桌面桥。
use crate::{
    approval::{ActionApprover, AllowAllActions, BrowserApprover, TerminalApprover},
    browser::ApprovedBrowser,
    ipc::IpcControl,
    preview::{PreviewEvent, PreviewPanel, WindowPicker},
};
use std::{
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::mpsc, time::timeout};
use yukii_agent_core::runner::AgentLimits;
use yukii_agent_infra::{InstanceLock, TaskStore};
use yukii_agent_ports::{
    browser::BrowserControl,
    model::LanguageModel,
    tools::{DesktopControl, ToolCatalog},
};
use yukii_agent_runtime::{Components, Executor};
use yukii_browser_use::{BrowserConfig, BrowserSession, BrowserTools};
use yukii_contracts::{
    CreateTask, ExecutionTarget, Task, TaskStatus,
    desktop::{ComputerPermissions, DesktopWindow},
};
use yukii_desktop_tools::ComputerTools;
use yukii_mcp_client::{McpConfig, McpToolCatalog};
use yukii_model_gateway::{HttpModel, config::Settings};
use yukii_tool_gateway::CompositeToolCatalog;

pub struct Options {
    pub prompt: String,
    pub data_dir: PathBuf,
    pub executor: PathBuf,
    pub bridge: PathBuf,
    pub window_id: Option<u32>,
    pub pid: Option<i32>,
    /// 按应用名筛选窗口；只有一个匹配时自动选择，避免用户看整张进程列表。
    pub application: Option<String>,
    /// 按标题子串筛选窗口；与 application 同时提供时取交集。
    pub window_title: Option<String>,
    pub control_minutes: i64,
    /// 浏览器模式：启动独立 profile 的 Chrome 并打开初始 URL。
    pub browser_url: Option<String>,
    /// 浏览器模式：连接已有 CDP 端点（测试或高级用法）。
    pub browser_endpoint: Option<String>,
    pub browser_profile: Option<PathBuf>,
    pub browser_path: Option<PathBuf>,
    /// 可选 MCP 配置；桌面任务的所有 MCP 工具强制使用 LocalComputer。
    pub mcp_config: Option<PathBuf>,
    pub close_browser: bool,
    /// 原生实时预览/控制面板路径；默认 target/native/preview-panel。
    pub preview_panel: Option<PathBuf>,
    pub no_preview: bool,
    /// 用原生窗口缩略图选择器完成选择 + 会话同意；通过后本次会话不再逐动作审批。
    pub panel_select: bool,
    pub window_picker: Option<PathBuf>,
    pub no_confirm_actions: bool,
    /// 仅跳过会话同意文本；仍要求显式窗口 ID/PID，避免盲选。
    pub yes: bool,
    /// 以 JSON 行输出任务事件/结果，供受信 Electron main 消费；默认人类可读输出。
    pub json_events: bool,
    /// 只做应用解析：调用模型从本机应用列表中选一个目标，输出 JSON 后退出。
    pub select_app: bool,
}

pub async fn run(mut options: Options) -> Result<(), Box<dyn std::error::Error>> {
    if options.select_app {
        return resolve_app_with_model(&options).await;
    }
    let browser_mode = options.browser_url.is_some() || options.browser_endpoint.is_some();
    let panel_select = options.panel_select;
    if options.browser_url.is_some() && options.browser_endpoint.is_some() {
        return Err("--browser-url 与 --browser-endpoint 只能二选一".into());
    }
    if browser_mode
        && (options.window_id.is_some()
            || options.pid.is_some()
            || options.application.is_some()
            || options.window_title.is_some())
    {
        return Err("浏览器模式不需要 --window-id/--pid/--app/--window-title".into());
    }
    if !panel_select && !std::io::stdin().is_terminal() && !options.yes {
        return Err("非交互运行必须显式加 --yes；交互模式请在终端运行".into());
    }
    if !browser_mode
        && !panel_select
        && options.yes
        && options.window_id.is_none()
        && options.pid.is_none()
        && options.application.is_none()
        && options.window_title.is_none()
    {
        return Err(
            "--yes 必须显式指定窗口：--window-id/--pid，或 --app/--window-title 过滤".into(),
        );
    }
    let settings = Settings::load(Path::new(".env"))?;
    if options.mcp_config.is_none() {
        options.mcp_config = match settings.get("YUKII_MCP_CONFIG") {
            "" => None,
            path => Some(PathBuf::from(path)),
        };
    }
    if !settings.vision_input()? {
        return Err("Computer Use / Browser Use 需要视觉模型。请在 .env 设置 YUKII_VISION_INPUT=true，并确认所配置模型支持图像输入。".into());
    }
    let model_config = settings
        .model()?
        .ok_or("未配置聊天模型；请填写 .env 中的 API 格式、密钥和模型名")?;
    let model: Arc<dyn LanguageModel> = Arc::new(HttpModel::new(model_config)?);
    // 桌面任务每一步通常需要一次观察 + 一次动作，8 轮不足以完成真实操作；
    // 这些默认值只影响桌面宿主，不改变网页办公任务的服务端默认。
    // 桌面与网页办公使用独立预算键，避免 .env 中网页的 8 轮/小 token 限制误伤桌面任务。
    let limits = AgentLimits {
        max_turns: settings.number("YUKII_DESKTOP_MAX_TURNS", 80, 1, 120)? as u32,
        max_tool_calls: settings.number("YUKII_DESKTOP_MAX_TOOL_CALLS", 240, 1, 300)? as u32,
        max_total_tokens: settings.number(
            "YUKII_DESKTOP_MAX_TOTAL_TOKENS",
            1500000,
            100,
            2000000,
        )?,
        timeout: Duration::from_secs(settings.number(
            "YUKII_DESKTOP_TIMEOUT_SECONDS",
            3600,
            5,
            1800,
        )?),
    };

    std::fs::create_dir_all(&options.data_dir)?;
    let data_dir = options.data_dir.canonicalize()?;
    let _instance = InstanceLock::acquire(data_dir.join("desktop-host.lock"))?;
    let database = data_dir.join("tasks.sqlite");
    let store = Arc::new(TaskStore::open(&database)?);
    let recovered = store.recover_interrupted()?;
    if recovered > 0 {
        eprintln!("已把 {recovered} 项上次中断的任务标记为失败，未自动重放。");
    }

    if !panel_select && !std::io::stdin().is_terminal() && !options.no_confirm_actions {
        return Err(
            "逐动作审批需要交互终端；若要在受控脚本中运行，请显式加 --no-confirm-actions。".into(),
        );
    }
    if browser_mode {
        return run_browser(&options, &data_dir, &store, model, limits).await;
    }
    if panel_select {
        eprintln!("使用窗口选择器：选择目标并点“允许控制”后，本次会话不再逐动作询问。");
        let session_id = uuid::Uuid::new_v4().to_string();
        let ipc = Arc::new(IpcControl::spawn(
            &options.executor,
            &database,
            &session_id,
            &options.bridge,
            Arc::new(AllowAllActions),
        )?);
        let outcome = run_panel_session(&options, &store, model, limits, ipc.clone()).await;
        ipc.shutdown().await;
        return outcome;
    }
    let approver: Arc<dyn ActionApprover> = if options.no_confirm_actions {
        eprintln!(
            "警告：已关闭逐动作审批。会话同意与窗口范围仍然生效，用户可切换前台窗口中断动作。"
        );
        Arc::new(AllowAllActions)
    } else {
        Arc::new(TerminalApprover::default())
    };

    let session_id = uuid::Uuid::new_v4().to_string();
    let ipc = Arc::new(IpcControl::spawn(
        &options.executor,
        &database,
        &session_id,
        &options.bridge,
        approver,
    )?);
    let outcome = run_session(&options, &store, model, limits, ipc.clone()).await;
    ipc.shutdown().await;
    outcome
}

/// 组合根与集成测试共用的会话执行入口；调用方负责提供已建立的 IPC 与模型。
#[derive(Default)]
struct PreselectedSession {
    window: Option<DesktopWindow>,
    panel: Option<PreviewPanel>,
    events: Option<mpsc::UnboundedReceiver<PreviewEvent>>,
}

async fn run_session_with(
    options: &Options,
    store: &Arc<TaskStore>,
    model: Arc<dyn LanguageModel>,
    limits: AgentLimits,
    ipc: Arc<IpcControl>,
    preselected: PreselectedSession,
) -> Result<(), Box<dyn std::error::Error>> {
    let PreselectedSession {
        window: preselected_window,
        panel: preselected_panel,
        events: preselected_events,
    } = preselected;
    ipc.hello().await.map_err(boxed)?;
    let permissions = ipc.permissions().await.map_err(boxed)?;
    print_permission_guidance(&permissions);
    require_permissions(&permissions)?;

    let windows = ipc.windows().await.map_err(boxed)?;
    if windows.is_empty() {
        return Err("没有可控制的窗口；请先打开目标应用窗口。".into());
    }
    let window = match preselected_window {
        Some(window) => {
            let allowed = windows.iter().any(|candidate| {
                candidate.window_id == window.window_id
                    && candidate.pid == window.pid
                    && !is_sensitive_application(&candidate.application)
            });
            if !allowed {
                return Err("面板选择的窗口已关闭、已变化或属于禁止控制的应用".into());
            }
            window
        }
        None => {
            let window = select_window(options, &windows)?;
            confirm_session(options, &window)?;
            window
        }
    };

    let created = store.create_local_once(
        CreateTask {
            conversation_id: None,
            prompt: options.prompt.clone(),
            target: ExecutionTarget::LocalComputer,
        },
        &uuid::Uuid::new_v4().to_string(),
        false,
    )?;
    let task_id = created.task.id.clone();
    let expires_at = now() + options.control_minutes * 60;
    if let Err(error) = ipc.begin_control(&task_id, &window, true, expires_at).await {
        let _ = store.cancel(&task_id);
        return Err(boxed(error));
    }
    eprintln!(
        "已开始受控会话：{}（窗口 {} / pid {}）。任务 {}",
        safe(&window.application),
        window.window_id,
        window.pid,
        task_id
    );
    if limits.timeout.as_secs() > (options.control_minutes * 60) as u64 {
        eprintln!(
            "警告：任务超时（{} 秒）长于桌面控制期限（{} 分钟）；超期后的动作会被拒绝。",
            limits.timeout.as_secs(),
            options.control_minutes
        );
    }

    let (mut preview, mut preview_events) = if let Some(panel) = preselected_panel {
        (Some(panel), preselected_events)
    } else {
        let mut panel = if options.no_preview {
            None
        } else {
            let path = options
                .preview_panel
                .clone()
                .unwrap_or_else(default_preview_panel);
            match PreviewPanel::spawn(&path, &window).await {
                Ok(panel) => Some(panel),
                Err(error) => {
                    eprintln!("实时预览不可用：{error}");
                    None
                }
            }
        };
        let events = panel.as_mut().and_then(|panel| panel.take_events());
        (panel, events)
    };
    if let Some(panel) = preview.as_mut() {
        let _ = panel
            .status(&format!(
                "Agent 执行中 · {}",
                &task_id[..8.min(task_id.len())]
            ))
            .await;
    }

    let control: Arc<dyn DesktopControl> = ipc.clone();
    let computer: Arc<dyn ToolCatalog> = Arc::new(ComputerTools::new(control));
    let tools: Arc<dyn ToolCatalog> = match options.mcp_config.as_deref() {
        None => computer,
        Some(path) => {
            let mut config = McpConfig::load(path)?;
            // 桌面任务的所有工具都必须匹配 LocalComputer；配置不能把 CUA 工具伪装成 CloudOffice。
            for server in &mut config.servers {
                server.target = Some(ExecutionTarget::LocalComputer);
            }
            let mcp = Arc::new(
                McpToolCatalog::connect_with_state(
                    config.servers,
                    Some(options.data_dir.join("mcp-state.json")),
                )
                .await?,
            );
            Arc::new(CompositeToolCatalog::new(vec![computer, mcp]))
        }
    };
    let executor = Executor::with_components(
        store.clone(),
        Components {
            model: Some(model),
            tools,
            contexts: Vec::new(),
            system_prompt: None,
            context_window_tokens: 128_000,
            max_output_tokens: 8_192,
            compaction_headroom_tokens: 16_384,
        },
        limits.clone(),
    );
    let worker = tokio::spawn(executor.clone().serve());
    if let Err(error) = executor.start(task_id.clone()).await {
        executor.shutdown();
        let _ = store.cancel(&task_id);
        if let Some(mut panel) = preview.take() {
            panel.close().await;
        }
        let _ = timeout(Duration::from_secs(10), worker).await;
        return Err(error.into());
    }

    let wait = wait_for_task(store, &task_id, limits.timeout, options.json_events);
    tokio::pin!(wait);
    let outcome = loop {
        tokio::select! {
            result = &mut wait => break result,
            _ = tokio::signal::ctrl_c() => {
                eprintln!("收到 Ctrl+C，正在取消任务并结束桌面控制。");
                break Err("用户停止".into());
            }
            event = async {
                match preview_events.as_mut() {
                    Some(receiver) => receiver.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match event {
                    Some(PreviewEvent::Stop) => {
                        eprintln!("控制面板请求停止任务。");
                        break Err("用户通过控制面板停止".into());
                    }
                    Some(PreviewEvent::Closed) => {
                        eprintln!("预览面板已关闭，停止任务。");
                        break Err("预览面板关闭".into());
                    }
                    Some(PreviewEvent::Paused) => {
                        match ipc.pause_control(&task_id).await {
                            Ok(()) => {
                                if let Some(panel) = preview.as_mut() {
                                    let _ = panel.status("Agent 已暂停：新的桌面动作会被拒绝").await;
                                }
                            }
                            Err(error) => {
                                if let Some(panel) = preview.as_mut() {
                                    let _ = panel.status(&format!("暂停失败：{error}")).await;
                                }
                            }
                        }
                    }
                    Some(PreviewEvent::Resumed) => {
                        match ipc.resume_control(&task_id).await {
                            Ok(()) => {
                                if let Some(panel) = preview.as_mut() {
                                    let _ = panel.status("Agent 已恢复；旧截图已失效，需要重新观察").await;
                                }
                            }
                            Err(error) => {
                                if let Some(panel) = preview.as_mut() {
                                    let _ = panel.status(&format!("恢复失败：{error}")).await;
                                }
                            }
                        }
                    }
                    Some(PreviewEvent::Ready { width, height }) => {
                        eprintln!("实时预览已启动：{width}×{height}");
                    }
                    Some(PreviewEvent::Error(message)) => {
                        eprintln!("预览面板错误：{message}；任务继续，但没有实时预览。");
                        preview_events = None;
                    }
                    None => {
                        preview_events = None;
                    }
                }
            }
        }
    };
    if outcome.is_err() {
        let _ = store.cancel(&task_id);
        executor.cancel(&task_id);
    }
    if let Some(mut panel) = preview {
        let text = if outcome.is_ok() {
            "任务结束"
        } else {
            "任务停止"
        };
        let _ = panel.status(text).await;
        panel.close().await;
    }
    executor.shutdown();
    let _ = timeout(Duration::from_secs(30), worker).await;
    let _ = ipc.end_control().await;
    outcome
}

async fn resolve_app_with_model(options: &Options) -> Result<(), Box<dyn std::error::Error>> {
    use tokio::io::AsyncWriteExt;
    let settings = Settings::load(Path::new(".env"))?;
    let model_config = settings
        .model()?
        .ok_or("未配置聊天模型；请填写 .env 中的 API 格式、密钥和模型名")?;
    let model = HttpModel::new(model_config)?;
    let mut child = tokio::process::Command::new(&options.bridge)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("原生桥输入不可用")?;
    stdin.write_all(b"{\"operation\":\"apps\"}").await?;
    drop(stdin);
    let output = child.wait_with_output().await?;
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let apps = value
        .get("apps")
        .and_then(|value| value.as_array())
        .ok_or("应用列表不可用")?
        .iter()
        .filter(|app| {
            let name = app
                .get("application")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            !is_sensitive_application(name) && !is_excluded_application(name)
        })
        .collect::<Vec<_>>();
    if apps.is_empty() {
        return Err("没有可控制的应用窗口".into());
    }
    let list = apps
        .iter()
        .map(|app| {
            format!(
                "{} | {} | {}",
                app.get("app_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or(""),
                app.get("application")
                    .and_then(|value| value.as_str())
                    .unwrap_or(""),
                app.get("title")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let system = "你是桌面 Computer Use 的应用解析器。根据用户任务，从应用列表中选择最合适的一个应用。只回复 app_id，不要解释。";
    let user = format!(
        "任务：{}\n\n应用列表（app_id | 应用 | 窗口标题）：\n{}",
        options.prompt, list
    );
    let reply = model
        .complete(
            system,
            &[yukii_agent_ports::model::Message::User(user)],
            &[],
        )
        .await
        .map_err(|error| format!("应用解析模型调用失败：{error}"))?;
    let raw = reply.text.trim().trim_matches('`').trim_matches('"').trim();
    let chosen = apps
        .iter()
        .find(|app| {
            let name = app
                .get("application")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            !is_excluded_application(name)
                && (app.get("app_id").and_then(|value| value.as_str()) == Some(raw)
                    || app.get("application").and_then(|value| value.as_str()) == Some(raw))
        })
        .or_else(|| {
            apps.iter().find(|app| {
                let app_id = app
                    .get("app_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                let name = app
                    .get("application")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                !is_excluded_application(name)
                    && ((!app_id.is_empty() && raw.contains(app_id))
                        || (!name.is_empty() && raw.contains(name)))
            })
        })
        .ok_or("应用解析没有返回可用应用")?;
    println!("{}", serde_json::to_string(chosen)?);
    Ok(())
}

/// 组合根与集成测试共用的会话执行入口；调用方负责提供已建立的 IPC 与模型。
pub async fn run_session(
    options: &Options,
    store: &Arc<TaskStore>,
    model: Arc<dyn LanguageModel>,
    limits: AgentLimits,
    ipc: Arc<IpcControl>,
) -> Result<(), Box<dyn std::error::Error>> {
    run_session_with(
        options,
        store,
        model,
        limits,
        ipc,
        PreselectedSession::default(),
    )
    .await
}

/// 面板选择器本身完成“选择 + 会话同意”；IPC 使用 AllowAll，后续只保留 Pause/Stop 边界。
async fn run_panel_session(
    options: &Options,
    store: &Arc<TaskStore>,
    model: Arc<dyn LanguageModel>,
    limits: AgentLimits,
    ipc: Arc<IpcControl>,
) -> Result<(), Box<dyn std::error::Error>> {
    ipc.hello().await.map_err(boxed)?;
    let permissions = ipc.permissions().await.map_err(boxed)?;
    print_permission_guidance(&permissions);
    require_permissions(&permissions)?;
    let windows = ipc.windows().await.map_err(boxed)?;
    if windows.is_empty() {
        return Err("没有可控制的窗口；请先打开目标应用窗口。".into());
    }
    let picker_path = options
        .window_picker
        .clone()
        .unwrap_or_else(default_window_picker);
    let mut chosen = None;
    for _ in 0..3 {
        let picked = WindowPicker::pick(&picker_path).await.map_err(boxed)?;
        if let Some(window) = windows.iter().find(|window| {
            window.window_id == picked.window_id
                && window.pid == picked.pid
                && !is_sensitive_application(&window.application)
        }) {
            chosen = Some(window.clone());
            break;
        }
        eprintln!("该窗口不能控制（终端/控制台或已变化），请重新选择。");
    }
    let window = chosen.ok_or("没有选择可控制窗口")?;
    let preview_path = options
        .preview_panel
        .clone()
        .unwrap_or_else(default_preview_panel);
    let mut panel = PreviewPanel::spawn(&preview_path, &window)
        .await
        .map_err(boxed)?;
    let events = panel.take_events().ok_or("预览面板事件通道不可用")?;
    eprintln!(
        "面板已授权控制：{} — {}（窗口 {} / pid {}）。本次会话不再逐动作审批，可用面板暂停/停止。",
        safe(&window.application),
        safe(&window.title),
        window.window_id,
        window.pid
    );
    run_session_with(
        options,
        store,
        model,
        limits,
        ipc,
        PreselectedSession {
            window: Some(window),
            panel: Some(panel),
            events: Some(events),
        },
    )
    .await
}

async fn run_browser(
    options: &Options,
    data_dir: &Path,
    store: &Arc<TaskStore>,
    model: Arc<dyn LanguageModel>,
    limits: AgentLimits,
) -> Result<(), Box<dyn std::error::Error>> {
    confirm_browser(options, data_dir)?;
    let session = if let Some(endpoint) = &options.browser_endpoint {
        BrowserSession::connect(endpoint, options.browser_url.as_deref())
            .await
            .map_err(boxed)?
    } else {
        let path = options
            .browser_path
            .clone()
            .or_else(default_chrome_path)
            .ok_or("找不到 Chrome/Chromium；请用 --browser-path 指定")?;
        let profile = options
            .browser_profile
            .clone()
            .unwrap_or_else(|| data_dir.join("browser-profile"));
        // 仅测试受限沙箱环境使用；生产 Chrome 应保留自己的 sandbox。
        let no_sandbox = std::env::var("YUKII_BROWSER_NO_SANDBOX").as_deref() == Ok("1");
        BrowserSession::launch(BrowserConfig {
            path,
            profile_dir: profile,
            initial_url: options.browser_url.clone(),
            no_sandbox,
        })
        .await
        .map_err(boxed)?
    };
    let session = Arc::new(session);
    let approver: Arc<dyn BrowserApprover> = if options.no_confirm_actions {
        eprintln!("警告：已关闭浏览器逐动作审批；页面仍可能产生提交/下载等外部效果。");
        Arc::new(AllowAllActions)
    } else {
        Arc::new(TerminalApprover::default())
    };
    let control: Arc<dyn BrowserControl> =
        Arc::new(ApprovedBrowser::new(session.clone(), approver));
    let browser: Arc<dyn ToolCatalog> = Arc::new(BrowserTools::new(control));
    let tools: Arc<dyn ToolCatalog> = match options.mcp_config.as_deref() {
        None => browser,
        Some(path) => {
            let mut config = McpConfig::load(path)?;
            for server in &mut config.servers {
                server.target = Some(ExecutionTarget::LocalComputer);
            }
            let mcp = Arc::new(
                McpToolCatalog::connect_with_state(
                    config.servers,
                    Some(data_dir.join("mcp-state.json")),
                )
                .await?,
            );
            Arc::new(CompositeToolCatalog::new(vec![browser, mcp]))
        }
    };
    let executor = Executor::with_components(
        store.clone(),
        Components {
            model: Some(model),
            tools,
            contexts: Vec::new(),
            system_prompt: Some(yukii_agent_core::runner::BROWSER_SYSTEM),
            context_window_tokens: 128_000,
            max_output_tokens: 8_192,
            compaction_headroom_tokens: 16_384,
        },
        limits.clone(),
    );
    let created = store.create_local_once(
        CreateTask {
            conversation_id: None,
            prompt: options.prompt.clone(),
            target: ExecutionTarget::LocalComputer,
        },
        &uuid::Uuid::new_v4().to_string(),
        false,
    )?;
    let task_id = created.task.id.clone();
    let worker = tokio::spawn(executor.clone().serve());
    if let Err(error) = executor.start(task_id.clone()).await {
        executor.shutdown();
        let _ = store.cancel(&task_id);
        let _ = timeout(Duration::from_secs(10), worker).await;
        if options.close_browser {
            session.close();
        }
        return Err(error.into());
    }
    let outcome = tokio::select! {
        result = wait_for_task(store, &task_id, limits.timeout, options.json_events) => result,
        _ = tokio::signal::ctrl_c() => {
            eprintln!("收到 Ctrl+C，正在取消浏览器任务。");
            Err("用户停止".into())
        }
    };
    if outcome.is_err() {
        let _ = store.cancel(&task_id);
        executor.cancel(&task_id);
    }
    executor.shutdown();
    let _ = timeout(Duration::from_secs(30), worker).await;
    if options.close_browser {
        session.close();
    } else {
        eprintln!("浏览器保持打开：{}", session.endpoint());
    }
    outcome
}

fn confirm_browser(options: &Options, data_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(endpoint) = &options.browser_endpoint {
        eprintln!(
            "即将连接已有 CDP 浏览器端点：{endpoint}\n             请确认这是专用/测试浏览器；该模式不会创建新 profile，也不会读取网页之外的桌面。"
        );
    } else {
        let profile = options
            .browser_profile
            .clone()
            .unwrap_or_else(|| data_dir.join("browser-profile"));
        eprintln!(
            "即将启动独立 Chrome profile：{}\n初始 URL：{}\n             不会读取你现有的 Chrome profile。网页内容与截图会发送给视觉模型。",
            profile.display(),
            options.browser_url.as_deref().unwrap_or("about:blank")
        );
    }
    eprintln!(
        "允许的操作：在浏览器标签页内导航、点击、输入、按键、滚动。         不要用它登录网银/邮箱，也不要输入密码、支付或验证码等敏感信息。"
    );
    if options.yes {
        eprintln!("已按 --yes 跳过浏览器会话确认。");
        return Ok(());
    }
    let answer = read_line("确认启动/连接浏览器？输入 yes 继续：")?;
    if answer.eq_ignore_ascii_case("yes") {
        Ok(())
    } else {
        Err("用户未同意浏览器会话".into())
    }
}

fn default_window_picker() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.parent()
                .and_then(|debug| debug.parent())
                .map(|target| target.join("native/window-picker"))
        })
        .unwrap_or_else(|| PathBuf::from("target/native/window-picker"))
}

fn default_preview_panel() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.parent()
                .and_then(|debug| debug.parent())
                .map(|target| target.join("native/preview-panel"))
        })
        .unwrap_or_else(|| PathBuf::from("target/native/preview-panel"))
}

fn default_chrome_path() -> Option<PathBuf> {
    [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
}

fn require_permissions(
    permissions: &ComputerPermissions,
) -> Result<(), Box<dyn std::error::Error>> {
    if !permissions.screen_capture {
        return Err(
            "缺少屏幕录制权限：系统设置 → 隐私与安全性 → 屏幕与系统音频录制，授权后重启宿主。"
                .into(),
        );
    }
    if !permissions.accessibility {
        return Err(
            "缺少辅助功能权限：系统设置 → 隐私与安全性 → 辅助功能，授权后重启宿主。".into(),
        );
    }
    Ok(())
}

fn print_permission_guidance(permissions: &ComputerPermissions) {
    if !permissions.screen_capture || !permissions.accessibility {
        eprintln!(
            "系统权限检查：屏幕录制={}，辅助功能={}",
            permissions.screen_capture, permissions.accessibility
        );
    }
}

fn select_window(
    options: &Options,
    windows: &[DesktopWindow],
) -> Result<DesktopWindow, Box<dyn std::error::Error>> {
    let candidates = windows
        .iter()
        .filter(|window| !is_sensitive_application(&window.application))
        .filter(|window| {
            window_matches(
                window,
                options.application.as_deref(),
                options.window_title.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Err(
            "没有匹配的可控窗口；请检查 --app/--window-title，终端/控制台窗口不能选择".into(),
        );
    }
    if options.window_id.is_none() && options.pid.is_none() && candidates.len() == 1 {
        let window = candidates[0];
        eprintln!(
            "自动选择唯一匹配窗口：{} — {}（{} / pid {}）",
            safe(&window.application),
            safe(&window.title),
            window.window_id,
            window.pid
        );
        return Ok((*window).clone());
    }
    match (options.window_id, options.pid) {
        (Some(window_id), Some(pid)) => {
            let window = candidates
                .iter()
                .find(|window| window.window_id == window_id && window.pid == pid)
                .ok_or("指定的窗口不存在、已关闭或属于禁止控制的应用")?;
            Ok((*window).clone())
        }
        (None, None) => {
            eprintln!("可控制窗口（已隐藏终端/控制台）：");
            for (index, window) in candidates.iter().enumerate() {
                eprintln!(
                    "  [{index}] {} — {} (window {}, pid {})",
                    safe(&window.application),
                    safe(&window.title),
                    window.window_id,
                    window.pid
                );
            }
            let input = read_line("输入窗口序号：")?;
            let index = input.parse::<usize>().map_err(|_| "窗口序号无效")?;
            let window = candidates.get(index).ok_or("窗口序号超出范围")?;
            Ok((*window).clone())
        }
        _ => Err("必须同时提供 --window-id 和 --pid".into()),
    }
}

fn window_matches(window: &DesktopWindow, application: Option<&str>, title: Option<&str>) -> bool {
    let application = application.map(str::to_lowercase);
    let title = title.map(str::to_lowercase);
    application.is_none_or(|needle| window.application.to_lowercase().contains(&needle))
        && title.is_none_or(|needle| window.title.to_lowercase().contains(&needle))
}

/// 终端中的任意文本输入加回车等同于执行命令，因此本版本不把终端/控制台窗口纳入范围。
/// 不让 CUA 选中自身 UI 或测试辅助窗口。
fn is_excluded_application(application: &str) -> bool {
    let normalized = application.to_lowercase();
    let configured = std::env::var("YUKII_EXCLUDED_APPS").unwrap_or_default();
    [
        "yukii",
        "yukii · 办公助手",
        "yukii control",
        "window-picker",
        "preview-panel",
    ]
    .iter()
    .copied()
    .chain(configured.split(','))
    .map(str::trim)
    .filter(|name| !name.is_empty())
    .any(|name| {
        let needle = name.to_lowercase();
        // 自身名是 Yukii，测试窗口叫 Yukii CU Test；只精确排除自身名，避免误伤测试目标。
        normalized == needle || (needle != "yukii" && normalized.contains(&needle))
    })
}

fn is_sensitive_application(application: &str) -> bool {
    let normalized = application.to_lowercase();
    [
        "terminal",
        "iterm",
        "warp",
        "console",
        "kitty",
        "alacritty",
        "hyper",
        "tabby",
        "ghostty",
    ]
    .iter()
    .any(|name| normalized.contains(name))
}

fn confirm_session(
    options: &Options,
    window: &DesktopWindow,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!(
        "\n即将允许 Agent 控制以下窗口：\n  应用：{}\n  标题：{}\n  窗口：{} / pid {}\n\
         允许的输入：点击、滚动、最多 256 字符文本、回车/制表/退格/方向键。\n\
         截图会发送给配置的视觉模型；截图中可能包含敏感信息。\n\
         点击、输入等动作仍会被限制在此窗口，终端/控制台窗口被禁止选择。",
        safe(&window.application),
        safe(&window.title),
        window.window_id,
        window.pid
    );
    if options.yes {
        if options.window_id.is_none()
            && options.pid.is_none()
            && options.application.is_none()
            && options.window_title.is_none()
        {
            return Err(
                "--yes 必须显式指定窗口：--window-id/--pid，或 --app/--window-title 过滤".into(),
            );
        }
        eprintln!("已按 --yes 跳过会话确认（目标由命令行显式指定或过滤）。");
        return Ok(());
    }
    let answer = read_line("确认允许截图分享与受控操作？输入 yes 继续：")?;
    if answer.eq_ignore_ascii_case("yes") {
        Ok(())
    } else {
        Err("用户未同意桌面控制".into())
    }
}

async fn wait_for_task(
    store: &TaskStore,
    task_id: &str,
    task_timeout: Duration,
    json_events: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + task_timeout + Duration::from_secs(60);
    let mut last_sequence = 0i64;
    loop {
        let events = store.events(task_id)?;
        for event in events {
            if event.sequence <= last_sequence {
                continue;
            }
            if json_events {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "status",
                        "sequence": event.sequence,
                        "status": event.status,
                        "message": event.message,
                        "created_at": event.created_at,
                    })
                );
            } else {
                println!("[{:?}] {}", event.status, event.message);
            }
            last_sequence = event.sequence;
        }
        let task = store.get(task_id)?;
        if is_terminal(task.status) {
            if json_events {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "result",
                        "task": task,
                    })
                );
            } else {
                print_task_result(&task);
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("任务超过宿主等待上限，已请求取消；这不是已确认的外部操作结果".into());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn is_terminal(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
    )
}

fn print_task_result(task: &Task) {
    println!("\n任务状态：{:?}", task.status);
    if let Some(result) = &task.result {
        println!("回复：\n{result}");
    }
    if let Some(error) = &task.error {
        println!("失败原因：{error}");
    }
    println!(
        "用量：输入 {} / 输出 {} token",
        task.input_tokens, task.output_tokens
    );
}

fn read_line(prompt: &str) -> Result<String, Box<dyn std::error::Error>> {
    eprint!("{prompt}");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line)? == 0 {
        return Err("没有可用的交互输入".into());
    }
    Ok(line.trim().to_owned())
}

fn safe(text: &str) -> String {
    let compact = text
        .chars()
        .filter(|character| !character.is_control())
        .take(80)
        .collect::<String>();
    if compact.is_empty() {
        "(无标题)".into()
    } else {
        compact
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn boxed(message: String) -> Box<dyn std::error::Error> {
    message.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_filters_match_application_and_title() {
        let window = DesktopWindow {
            window_id: 1,
            pid: 2,
            application: "Google Chrome".into(),
            title: "Example Domain".into(),
        };
        assert!(window_matches(&window, Some("chrome"), None));
        assert!(window_matches(&window, None, Some("example")));
        assert!(window_matches(&window, Some("google"), Some("domain")));
        assert!(!window_matches(&window, Some("Safari"), None));
        assert!(!window_matches(&window, None, Some("Safari")));
    }

    #[test]
    fn terminal_and_console_windows_are_never_offered() {
        for application in ["Terminal", "iTerm2", "Warp", "Console", "Ghostty"] {
            assert!(
                is_sensitive_application(application),
                "{application} 必须被排除"
            );
        }
        for application in ["TextEdit", "Microsoft Word", "Notes", "Preview"] {
            assert!(!is_sensitive_application(application));
        }
    }

    #[test]
    fn self_apps_are_not_cua_targets() {
        for application in ["Yukii", "Yukii · 办公助手"] {
            assert!(
                is_excluded_application(application),
                "{application} 必须被排除"
            );
        }
        for application in [
            "Google Chrome",
            "Safari",
            "Microsoft Word",
            "Notes",
            "Yukii CU Test",
        ] {
            assert!(!is_excluded_application(application));
        }
    }

    #[test]
    fn window_text_is_bounded_and_control_characters_are_removed() {
        let text = format!("标题\n{}", "x".repeat(200));
        let display = safe(&text);
        assert!(!display.contains('\n'));
        assert_eq!(display.chars().count(), 80);
        assert_eq!(safe("   "), "   ");
    }

    #[test]
    fn permission_failures_are_explicit() {
        assert!(
            require_permissions(&ComputerPermissions {
                screen_capture: false,
                accessibility: true
            })
            .is_err()
        );
        assert!(
            require_permissions(&ComputerPermissions {
                screen_capture: true,
                accessibility: false
            })
            .is_err()
        );
        assert!(
            require_permissions(&ComputerPermissions {
                screen_capture: true,
                accessibility: true
            })
            .is_ok()
        );
    }
}
