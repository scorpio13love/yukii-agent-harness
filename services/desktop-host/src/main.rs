//! 受信桌面宿主入口。只支持显式窗口选择与会话同意；网页进程不能启动此桥。
use std::{path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter("yukii_desktop_host=info,yukii_agent_runtime=info")
        .init();
    let options = match parse(std::env::args().skip(1).collect()) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("无法启动异步运行时：{error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(yukii_desktop_host::host::run(options)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("桌面宿主失败：{error}");
            ExitCode::FAILURE
        }
    }
}

fn parse(arguments: Vec<String>) -> Result<yukii_desktop_host::host::Options, String> {
    let mut prompt = None;
    let mut data_dir = PathBuf::from(".data/desktop-host");
    let mut executor = default_executor();
    let mut bridge = default_bridge();
    let mut window_id = None;
    let mut pid = None;
    let mut control_minutes = 15i64;
    let mut browser_url = None;
    let mut browser_endpoint = None;
    let mut browser_profile = None;
    let mut browser_path = None;
    let mut close_browser = false;
    let mut preview_panel = None;
    let mut no_preview = false;
    let mut panel_select = false;
    let mut window_picker = None;
    let mut no_confirm_actions = false;
    let mut yes = false;
    let mut json_events = false;
    let mut select_app = false;
    let mut application = None;
    let mut window_title = None;
    let mut arguments = arguments.into_iter();
    while let Some(flag) = arguments.next() {
        match flag.as_str() {
            "--prompt" => prompt = arguments.next(),
            "--data-dir" => data_dir = PathBuf::from(arguments.next().ok_or("--data-dir 缺少值")?),
            "--executor" => executor = PathBuf::from(arguments.next().ok_or("--executor 缺少值")?),
            "--bridge" => bridge = PathBuf::from(arguments.next().ok_or("--bridge 缺少值")?),
            "--app" => application = arguments.next(),
            "--window-title" => window_title = arguments.next(),
            "--browser-url" => browser_url = arguments.next(),
            "--browser-endpoint" => browser_endpoint = arguments.next(),
            "--browser-profile" => {
                browser_profile = Some(PathBuf::from(
                    arguments.next().ok_or("--browser-profile 缺少值")?,
                ))
            }
            "--browser-path" => {
                browser_path = Some(PathBuf::from(
                    arguments.next().ok_or("--browser-path 缺少值")?,
                ))
            }
            "--close-browser" => close_browser = true,
            "--preview-panel" => {
                preview_panel = Some(PathBuf::from(
                    arguments.next().ok_or("--preview-panel 缺少值")?,
                ))
            }
            "--no-preview" => no_preview = true,
            "--json-events" => json_events = true,
            "--select-app" => select_app = true,
            "--panel-select" => panel_select = true,
            "--window-picker" => {
                window_picker = Some(PathBuf::from(
                    arguments.next().ok_or("--window-picker 缺少值")?,
                ))
            }
            "--window-id" => {
                window_id = Some(
                    arguments
                        .next()
                        .ok_or("--window-id 缺少值")?
                        .parse()
                        .map_err(|_| "--window-id 必须是正整数")?,
                )
            }
            "--pid" => {
                pid = Some(
                    arguments
                        .next()
                        .ok_or("--pid 缺少值")?
                        .parse()
                        .map_err(|_| "--pid 必须是正整数")?,
                )
            }
            "--control-minutes" => {
                control_minutes = arguments
                    .next()
                    .ok_or("--control-minutes 缺少值")?
                    .parse()
                    .map_err(|_| "--control-minutes 必须是整数")?;
                if !(1..=30).contains(&control_minutes) {
                    return Err("--control-minutes 必须在 1 到 30 之间".into());
                }
            }
            "--no-confirm-actions" => no_confirm_actions = true,
            "--yes" => yes = true,
            "--help" | "-h" => {
                return Err(usage());
            }
            other => return Err(format!("未知参数：{other}\n\n{}", usage())),
        }
    }
    let prompt = prompt
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| usage().to_string())?;
    let browser_mode = browser_url.is_some() || browser_endpoint.is_some();
    if browser_url.is_some() && browser_endpoint.is_some() {
        return Err("--browser-url 与 --browser-endpoint 只能二选一".into());
    }
    if browser_mode
        && (window_id.is_some() || pid.is_some() || application.is_some() || window_title.is_some())
    {
        return Err("浏览器模式不需要 --window-id/--pid/--app/--window-title".into());
    }
    if !browser_mode {
        if !executor.is_file() {
            return Err(format!(
                "找不到 Executor：{}\n请先运行 ./tooling/cargo.sh build -p yukii-desktop-executor-service",
                executor.display()
            ));
        }
        if !bridge.is_file() {
            return Err(format!(
                "找不到原生桥：{}\n请先运行 ./desktop/macos-native/build.sh",
                bridge.display()
            ));
        }
    }
    Ok(yukii_desktop_host::host::Options {
        prompt,
        data_dir,
        executor,
        bridge,
        window_id,
        pid,
        application,
        window_title,
        control_minutes,
        browser_url,
        browser_endpoint,
        browser_profile,
        browser_path,
        mcp_config: None,
        close_browser,
        preview_panel,
        no_preview,
        panel_select,
        window_picker,
        no_confirm_actions,
        yes,
        json_events,
        select_app,
    })
}

fn default_executor() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.parent()
                .map(|parent| parent.join("yukii-desktop-executor-service"))
        })
        .unwrap_or_else(|| PathBuf::from("yukii-desktop-executor-service"))
}

fn default_bridge() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.parent()
                .and_then(|debug| debug.parent())
                .map(|target| target.join("native/computer-bridge"))
        })
        .unwrap_or_else(|| PathBuf::from("target/native/computer-bridge"))
}

fn usage() -> String {
    "Yukii 受信宿主（Desktop Computer Use / Browser Use）\n\n用法：\n  yukii-desktop-host --prompt \"任务\" [选项]\n\n桌面模式（需要屏幕录制+辅助功能）：\n  --window-id <ID> --pid <PID>  显式选择窗口；省略时交互选择\n  --app <NAME>              按应用名筛选窗口；唯一匹配时自动选择\n  --window-title <TEXT>     按标题子串筛选窗口；与 --app 取交集\n\n浏览器模式（不需要屏幕录制；使用独立 Chrome profile 或已有 CDP）：\n  --browser-url <URL>       启动独立 Chrome profile 并打开 URL\n  --browser-endpoint <URL>  连接已有 CDP 端点，例如 http://127.0.0.1:9222\n  --browser-profile <DIR>   独立 profile，默认 <data-dir>/browser-profile\n  --browser-path <PATH>     指定 Chrome/Chromium 可执行文件\n  --close-browser           任务结束后关闭本次启动的浏览器\n\n通用：\n  --preview-panel <PATH>    桌面实时预览/控制面板，默认 target/native/preview-panel\n  --no-preview              不启动桌面实时预览面板\n  --panel-select            用窗口缩略图选择器选择并授权；本会话不再逐动作审批\n  --window-picker <PATH>    窗口选择器路径，默认 target/native/window-picker\n\n  --data-dir <DIR>          本机任务/浏览器 profile 目录，默认 .data/desktop-host\n  --executor <PATH>         独立执行服务路径，默认与宿主同目录（仅桌面模式）\n  --bridge <PATH>           原生桥路径，默认 target/native/computer-bridge（仅桌面模式）\n  --control-minutes <1-30>  桌面控制会话最长期限，默认 15 分钟\n  --no-confirm-actions      跳过逐动作审批（仍需会话同意/停止边界）\n  --yes                     跳过会话确认\n\n桌面/浏览器默认预算：20 轮、40 次工具、120000 token、900 秒。可用 YUKII_DESKTOP_* 覆盖。\n安全边界：本程序不监听网络，不向网页暴露接口；终端/控制台窗口默认禁止选择。\n浏览器模式只操作独立 profile 或显式 CDP 端点，不读取现有 Chrome profile。\n需要 .env 中 YUKII_VISION_INPUT=true；桌面模式还需要系统屏幕录制/辅助功能权限。"
        .into()
}
