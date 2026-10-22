use axum::{Router, http::StatusCode, response::Redirect, routing::get};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::PathBuf, sync::Arc};
use yukii_agent_extensions::marketplace::Marketplace;
use yukii_agent_ports::extensions::PluginRepository;
use yukii_api_server::store::TaskStore;

const PACKAGE: &str =
    include_str!("../../../examples/marketplace/packages/office_writing-1.0.0.json");

/// 使用真实 HTTP 往返覆盖市场包被替换与重定向，防止仅校验本地示例。
#[tokio::test]
async fn remote_market_checks_digest_identity_and_rejects_redirects() {
    let digest = format!("{:x}", Sha256::digest(PACKAGE));
    let index = |sha256: String, id: &str| {
        json!({
        "schema_version":1,"plugins":[{"id":id,"version":"1.0.0","name":"test", "description":"test",
        "package":"package.json","sha256":sha256}]
    }).to_string()
    };
    let valid = index(digest.clone(), "office_writing");
    let tampered = index("0".repeat(64), "office_writing");
    let mismatch = index(digest, "another_plugin");
    let app = Router::new()
        .route("/index.json", get(move || async { valid }))
        .route("/tampered.json", get(move || async { tampered }))
        .route("/mismatch.json", get(move || async { mismatch }))
        .route(
            "/redirect.json",
            get(|| async { Redirect::temporary("/index.json") }),
        )
        .route("/package.json", get(|| async { (StatusCode::OK, PACKAGE) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let market =
        |file| Marketplace::new(PathBuf::new(), Some(&format!("http://{address}/{file}"))).unwrap();
    assert_eq!(
        market("index.json")
            .preview("office_writing", "1.0.0")
            .await
            .unwrap()
            .manifest
            .id,
        "office_writing"
    );
    assert!(
        market("tampered.json")
            .preview("office_writing", "1.0.0")
            .await
            .unwrap_err()
            .contains("SHA-256")
    );
    assert!(
        market("mismatch.json")
            .preview("another_plugin", "1.0.0")
            .await
            .unwrap_err()
            .contains("身份")
    );
    assert!(market("redirect.json").catalog().await.is_err());
    server.abort();
}

#[test]
fn plugin_upgrade_requires_current_revision_and_never_inherits_approval() {
    let store = Arc::new(TaskStore::open(":memory:").unwrap());
    let mut manifest = serde_json::from_str(PACKAGE).unwrap();
    let first = store
        .install_plugin(&manifest, &"a".repeat(64), "test", None)
        .unwrap();
    store
        .set_plugin_enabled("office_writing", &first.sha256, true)
        .unwrap();
    let mut value: Value = serde_json::to_value(&manifest).unwrap();
    value["version"] = json!("1.0.1");
    manifest = serde_json::from_value(value).unwrap();
    assert!(
        store
            .install_plugin(&manifest, &"b".repeat(64), "test", None)
            .is_err()
    );
    assert!(
        store
            .install_plugin(&manifest, &"b".repeat(64), "test", Some(&"0".repeat(64)))
            .is_err()
    );
    let updated = store
        .install_plugin(&manifest, &"b".repeat(64), "test", Some(&first.sha256))
        .unwrap();
    assert!(!updated.enabled);
    assert_eq!(updated.manifest.version, "1.0.1");
    assert!(
        store
            .set_plugin_enabled("office_writing", &first.sha256, true)
            .is_err()
    );
}
