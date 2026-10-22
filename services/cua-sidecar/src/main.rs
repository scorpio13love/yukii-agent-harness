//! CUA sidecar：Electron/其他受信宿主与受控执行器之间的窄 stdio 协议。
//! 只暴露 CUA 原语（选择窗口、观察、元素/坐标动作、暂停/停止），不运行模型、不监听网络。
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    process::{ExitCode, Stdio},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Stdout},
    process::{Child, Command},
    sync::Mutex,
    task::JoinHandle,
};
use yukii_agent_infra::TaskStore;
use yukii_contracts::{
    CreateTask, ExecutionTarget, TaskStatus,
    desktop::{DesktopWindow, GuiAction},
};
use yukii_desktop_host::{
    approval::AllowAllActions,
    ipc::IpcControl,
    preview::{PickedWindow, WindowPicker},
};

const PROTOCOL_VERSION: u32 = 1;
const ALLOWED_APPS_FILE: &str = "allowed_apps.json";

#[derive(Debug, Default, Serialize, Deserialize)]
struct AllowedApps {
    allowed: HashMap<String, String>,
}

struct Options {
    executor: PathBuf,
    bridge: PathBuf,
    data_dir: PathBuf,
    picker: PathBuf,
    preview_stream: PathBuf,
    session_minutes: i64,
}

struct PreviewProcess {
    child: Child,
    reader: JoinHandle<()>,
}

struct Sidecar {
    ipc: Arc<IpcControl>,
    store: Arc<TaskStore>,
    bridge: PathBuf,
    picker: PathBuf,
    preview_stream: PathBuf,
    out: Arc<Mutex<Stdout>>,
    preview: Option<PreviewProcess>,
    allowed_path: PathBuf,
    allowed: HashMap<String, String>,
    session_minutes: i64,
    task_id: Option<String>,
    window: Option<DesktopWindow>,
    preview_window: Option<DesktopWindow>,
}

struct Request {
    id: String,
    command: String,
    payload: Value,
}

impl Sidecar {
    async fn handle(&mut self, request: Request) -> (Value, bool) {
        let result = match request.command.as_str() {
            "hello" => Ok(json!({
                "protocol": PROTOCOL_VERSION,
                "platform": "macos",
                "capabilities": ["permissions", "windows", "apps", "allowlist", "picker", "control", "elements", "preview", "preview_stream", "coordinate_fallback"]
            })),
            "permissions" => self
                .ipc
                .permissions()
                .await
                .map(|value| serde_json::to_value(value).unwrap_or(Value::Null))
                .map_err(|error| error.to_string()),
            "windows" => self
                .ipc
                .windows()
                .await
                .map(|value| serde_json::to_value(value).unwrap_or(Value::Null))
                .map_err(|error| error.to_string()),
            "apps" => self.apps().await,
            "allowed_apps" => Ok(json!({"allowed": self.allowed})),
            "allow_app" => self.allow_app(&request.payload).await,
            "revoke_app" => self.revoke_app(&request.payload).await,
            "begin_app" => self.begin_app(&request.payload).await,
            "pick" => match WindowPicker::pick(&self.picker).await {
                Ok(PickedWindow { window_id, pid }) => {
                    Ok(json!({"window_id": window_id, "pid": pid}))
                }
                Err(error) => Err(error),
            },
            "begin" => self.begin(&request.payload).await,
            "observe" => self.observe().await,
            "preview" => self.preview().await,
            "preview_start" => self.start_preview(&request.payload).await,
            "preview_stop" => self.stop_preview().await,
            "act" => self.act(&request.payload).await,
            "pause" => self.simple_control("pause").await,
            "resume" => self.simple_control("resume").await,
            "end" => self.end().await,
            "status" => Ok(json!({
                "task_id": self.task_id,
                "window": self.window.as_ref().map(|window| json!({
                    "window_id": window.window_id,
                    "pid": window.pid,
                    "application": window.application,
                    "title": window.title,
                })),
            })),
            "shutdown" => {
                let _ = self.end().await;
                return (
                    json!({"id": request.id, "ok": true, "result": {"stopped": true}}),
                    true,
                );
            }
            _ => Err("unknown_command".to_string()),
        };
        match result {
            Ok(value) => (
                json!({"id": request.id, "ok": true, "result": value}),
                false,
            ),
            Err(error) => (
                json!({"id": request.id, "ok": false, "error": error}),
                false,
            ),
        }
    }

    async fn begin(&mut self, payload: &Value) -> Result<Value, String> {
        if self.task_id.is_some() {
            return Err("control_already_active".into());
        }
        let window = DesktopWindow {
            window_id: payload["window_id"]
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or("invalid_window_id")?,
            pid: payload["pid"]
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or("invalid_pid")?,
            application: payload["application"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(256)
                .collect(),
            title: payload["title"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(512)
                .collect(),
        };
        self.start_control(window).await
    }

    async fn start_control(&mut self, window: DesktopWindow) -> Result<Value, String> {
        self.stop_preview().await?;
        let created = self
            .store
            .create_local_once(
                CreateTask {
                    conversation_id: None,
                    prompt: "CUA sidecar session".into(),
                    target: ExecutionTarget::LocalComputer,
                },
                &uuid::Uuid::new_v4().to_string(),
                false,
            )
            .map_err(|error| format!("无法创建 CUA 会话任务：{error}"))?;
        let task_id = created.task.id.clone();
        let expires_at = now() + self.session_minutes * 60;
        let control = match self
            .ipc
            .begin_control(&task_id, &window, true, expires_at)
            .await
        {
            Ok(control) => control,
            Err(error) => {
                let _ = self.store.cancel(&task_id);
                return Err(error);
            }
        };
        if let Err(error) = self
            .store
            .advance(
                &task_id,
                TaskStatus::Planning,
                "CUA sidecar 会话准备。",
                None,
                None,
            )
            .and_then(|_| {
                self.store.advance(
                    &task_id,
                    TaskStatus::Running,
                    "CUA sidecar 会话开始。",
                    None,
                    None,
                )
            })
        {
            let _ = self.ipc.end_control().await;
            let _ = self.store.cancel(&task_id);
            return Err(format!("无法启动 CUA 会话任务：{error}"));
        }
        self.task_id = Some(task_id.clone());
        self.window = Some(window.clone());
        Ok(json!({
            "task_id": task_id,
            "window": window,
            "expires_at": control.expires_at,
        }))
    }

    async fn bridge_call(&self, request: Value) -> Result<Value, String> {
        let mut child = Command::new(&self.bridge)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("无法启动原生桥：{error}"))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "原生桥输入不可用".to_string())?;
        stdin
            .write_all(
                serde_json::to_string(&request)
                    .map_err(|_| "原生桥请求编码失败".to_string())?
                    .as_bytes(),
            )
            .await
            .map_err(|_| "原生桥写入失败".to_string())?;
        drop(stdin);
        let output = child
            .wait_with_output()
            .await
            .map_err(|error| format!("原生桥等待失败：{error}"))?;
        let value: Value =
            serde_json::from_slice(&output.stdout).map_err(|_| "原生桥返回格式无效".to_string())?;
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            return Err(error.to_string());
        }
        Ok(value)
    }

    async fn apps(&self) -> Result<Value, String> {
        let value = self.bridge_call(json!({"operation":"apps"})).await?;
        Ok(json!({
            "apps": value.get("apps").cloned().unwrap_or_else(|| json!([])),
            "allowed": self.allowed,
        }))
    }

    async fn allow_app(&mut self, payload: &Value) -> Result<Value, String> {
        let app_id = payload["app_id"]
            .as_str()
            .filter(|value| !value.is_empty() && value.len() <= 256)
            .ok_or("invalid_app_id")?;
        let name = payload["application"]
            .as_str()
            .unwrap_or(app_id)
            .chars()
            .take(256)
            .collect::<String>();
        self.allowed.insert(app_id.to_string(), name);
        self.save_allowed()?;
        Ok(json!({"allowed": true, "app_id": app_id}))
    }

    async fn revoke_app(&mut self, payload: &Value) -> Result<Value, String> {
        let app_id = payload["app_id"].as_str().ok_or("invalid_app_id")?;
        self.allowed.remove(app_id);
        self.save_allowed()?;
        Ok(json!({"revoked": true, "app_id": app_id}))
    }

    async fn begin_app(&mut self, payload: &Value) -> Result<Value, String> {
        if self.task_id.is_some() {
            return Err("control_already_active".into());
        }
        let app_id = payload["app_id"].as_str().ok_or("invalid_app_id")?;
        // 单次审批由上层 MCP 审批流传入；只对本次 begin_app 生效，不写入 allowlist。
        let approved_once = payload["approved_once"].as_bool().unwrap_or(false);
        if !approved_once && !self.allowed.contains_key(app_id) {
            return Err("app_not_allowed".into());
        }
        let value = self.bridge_call(json!({"operation":"apps"})).await?;
        let apps = value
            .get("apps")
            .and_then(Value::as_array)
            .ok_or("app_list_unavailable")?;
        let app = apps
            .iter()
            .find(|app| app.get("app_id").and_then(Value::as_str) == Some(app_id))
            .ok_or("app_window_unavailable")?;
        let window = DesktopWindow {
            window_id: app
                .get("window_id")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or("invalid_window_id")?,
            pid: app
                .get("pid")
                .and_then(Value::as_i64)
                .and_then(|value| i32::try_from(value).ok())
                .ok_or("invalid_pid")?,
            application: app
                .get("application")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .chars()
                .take(256)
                .collect(),
            title: app
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .chars()
                .take(512)
                .collect(),
        };
        self.start_control(window).await
    }

    fn save_allowed(&self) -> Result<(), String> {
        let payload = serde_json::to_vec_pretty(&AllowedApps {
            allowed: self.allowed.clone(),
        })
        .map_err(|_| "allowed_apps_encoding_failed".to_string())?;
        let temp = self.allowed_path.with_extension("json.tmp");
        fs::write(&temp, payload).map_err(|_| "allowed_apps_write_failed".to_string())?;
        fs::rename(&temp, &self.allowed_path).map_err(|_| "allowed_apps_rename_failed".to_string())
    }

    async fn start_preview(&mut self, payload: &Value) -> Result<Value, String> {
        // 产品侧栏可以只预览一个由本地任务控制的窗口；不创建 sidecar 控制会话。
        let explicit = payload["window_id"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .and_then(|window_id| {
                let pid = payload["pid"]
                    .as_i64()
                    .and_then(|value| i32::try_from(value).ok())?;
                Some(DesktopWindow {
                    window_id,
                    pid,
                    application: payload["application"]
                        .as_str()
                        .unwrap_or_default()
                        .chars()
                        .take(256)
                        .collect(),
                    title: payload["title"]
                        .as_str()
                        .unwrap_or_default()
                        .chars()
                        .take(512)
                        .collect(),
                })
            });
        let window = explicit
            .or_else(|| self.window.clone())
            .or_else(|| self.preview_window.clone())
            .ok_or("control_not_started")?;
        self.preview_window = Some(window.clone());
        self.stop_preview().await?;
        if !self.preview_stream.is_file() {
            return Err("preview_stream_missing".into());
        }
        let mut child = Command::new(&self.preview_stream)
            .arg("--window-id")
            .arg(window.window_id.to_string())
            .arg("--pid")
            .arg(window.pid.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| "preview_stream_spawn_failed".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "preview_stream_stdout_missing".to_string())?;
        let out = self.out.clone();
        let reader = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let event = match value["event"].as_str() {
                    Some("ready") => json!({
                        "event": "preview_ready",
                        "width": value["width"],
                        "height": value["height"],
                    }),
                    Some("frame") => json!({
                        "event": "preview",
                        "data": value["data"],
                        "width": value["width"],
                        "height": value["height"],
                    }),
                    Some("error") => json!({
                        "event": "preview_error",
                        "message": value["message"],
                    }),
                    _ => continue,
                };
                let _ = write_json(&out, &event).await;
            }
            let _ = write_json(&out, &json!({"event":"preview_stopped"})).await;
        });
        self.preview = Some(PreviewProcess { child, reader });
        Ok(json!({"streaming": true}))
    }

    async fn stop_preview(&mut self) -> Result<Value, String> {
        if let Some(mut preview) = self.preview.take() {
            let _ = preview.child.kill().await;
            preview.reader.abort();
            let _ = preview.reader.await;
        }
        Ok(json!({"streaming": false}))
    }

    async fn preview(&self) -> Result<Value, String> {
        let window = self
            .window
            .as_ref()
            .or(self.preview_window.as_ref())
            .ok_or("control_not_started")?;
        self.bridge_call(json!({
            "operation": "preview",
            "window_id": window.window_id,
            "pid": window.pid,
        }))
        .await
    }

    async fn observe(&mut self) -> Result<Value, String> {
        let task_id = self.task_id.as_deref().ok_or("control_not_started")?;
        let observation = self.ipc.observe(task_id).await?;
        serde_json::to_value(observation).map_err(|_| "observation_encoding_failed".into())
    }

    async fn act(&mut self, payload: &Value) -> Result<Value, String> {
        let task_id = self.task_id.as_deref().ok_or("control_not_started")?;
        let observation_id = payload["observation_id"]
            .as_str()
            .filter(|value| !value.is_empty() && value.len() <= 128)
            .ok_or("invalid_observation_id")?;
        let action: GuiAction = serde_json::from_value(payload["action"].clone())
            .map_err(|_| "invalid_action".to_string())?;
        let call_id = uuid::Uuid::new_v4().to_string();
        let dispatch = self
            .ipc
            .act_dispatch(task_id, &call_id, observation_id, &action)
            .await?;
        serde_json::to_value(dispatch).map_err(|_| "dispatch_encoding_failed".into())
    }

    async fn simple_control(&mut self, operation: &str) -> Result<Value, String> {
        let task_id = self.task_id.as_deref().ok_or("control_not_started")?;
        match operation {
            "pause" => self.ipc.pause_control(task_id).await?,
            "resume" => self.ipc.resume_control(task_id).await?,
            _ => return Err("unknown_control".into()),
        }
        Ok(json!({"ok": true}))
    }

    async fn end(&mut self) -> Result<Value, String> {
        if self.task_id.is_none() {
            return Ok(json!({"ended": false}));
        }
        let task_id = self.task_id.clone();
        self.stop_preview().await?;
        self.ipc.end_control().await?;
        if let Some(task_id) = task_id {
            let _ = self.store.cancel(&task_id);
        }
        self.task_id = None;
        self.window = None;
        Ok(json!({"ended": true}))
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let options = match parse_options() {
        Ok(options) => options,
        Err(error) => {
            eprintln!("cua-sidecar: {error}");
            return ExitCode::from(2);
        }
    };
    match run(options).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cua-sidecar: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(options: Options) -> Result<(), String> {
    std::fs::create_dir_all(&options.data_dir)
        .map_err(|error| format!("无法创建数据目录：{error}"))?;
    let database = options.data_dir.join("cua-sidecar.sqlite");
    // 先让 TaskStore 完成 SQLite 迁移；Executor 直接打开同一数据库时不能是空文件。
    let store = Arc::new(
        yukii_agent_infra::TaskStore::open(&database)
            .map_err(|error| format!("无法初始化审计数据库：{error}"))?,
    );
    let session_id = uuid::Uuid::new_v4().to_string();
    let ipc = Arc::new(IpcControl::spawn(
        &options.executor,
        &database,
        &session_id,
        &options.bridge,
        Arc::new(AllowAllActions),
    )?);
    ipc.hello().await?;
    let allowed_path = options.data_dir.join(ALLOWED_APPS_FILE);
    let allowed = if allowed_path.is_file() {
        fs::read(&allowed_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<AllowedApps>(&bytes).ok())
            .map(|value| value.allowed)
            .unwrap_or_default()
    } else {
        HashMap::new()
    };
    let output = Arc::new(Mutex::new(tokio::io::stdout()));
    let mut sidecar = Sidecar {
        ipc,
        store: store.clone(),
        bridge: options.bridge,
        picker: options.picker,
        preview_stream: options.preview_stream,
        out: output.clone(),
        preview: None,
        allowed_path,
        allowed,
        session_minutes: options.session_minutes,
        task_id: None,
        window: None,
        preview_window: None,
    };

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let parsed = match serde_json::from_str::<Value>(&line) {
            Ok(value) => value,
            Err(_) => {
                let response = json!({"id":"", "ok":false, "error":"invalid_json"});
                write_json(&output, &response).await?;
                continue;
            }
        };
        let request = Request {
            id: parsed["id"].as_str().unwrap_or_default().to_string(),
            command: parsed["command"].as_str().unwrap_or_default().to_string(),
            payload: parsed,
        };
        let (response, shutdown) = sidecar.handle(request).await;
        write_json(&output, &response).await?;
        if shutdown {
            break;
        }
    }
    let _ = sidecar.ipc.end_control().await;
    sidecar.ipc.shutdown().await;
    drop(store);
    Ok(())
}

async fn write_json(output: &Arc<Mutex<Stdout>>, value: &Value) -> Result<(), String> {
    let mut line =
        serde_json::to_string(value).map_err(|_| "response_encoding_failed".to_string())?;
    line.push('\n');
    let mut output = output.lock().await;
    output
        .write_all(line.as_bytes())
        .await
        .map_err(|_| "stdout_write_failed".to_string())?;
    output
        .flush()
        .await
        .map_err(|_| "stdout_flush_failed".to_string())
}

fn parse_options() -> Result<Options, String> {
    let mut executor = default_executor();
    let mut bridge = default_bridge();
    let mut data_dir = PathBuf::from(".data/cua-sidecar");
    let mut picker = default_picker();
    let mut preview_stream = default_preview_stream();
    let mut session_minutes = 15i64;
    let mut arguments = std::env::args().skip(1);
    while let Some(flag) = arguments.next() {
        match flag.as_str() {
            "--executor" => executor = PathBuf::from(arguments.next().ok_or("--executor 缺少值")?),
            "--bridge" => bridge = PathBuf::from(arguments.next().ok_or("--bridge 缺少值")?),
            "--data-dir" => data_dir = PathBuf::from(arguments.next().ok_or("--data-dir 缺少值")?),
            "--window-picker" => {
                picker = PathBuf::from(arguments.next().ok_or("--window-picker 缺少值")?)
            }
            "--preview-stream" => {
                preview_stream = PathBuf::from(arguments.next().ok_or("--preview-stream 缺少值")?)
            }
            "--session-minutes" => {
                session_minutes = arguments
                    .next()
                    .ok_or("--session-minutes 缺少值")?
                    .parse()
                    .map_err(|_| "--session-minutes 必须是整数")?;
                if !(1..=60).contains(&session_minutes) {
                    return Err("--session-minutes 必须在 1 到 60 之间".into());
                }
            }
            "--help" | "-h" => {
                println!(
                    "yukii-cua-sidecar：私有 CUA stdio sidecar\n\n--executor <PATH> --bridge <PATH> --data-dir <DIR> --window-picker <PATH> --session-minutes <1-60>"
                );
                std::process::exit(0);
            }
            other => return Err(format!("未知参数：{other}")),
        }
    }
    let executor = absolute_file(executor, "Executor")?;
    let bridge = absolute_file(bridge, "原生桥")?;
    let picker = absolute_file(picker, "窗口选择器")?;
    let preview_stream = absolute_file(preview_stream, "预览流")?;
    let data_dir = if data_dir.is_absolute() {
        data_dir
    } else {
        std::env::current_dir()
            .map_err(|error| format!("无法取得当前目录：{error}"))?
            .join(data_dir)
    };
    Ok(Options {
        executor,
        bridge,
        data_dir,
        picker,
        preview_stream,
        session_minutes,
    })
}

fn absolute_file(path: PathBuf, label: &str) -> Result<PathBuf, String> {
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|error| format!("无法取得当前目录：{error}"))?
            .join(path)
    };
    if !path.is_file() {
        return Err(format!("找不到{label}：{}", path.display()));
    }
    path.canonicalize()
        .map_err(|error| format!("无法解析{label}路径：{error}"))
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

fn default_preview_stream() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.parent()
                .and_then(|debug| debug.parent())
                .map(|target| target.join("native/preview-stream"))
        })
        .unwrap_or_else(|| PathBuf::from("target/native/preview-stream"))
}

fn default_picker() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.parent()
                .and_then(|debug| debug.parent())
                .map(|target| target.join("native/window-picker"))
        })
        .unwrap_or_else(|| PathBuf::from("target/native/window-picker"))
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}
