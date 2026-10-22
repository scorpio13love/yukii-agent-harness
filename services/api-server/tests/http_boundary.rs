use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use std::sync::Arc;
use tower::ServiceExt;
use yukii_api_server::{router, store::TaskStore};

fn create(key: Option<&str>, prompt: &str) -> Request<Body> {
    let mut request = Request::post("/api/tasks")
        .header("host", "localhost:8765")
        .header("content-type", "application/json");
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    request
        .body(Body::from(
            serde_json::json!({"prompt":prompt,"target":"cloud_office"}).to_string(),
        ))
        .unwrap()
}

#[tokio::test]
async fn web_api_rejects_local_execution_without_creating_a_task() {
    let store = Arc::new(TaskStore::open(":memory:").unwrap());
    let response = router(store.clone())
        .oneshot(
            Request::post("/api/tasks")
                .header("host", "localhost:8765")
                .header("content-type", "application/json")
                .header("idempotency-key", "local-attempt")
                .body(Body::from(
                    r#"{"prompt":"读取本机文件","target":"local_computer"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(store.list().unwrap().is_empty());
}
#[tokio::test]
async fn create_requires_key_and_replay_returns_original_task() {
    let store = Arc::new(TaskStore::open(":memory:").unwrap());
    let app = router(store.clone());
    assert_eq!(
        app.clone()
            .oneshot(create(None, "hello"))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        app.clone()
            .oneshot(create(Some("key 1"), "hello"))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    let first = app
        .clone()
        .oneshot(create(Some("key-1"), "hello"))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);
    let first = to_bytes(first.into_body(), 65536).await.unwrap();
    let replay = app
        .clone()
        .oneshot(create(Some("key-1"), "hello"))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(first, to_bytes(replay.into_body(), 65536).await.unwrap());
    let conflict = app
        .oneshot(create(Some("key-1"), "different"))
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(store.list().unwrap().len(), 1);
}
#[tokio::test]
async fn rejects_cross_origin_and_rebinding_and_correlates_responses() {
    let app = router(Arc::new(TaskStore::open(":memory:").unwrap()));
    for (host, origin, expected) in [
        (
            "evil.example",
            "http://localhost:5173",
            StatusCode::FORBIDDEN,
        ),
        (
            "localhost:8765",
            "https://evil.example",
            StatusCode::FORBIDDEN,
        ),
        ("localhost:8765", "null", StatusCode::FORBIDDEN),
        ("localhost:5173", "http://localhost:5173", StatusCode::OK),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::get("/api/health")
                    .header("host", host)
                    .header("origin", origin)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        assert!(
            uuid::Uuid::parse_str(response.headers()["x-request-id"].to_str().unwrap()).is_ok()
        );
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    let response = app
        .oneshot(
            Request::get("/api/ready")
                .header("host", "localhost:8765")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}
