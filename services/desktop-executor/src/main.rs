//! 由受信宿主启动，使用专属 stdin/stdout 管道；不加载 .env、不监听端口。
use std::{
    io::{self, IsTerminal},
    path::Path,
    sync::Arc,
};
use yukii_agent_infra::TaskStore;
use yukii_agent_ports::computer::ComputerRepository;
use yukii_desktop_executor::ComputerExecutor;
use yukii_desktop_executor_service::{DesktopExecutorService, native::ProcessBridge};

fn main() {
    if let Err(error) = run() {
        eprintln!("desktop executor: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let (mut database, mut session, mut bridge) = (None, None, None);
    while let Some(flag) = arguments.next() {
        match flag.as_str() {
            "--database" => database = arguments.next(),
            "--session" => session = arguments.next(),
            "--bridge" => bridge = arguments.next(),
            _ => return Err("未知参数".into()),
        }
    }
    let (Some(database), Some(session)) = (database, session) else {
        return Err(
            "用法：--database <已有数据库绝对路径> --session <宿主会话标识> [--bridge <原生桥绝对路径>]"
                .into(),
        );
    };
    let path = Path::new(&database);
    if !path.is_absolute() || !path.is_file() {
        return Err("需要已有的桌面数据库绝对路径".into());
    }
    if io::stdin().is_terminal() || io::stdout().is_terminal() {
        return Err("执行服务需要受信宿主的专属输入输出管道".into());
    }
    let store = Arc::new(TaskStore::open(path)?);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let computer = match bridge {
        None => None,
        Some(path) => {
            let bridge = Arc::new(ProcessBridge::new(path)?);
            let repository: Arc<dyn ComputerRepository> = store.clone();
            Some(runtime.block_on(ComputerExecutor::new(session.clone(), repository, bridge))?)
        }
    };
    // 子进程不得调用任务启动恢复；父进程可能正在使用同一数据库运行任务。
    let mut service = DesktopExecutorService::new(store, session, computer)?;
    service.serve(&mut io::stdin().lock(), &mut io::stdout().lock(), &runtime)?;
    Ok(())
}
