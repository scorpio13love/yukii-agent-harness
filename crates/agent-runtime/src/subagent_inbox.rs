//! 子 Agent live inbox：运行中/空闲 child 都保留 sender；send_message 只投递并返回 accepted。
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};
use tokio::sync::mpsc::UnboundedSender;

struct Entry {
    sender: UnboundedSender<String>,
    status: &'static str,
}

static INBOX: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();

fn inbox() -> &'static Mutex<HashMap<String, Entry>> {
    INBOX.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register(agent_id: &str, sender: UnboundedSender<String>) {
    if let Ok(mut map) = inbox().lock() {
        map.insert(
            agent_id.to_owned(),
            Entry {
                sender,
                status: "running",
            },
        );
    }
}

pub fn set_status(agent_id: &str, status: &'static str) {
    if let Ok(mut map) = inbox().lock()
        && let Some(entry) = map.get_mut(agent_id)
    {
        entry.status = status;
    }
}

pub fn status(agent_id: &str) -> Option<&'static str> {
    inbox()
        .lock()
        .ok()
        .and_then(|map| map.get(agent_id).map(|entry| entry.status))
}

pub fn send(agent_id: &str, message: String) -> bool {
    inbox()
        .lock()
        .ok()
        .and_then(|map| {
            map.get(agent_id)
                .map(|entry| entry.sender.send(message).is_ok())
        })
        .unwrap_or(false)
}

pub fn remove(agent_id: &str) {
    if let Ok(mut map) = inbox().lock() {
        map.remove(agent_id);
    }
}

/// 运行中的 child 与 idle loop 共享同一个 receiver；runner 在 step 边界 try_recv。
pub struct ChildInbox {
    receiver: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<String>>,
}

impl ChildInbox {
    pub fn new(receiver: tokio::sync::mpsc::UnboundedReceiver<String>) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            receiver: tokio::sync::Mutex::new(receiver),
        })
    }

    pub fn try_recv(&self) -> Option<String> {
        self.receiver.try_lock().ok()?.try_recv().ok()
    }

    pub async fn recv(&self) -> Option<String> {
        self.receiver.lock().await.recv().await
    }
}
