//! 浏览器动作审批包装：工具目录只看到 BrowserControl，审批在本层执行。
use crate::approval::BrowserApprover;
use async_trait::async_trait;
use std::sync::Arc;
use yukii_agent_ports::browser::{BrowserControl, BrowserObservation};

pub struct ApprovedBrowser {
    inner: Arc<dyn BrowserControl>,
    approver: Arc<dyn BrowserApprover>,
}

impl ApprovedBrowser {
    pub fn new(inner: Arc<dyn BrowserControl>, approver: Arc<dyn BrowserApprover>) -> Self {
        Self { inner, approver }
    }

    async fn require(&self, description: String) -> Result<(), String> {
        if self.approver.approve(&description).await? {
            Ok(())
        } else {
            Err("用户拒绝了本次浏览器动作".into())
        }
    }
}

#[async_trait]
impl BrowserControl for ApprovedBrowser {
    async fn observe(&self) -> Result<BrowserObservation, String> {
        self.inner.observe().await
    }

    async fn navigate(&self, url: &str) -> Result<BrowserObservation, String> {
        self.require(format!("导航到 {url}")).await?;
        self.inner.navigate(url).await
    }

    async fn click(&self, element_id: &str) -> Result<(), String> {
        self.require(format!("点击页面元素 {element_id}")).await?;
        self.inner.click(element_id).await
    }

    async fn type_text(&self, element_id: &str, text: &str, clear: bool) -> Result<(), String> {
        let preview: String = text.chars().take(120).collect();
        self.require(format!(
            "在页面元素 {element_id} {}输入“{preview}”",
            if clear { "替换" } else { "追加" }
        ))
        .await?;
        self.inner.type_text(element_id, text, clear).await
    }

    async fn press_key(&self, key: &str) -> Result<(), String> {
        self.require(format!("按下浏览器按键 {key}")).await?;
        self.inner.press_key(key).await
    }

    async fn scroll(&self, delta_y: i32) -> Result<(), String> {
        self.require(format!("滚动页面 {delta_y} 像素")).await?;
        self.inner.scroll(delta_y).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use yukii_agent_ports::browser::BrowserElement;

    #[derive(Default)]
    struct FakeBrowser {
        calls: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl BrowserControl for FakeBrowser {
        async fn observe(&self) -> Result<BrowserObservation, String> {
            self.calls.lock().unwrap().push("observe".into());
            Ok(BrowserObservation {
                url: "https://example.com/".into(),
                title: "Example".into(),
                text: "Example".into(),
                elements: Vec::<BrowserElement>::new(),
                screenshot_base64: "png".into(),
                width: 1,
                height: 1,
            })
        }
        async fn navigate(&self, url: &str) -> Result<BrowserObservation, String> {
            self.calls.lock().unwrap().push(format!("navigate:{url}"));
            self.observe().await
        }
        async fn click(&self, element_id: &str) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("click:{element_id}"));
            Ok(())
        }
        async fn type_text(&self, element_id: &str, text: &str, clear: bool) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("type:{element_id}:{text}:{clear}"));
            Ok(())
        }
        async fn press_key(&self, key: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("key:{key}"));
            Ok(())
        }
        async fn scroll(&self, delta_y: i32) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("scroll:{delta_y}"));
            Ok(())
        }
    }

    struct Approver {
        allow: AtomicBool,
        prompts: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl BrowserApprover for Approver {
        async fn approve(&self, description: &str) -> Result<bool, String> {
            self.prompts.lock().unwrap().push(description.into());
            Ok(self.allow.load(Ordering::SeqCst))
        }
    }

    #[tokio::test]
    async fn observe_is_not_approved_but_mutations_are_and_denial_blocks() {
        let browser = Arc::new(FakeBrowser::default());
        let approver = Arc::new(Approver {
            allow: AtomicBool::new(true),
            prompts: Mutex::new(Vec::new()),
        });
        let approved = ApprovedBrowser::new(browser.clone(), approver.clone());
        approved.observe().await.unwrap();
        approved.navigate("https://example.com").await.unwrap();
        approved.click("el-1").await.unwrap();
        approved.type_text("el-2", "hello", true).await.unwrap();
        approved.press_key("enter").await.unwrap();
        approved.scroll(-200).await.unwrap();
        assert_eq!(approver.prompts.lock().unwrap().len(), 5);
        assert!(
            browser
                .calls
                .lock()
                .unwrap()
                .contains(&"navigate:https://example.com".to_string())
        );

        approver.allow.store(false, Ordering::SeqCst);
        let denied = ApprovedBrowser::new(browser.clone(), approver.clone());
        assert!(denied.click("el-3").await.is_err());
        assert!(
            !browser
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| call == "click:el-3")
        );
    }
}
