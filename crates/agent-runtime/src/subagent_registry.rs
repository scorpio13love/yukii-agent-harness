//! 子 Agent 取消登记：run_id 只对应一个运行中的取消令牌。
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use tokio_util::sync::CancellationToken;

fn registry() -> &'static Mutex<HashMap<String, CancellationToken>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, CancellationToken>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register(run_id: &str, token: CancellationToken) {
    if let Ok(mut map) = registry().lock() {
        map.insert(run_id.to_owned(), token);
    }
}

pub fn unregister(run_id: &str) {
    if let Ok(mut map) = registry().lock() {
        map.remove(run_id);
    }
}

/// 返回 false 表示该子 Agent 已结束或不存在。
pub fn cancel(run_id: &str) -> bool {
    match registry().lock() {
        Ok(map) => map
            .get(run_id)
            .map(|token| {
                token.cancel();
                true
            })
            .unwrap_or(false),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancels_only_registered_runs() {
        let token = CancellationToken::new();
        register("run-1", token.clone());
        assert!(cancel("run-1"));
        assert!(token.is_cancelled());
        unregister("run-1");
        assert!(!cancel("run-1"));
    }
}
