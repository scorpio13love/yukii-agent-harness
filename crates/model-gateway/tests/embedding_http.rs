//! 本机真实 HTTP 测试：不读取 .env，不调用付费向量模型。
use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
use serde_json::{Value, json};
use yukii_agent_ports::embedding::{EmbeddingError, EmbeddingModel, EmbeddingPurpose};
use yukii_model_gateway::embedding::{EmbeddingConfig, EmbeddingFormat, HttpEmbedding};

#[derive(Clone)]
struct Mock {
    native: bool,
    response: Value,
}
async fn respond(
    State(state): State<Mock>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    assert_eq!(headers["authorization"], "Bearer test-only");
    assert_eq!(body["model"], "test-vector");
    if state.native {
        assert_eq!(body["input"]["texts"], json!(["甲", "乙"]));
        assert_eq!(body["parameters"]["dimension"], 2);
        assert_eq!(body["parameters"]["text_type"], "query");
    } else {
        assert_eq!(body["input"], json!(["甲", "乙"]));
        assert_eq!(body["dimensions"], 2);
        assert_eq!(body["encoding_format"], "float");
    }
    Json(state.response)
}
async fn call(format: EmbeddingFormat, response: Value) -> Result<Vec<Vec<f32>>, EmbeddingError> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/embeddings", post(respond))
        .with_state(Mock {
            native: matches!(format, EmbeddingFormat::DashScope),
            response,
        });
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let model = HttpEmbedding::new(EmbeddingConfig {
        format,
        endpoint: format!("http://{address}/embeddings").parse().unwrap(),
        api_key: "test-only".into(),
        model: "test-vector".into(),
        dimensions: 2,
        timeout_seconds: 2,
    })
    .unwrap();
    let result = model
        .embed(&["甲".into(), "乙".into()], EmbeddingPurpose::Query)
        .await;
    server.abort();
    result
}
#[tokio::test]
async fn both_formats_reorder_and_normalize_vectors() {
    let openai = call(
        EmbeddingFormat::OpenAi,
        json!({"data":[{"index":1,"embedding":[0,3]},{"index":0,"embedding":[2,0]}]}),
    )
    .await
    .unwrap();
    let native=call(EmbeddingFormat::DashScope,json!({"output":{"embeddings":[{"text_index":1,"embedding":[0,3]},{"text_index":0,"embedding":[2,0]}]}})).await.unwrap();
    assert_eq!(openai, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
    assert_eq!(native, openai);
}
#[tokio::test]
async fn rejects_missing_duplicate_wrong_dimension_and_zero_vectors() {
    for response in [
        json!({"data":[{"index":0,"embedding":[1,0]}]}),
        json!({"data":[{"index":0,"embedding":[1,0]},{"index":0,"embedding":[0,1]}]}),
        json!({"data":[{"index":0,"embedding":[1,0]},{"index":1,"embedding":[1,0,0]}]}),
        json!({"data":[{"index":0,"embedding":[1,0]},{"index":1,"embedding":[0,0]}]}),
    ] {
        assert!(matches!(
            call(EmbeddingFormat::OpenAi, response).await,
            Err(EmbeddingError::Protocol)
        ));
    }
}
