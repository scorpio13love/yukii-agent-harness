//! 动作审批只发生在受信宿主进程内。模型工具调用不能自行跳过审批。
use async_trait::async_trait;
use std::io::Write;
use tokio::sync::Mutex;
use yukii_contracts::desktop::GuiAction;

#[async_trait]
pub trait ActionApprover: Send + Sync {
    /// 返回 false 表示用户拒绝；错误表示无法取得决定，二者都必须阻止派发。
    async fn approve(&self, observation_id: &str, action: &GuiAction) -> Result<bool, String>;
}

/// 终端交互审批。动作派发前原生桥会重新激活目标应用，避免审批焦点导致误操作。
pub struct TerminalApprover {
    input: Mutex<()>,
}

impl Default for TerminalApprover {
    fn default() -> Self {
        Self {
            input: Mutex::new(()),
        }
    }
}

impl TerminalApprover {
    async fn prompt(&self, description: &str) -> Result<bool, String> {
        let _guard = self.input.lock().await;
        eprint!("\nAgent 请求执行：{description}\n输入 y 批准，其他任何输入拒绝：");
        std::io::stderr().flush().ok();
        let line = tokio::task::spawn_blocking(|| {
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).map(|_| line)
        })
        .await
        .map_err(|_| "无法读取审批输入".to_string())?
        .map_err(|_| "无法读取审批输入".to_string())?;
        Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES"))
    }
}

#[async_trait]
impl ActionApprover for TerminalApprover {
    async fn approve(&self, observation_id: &str, action: &GuiAction) -> Result<bool, String> {
        self.prompt(&format!(
            "{}（观察 {}）",
            describe(action),
            observation_id.chars().take(12).collect::<String>()
        ))
        .await
    }
}

/// 浏览器动作审批。页面内容不进入审批文本，避免把不可信内容带进决策。
#[async_trait]
pub trait BrowserApprover: Send + Sync {
    async fn approve(&self, description: &str) -> Result<bool, String>;
}

#[async_trait]
impl BrowserApprover for TerminalApprover {
    async fn approve(&self, description: &str) -> Result<bool, String> {
        self.prompt(description).await
    }
}

/// 只用于用户明确加 `--no-confirm-actions` 的会话级自主模式；仍保留会话同意和停止边界。
pub struct AllowAllActions;

#[async_trait]
impl ActionApprover for AllowAllActions {
    async fn approve(&self, _: &str, _: &GuiAction) -> Result<bool, String> {
        Ok(true)
    }
}

#[async_trait]
impl BrowserApprover for AllowAllActions {
    async fn approve(&self, _: &str) -> Result<bool, String> {
        Ok(true)
    }
}

pub fn describe(action: &GuiAction) -> String {
    match action {
        GuiAction::Click { x, y } => format!("点击窗口坐标 ({x:.0}, {y:.0})"),
        GuiAction::Scroll { x, y, delta_y } => {
            format!("在 ({x:.0}, {y:.0}) 滚动 {delta_y:.0}")
        }
        GuiAction::TypeText { text } => {
            let preview: String = text.chars().take(120).collect();
            format!("输入文本“{preview}”")
        }
        GuiAction::ReplaceText { text } => {
            let preview: String = text.chars().take(120).collect();
            format!("替换输入框文本为“{preview}”")
        }
        GuiAction::PressKey { key } => format!("按下按键 {key}"),
        GuiAction::ClickElement { element_id } => format!("点击可访问性元素 {element_id}"),
        GuiAction::TypeElement {
            element_id,
            text,
            clear,
        } => {
            let preview: String = text.chars().take(120).collect();
            format!(
                "在元素 {element_id} {}输入“{preview}”",
                if *clear { "替换" } else { "追加" }
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_descriptions_do_not_include_hidden_reasoning() {
        assert!(describe(&GuiAction::Click { x: 1.0, y: 2.0 }).contains("点击"));
        assert!(
            describe(&GuiAction::PressKey {
                key: "enter".into()
            })
            .contains("enter")
        );
        assert!(
            describe(&GuiAction::TypeText {
                text: "x".repeat(200)
            })
            .chars()
            .count()
                < 150
        );
    }
}
