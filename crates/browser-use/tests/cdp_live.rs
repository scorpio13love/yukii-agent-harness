//! 真实 CDP 冒烟测试：连接本机隔离 Chrome 端点，导航 example.com 并读取快照。
//! 默认忽略；需要先启动带 remote-debugging-port 的测试 Chrome。
use std::time::Duration;
use yukii_browser_use::BrowserSession;

#[tokio::test]
#[ignore = "需要本机 Chrome CDP 测试端点"]
async fn real_cdp_navigates_and_observes() {
    let endpoint =
        std::env::var("YUKII_BROWSER_ENDPOINT").unwrap_or_else(|_| "http://127.0.0.1:9222".into());
    let session = tokio::time::timeout(
        Duration::from_secs(10),
        BrowserSession::connect(&endpoint, None),
    )
    .await
    .expect("连接超时")
    .expect("连接 CDP 失败");
    let observation = session
        .navigate("https://example.com")
        .await
        .expect("导航失败");
    assert!(
        observation.title.contains("Example Domain"),
        "{}",
        observation.title
    );
    assert!(observation.url.starts_with("https://example.com"));
    assert!(!observation.text.is_empty());
    assert!(!observation.screenshot_base64.is_empty());
    assert!(observation.width > 0 && observation.height > 0);
    println!(
        "title={} url={} elements={} screenshot={}x{}",
        observation.title,
        observation.url,
        observation.elements.len(),
        observation.width,
        observation.height
    );
}

#[tokio::test]
#[ignore = "需要本机 Chrome CDP 测试端点和本地表单页面"]
async fn real_cdp_types_and_clicks_by_element_id() {
    let endpoint =
        std::env::var("YUKII_BROWSER_ENDPOINT").unwrap_or_else(|_| "http://127.0.0.1:9222".into());
    let page_url =
        std::env::var("YUKII_BROWSER_TEST_URL").unwrap_or_else(|_| "http://127.0.0.1:8766/".into());
    let session = BrowserSession::connect(&endpoint, None)
        .await
        .expect("连接 CDP 失败");
    let page = session.navigate(&page_url).await.expect("导航失败");
    let input = page
        .elements
        .iter()
        .find(|element| element.tag == "input")
        .expect("找不到输入框");
    let button = page
        .elements
        .iter()
        .find(|element| element.tag == "button")
        .expect("找不到提交按钮");
    session
        .type_text(&input.id, "BILIBILI-OK", true)
        .await
        .expect("输入失败");
    session.click(&button.id).await.expect("点击失败");
    let after = session.observe().await.expect("提交后观察失败");
    assert!(
        after.text.contains("submitted:BILIBILI-OK"),
        "页面结果：{}",
        after.text
    );
    println!(
        "form result: {}",
        after
            .text
            .lines()
            .find(|line| line.contains("submitted:"))
            .unwrap_or("")
    );
}
