//! 使用本机模拟 HTTP 服务验证真实协议往返，不读取 .env、不发送真实密钥。
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    routing::post,
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tower::ServiceExt;
use yukii_api_server::{AppState, configured_router, executor::Executor, store::TaskStore};
use yukii_contracts::{CreateTask, ExecutionTarget, Task, TaskStatus};
use yukii_model_gateway::{
    HttpModel,
    config::{ApiFormat, ModelConfig},
};

#[derive(Clone)]
struct MockState {
    format: ApiFormat,
    requests: Arc<Mutex<Vec<Value>>>,
}
async fn mock_model(
    State(state): State<MockState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    assert_eq!(body["model"], "test-model");
    match state.format {
        ApiFormat::OpenAi => assert_eq!(body["stream"], true),
        ApiFormat::Anthropic => assert_eq!(body["stream"], false),
    }
    assert_eq!(body["tools"].as_array().unwrap().len(), 9);
    match state.format {
        ApiFormat::OpenAi => {
            assert_eq!(headers["authorization"], "Bearer test-only-key");
            assert_eq!(body["max_completion_tokens"], 1024);
        }
        ApiFormat::Anthropic => {
            assert_eq!(headers["x-api-key"], "test-only-key");
            assert_eq!(headers["anthropic-version"], "2023-06-01");
            assert!(body["system"].is_string());
        }
    }
    let mut requests = state.requests.lock().unwrap();
    let turn = requests.len();
    requests.push(body.clone());
    if turn == 1 {
        let messages = body["messages"].as_array().unwrap();
        match state.format {
            ApiFormat::OpenAi => {
                let last = messages.last().unwrap();
                assert_eq!(last["role"], "tool");
                assert_eq!(last["tool_call_id"], "calc-1");
                assert!(last["content"].as_str().unwrap().contains("6.0"));
            }
            ApiFormat::Anthropic => {
                let last = messages.last().unwrap();
                assert_eq!(last["content"][0]["type"], "tool_result");
                assert_eq!(last["content"][0]["tool_use_id"], "calc-1");
            }
        }
    }
    let call = match turn {
        0 => Some((
            "calc-1",
            "calculate",
            json!({"operation":"sum","values":[1,2,3]}),
        )),
        1 | 2 => Some((
            "doc-1",
            "create_markdown",
            json!({"title":"测试报告","content":"# 结果\n\n合计为 6。"}),
        )),
        _ => None,
    };
    Json(match state.format {
        ApiFormat::OpenAi => {
            let mut message = json!({"role":"assistant","content":"报告已生成，合计为 6。"});
            if let Some((id, name, args)) = &call {
                message = json!({"role":"assistant","content":null,"tool_calls":[{"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]});
            }
            json!({"choices":[{"message":message,"finish_reason":if call.is_some(){"tool_calls"}else{"stop"}}],"usage":{"prompt_tokens":10,"completion_tokens":5}})
        }
        ApiFormat::Anthropic => {
            let content = if let Some((id, name, args)) = &call {
                json!([{"type":"tool_use","id":id,"name":name,"input":args}])
            } else {
                json!([{"type":"text","text":"报告已生成，合计为 6。"}])
            };
            json!({"content":content,"stop_reason":if call.is_some(){"tool_use"}else{"end_turn"},"usage":{"input_tokens":10,"output_tokens":5}})
        }
    })
}

async fn round_trip(format: ApiFormat) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let route = match format {
        ApiFormat::OpenAi => "/v1/chat/completions",
        ApiFormat::Anthropic => "/v1/messages",
    };
    let mock = Router::new()
        .route(route, post(mock_model))
        .with_state(MockState {
            format,
            requests: requests.clone(),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let model = HttpModel::new(ModelConfig {
        format,
        endpoint: format!("http://{address}{route}").parse().unwrap(),
        api_key: "test-only-key".into(),
        model: "test-model".into(),
        anthropic_version: "2023-06-01".into(),
        openai_token_field: "max_completion_tokens".into(),
        max_output_tokens: 1024,
        request_timeout_seconds: 5,
    })
    .unwrap();
    let store = Arc::new(TaskStore::open(":memory:").unwrap());
    let executor = Executor::new(store.clone(), Some(Arc::new(model)), Default::default());
    let worker = tokio::spawn(executor.clone().serve());
    let app = configured_router(AppState {
        store: store.clone(),
        executor: executor.clone(),
        office: None,
        mcp: None,
        uploads: std::path::PathBuf::from(".data/test-uploads"),
        model: None,
        web_dist: None,
        memory: None,
        tools: None,
        workspace_root: std::env::temp_dir()
            .join(format!("yukii-workspaces-{}", uuid::Uuid::new_v4())),
    });
    let response = app
        .clone()
        .oneshot(
            Request::post("/api/tasks")
                .header("host", "localhost:8765")
                .header("idempotency-key", "integration-test")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"prompt":"计算 1+2+3 并生成报告","target":"cloud_office"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let task: Task =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    let finished = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current = store.get(&task.id).unwrap();
            if matches!(current.status, TaskStatus::Completed | TaskStatus::Failed) {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        finished.status,
        TaskStatus::Completed,
        "{:?}",
        finished.error
    );
    assert!(finished.result.unwrap().contains("合计为 6"));
    assert_eq!(finished.input_tokens, 40);
    assert_eq!(finished.output_tokens, 20);
    let ready = app
        .clone()
        .oneshot(
            Request::get("/api/ready")
                .header("host", "localhost:8765")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ready.status(), StatusCode::OK);
    let artifacts = store.artifacts(&task.id).unwrap();
    assert_eq!(artifacts.len(), 1);
    let response = app
        .clone()
        .oneshot(
            Request::get(format!("/api/artifacts/{}/download", artifacts[0].id))
                .header("host", "localhost:8765")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "text/markdown; charset=utf-8"
    );
    assert_eq!(
        to_bytes(response.into_body(), 10000).await.unwrap(),
        "# 结果\n\n合计为 6。"
    );
    let response = app
        .oneshot(
            Request::post(format!("/api/tasks/{}/run", task.id))
                .header("host", "localhost:8765")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(requests.lock().unwrap().len(), 4);
    let followup = store
        .create_once(
            CreateTask {
                conversation_id: Some(task.conversation_id.clone()),
                prompt: "继续解释刚才的结果".into(),
                target: ExecutionTarget::CloudOffice,
            },
            "followup",
            true,
        )
        .unwrap()
        .task;
    executor.notify();
    tokio::time::timeout(Duration::from_secs(5), async {
        while store.get(&followup.id).unwrap().status != TaskStatus::Completed {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let captured = requests.lock().unwrap().last().unwrap().clone();
    let messages = captured["messages"].as_array().unwrap();
    let offset = if matches!(format, ApiFormat::OpenAi) {
        1
    } else {
        0
    };
    assert_eq!(messages[offset]["content"], task.prompt);
    let serialized = serde_json::to_string(messages).unwrap();
    assert!(serialized.contains("合计为 6"));
    assert!(
        serialized.contains("继续解释刚才的结果"),
        "续聊请求应携带新的用户消息"
    );
    assert_eq!(store.conversations().unwrap().len(), 1);

    executor.shutdown();
    worker.await.unwrap();
    server.abort();
}
#[tokio::test]
async fn openai_http_tool_loop_and_artifact_download() {
    round_trip(ApiFormat::OpenAi).await;
}
#[tokio::test]
async fn anthropic_http_tool_loop_and_artifact_download() {
    round_trip(ApiFormat::Anthropic).await;
}

#[test]
fn cancelled_task_cannot_publish_late_results_or_documents() {
    let store = TaskStore::open(":memory:").unwrap();
    let task = store
        .create(CreateTask {
            conversation_id: None,
            prompt: "test".into(),
            target: ExecutionTarget::CloudOffice,
        })
        .unwrap();
    store
        .advance(&task.id, TaskStatus::Planning, "start", None, None)
        .unwrap();
    store
        .advance(&task.id, TaskStatus::Running, "run", None, None)
        .unwrap();
    store.cancel(&task.id).unwrap();
    assert!(store.document(&task.id, "late", "late").is_err());
    assert!(
        store
            .advance(&task.id, TaskStatus::Verifying, "late", None, None)
            .is_err()
    );
    assert!(store.artifacts(&task.id).unwrap().is_empty());
}

#[test]
fn restart_marks_only_inflight_tasks_failed() {
    let store = TaskStore::open(":memory:").unwrap();
    let task = store
        .create(CreateTask {
            conversation_id: None,
            prompt: "inflight".into(),
            target: ExecutionTarget::CloudOffice,
        })
        .unwrap();
    let queued = store
        .create(CreateTask {
            conversation_id: None,
            prompt: "queued".into(),
            target: ExecutionTarget::CloudOffice,
        })
        .unwrap();
    store
        .advance(&task.id, TaskStatus::Planning, "start", None, None)
        .unwrap();
    assert_eq!(store.recover_interrupted().unwrap(), 1);
    assert_eq!(store.get(&task.id).unwrap().status, TaskStatus::Failed);
    assert_eq!(store.get(&queued.id).unwrap().status, TaskStatus::Queued);
}
