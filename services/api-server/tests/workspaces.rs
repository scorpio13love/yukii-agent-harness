use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::Value;
use std::{path::Path, sync::Arc};
use tower::ServiceExt;
use yukii_agent_ports::tools::{ToolCatalog, ToolContext};
use yukii_api_server::{
    AppState, configured_router, dev_tools::DevTools, executor::Executor, store::TaskStore,
};
use yukii_contracts::Task;

fn create_task(key: &str, prompt: &str) -> Request<Body> {
    Request::post("/api/tasks")
        .header("host", "localhost:8765")
        .header("content-type", "application/json")
        .header("idempotency-key", key)
        .body(Body::from(
            serde_json::json!({"prompt": prompt, "target": "cloud_office"}).to_string(),
        ))
        .unwrap()
}

#[tokio::test]
async fn session_workspace_is_created_and_can_fall_back_from_project() {
    let root = std::env::temp_dir().join(format!("yukii-workspaces-{}", uuid::Uuid::new_v4()));
    let store = Arc::new(TaskStore::open(":memory:").unwrap());
    let executor = Executor::new(store.clone(), None, Default::default());
    let app = configured_router(AppState {
        store: store.clone(),
        executor,
        office: None,
        mcp: None,
        uploads: std::env::temp_dir().join("yukii-test-uploads"),
        model: None,
        web_dist: None,
        memory: None,
        tools: None,
        workspace_root: root.clone(),
    });

    let response = app
        .clone()
        .oneshot(create_task("workspace-create", "写一个文件"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let task: Task =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();

    let workspace = store
        .session_workspace(&task.conversation_id)
        .unwrap()
        .unwrap();
    assert_eq!(workspace["kind"], "session");
    let default_path = workspace["path"].as_str().unwrap();
    assert!(Path::new(default_path).is_dir());
    assert!(Path::new(default_path).starts_with(&root));

    let tools = DevTools::new(store.clone(), root.clone())
        .tools()
        .await
        .unwrap();
    let write = tools
        .iter()
        .find(|tool| tool.definition().name == "write_file")
        .unwrap();
    write
        .execute(
            &ToolContext::office(&task.id, "call-1"),
            serde_json::json!({"path": "note.txt", "content": "hello"}),
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(Path::new(default_path).join("note.txt")).unwrap(),
        "hello"
    );

    let response = app
        .clone()
        .oneshot(
            Request::get("/api/workspaces")
                .header("host", "localhost:8765")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(body["workspaces"].as_array().unwrap().len(), 1);

    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::put(format!("/api/sessions/{}/workspace", task.conversation_id))
                .header("host", "localhost:8765")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"path": project.to_string_lossy()}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let workspace = store
        .session_workspace(&task.conversation_id)
        .unwrap()
        .unwrap();
    assert_eq!(workspace["kind"], "project");
    assert_eq!(
        workspace["path"],
        std::fs::canonicalize(&project)
            .unwrap()
            .to_string_lossy()
            .to_string()
    );

    let response = app
        .oneshot(
            Request::delete(format!("/api/sessions/{}/workspace", task.conversation_id))
                .header("host", "localhost:8765")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let workspace = store
        .session_workspace(&task.conversation_id)
        .unwrap()
        .unwrap();
    assert_eq!(workspace["kind"], "session");
    assert_eq!(workspace["path"], default_path);

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn goal_and_plan_modes_are_persisted_for_a_new_session() {
    let root = std::env::temp_dir().join(format!("yukii-modes-{}", uuid::Uuid::new_v4()));
    let store = Arc::new(TaskStore::open(":memory:").unwrap());
    let executor = Executor::new(store.clone(), None, Default::default());
    let app = configured_router(AppState {
        store: store.clone(),
        executor,
        office: None,
        mcp: None,
        uploads: std::env::temp_dir().join("yukii-test-uploads"),
        model: None,
        web_dist: None,
        memory: None,
        tools: None,
        workspace_root: root.clone(),
    });

    let response = app
        .clone()
        .oneshot(
            Request::post("/api/tasks")
                .header("host", "localhost:8765")
                .header("content-type", "application/json")
                .header("idempotency-key", "goal-mode")
                .body(Body::from(
                    serde_json::json!({
                        "prompt": "完成长期报告",
                        "target": "cloud_office",
                        "mode": "goal",
                        "goal_objective": "完成长期报告",
                        "command": "/goal"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let task: Task =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    let mode = store.session_mode(&task.conversation_id).unwrap().unwrap();
    assert_eq!(mode["goal"]["objective"], "完成长期报告");
    assert_eq!(mode["goal"]["phase"], "active");
    assert_eq!(task.command.as_deref(), Some("/goal"));

    let response = app
        .clone()
        .oneshot(
            Request::post(format!("/api/sessions/{}/plan", task.conversation_id))
                .header("host", "localhost:8765")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"active": true}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mode = store.session_mode(&task.conversation_id).unwrap().unwrap();
    assert_eq!(mode["plan_active"], true);
    store.cancel(&task.id).unwrap();

    store
        .set_plan_pending(&task.conversation_id, "# 计划\n\n先检查再执行")
        .unwrap();
    store
        .set_plan_steps(
            &task.conversation_id,
            &serde_json::json!([
                {"title": "检查现状", "status": "completed"},
                {"title": "执行修改", "status": "in_progress"},
                {"title": "验证结果", "status": "pending"}
            ]),
        )
        .unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::post(format!(
                "/api/sessions/{}/plan/decision",
                task.conversation_id
            ))
            .header("host", "localhost:8765")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"approved": true}).to_string(),
            ))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mode = store.session_mode(&task.conversation_id).unwrap().unwrap();
    assert_eq!(mode["plan_active"], false);
    assert!(mode["pending_plan"].is_null());
    assert_eq!(mode["plan_steps"].as_array().unwrap().len(), 3);
    assert_eq!(mode["plan_steps"][1]["title"], "执行修改");
    assert_eq!(store.conversation(&task.conversation_id).unwrap().len(), 2);

    let response = app
        .clone()
        .oneshot(
            Request::post(format!("/api/sessions/{}/plan", task.conversation_id))
                .header("host", "localhost:8765")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"active": false}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mode = store.session_mode(&task.conversation_id).unwrap().unwrap();
    assert!(mode["plan_steps"].as_array().unwrap().is_empty());

    store
        .record_file_change(
            &task.id,
            &task.conversation_id,
            "note.txt",
            "old\n",
            "new\n",
            "modify",
        )
        .unwrap();
    let response = app
        .oneshot(
            Request::get(format!("/api/tasks/{}/file-changes", task.id))
                .header("host", "localhost:8765")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(body["changes"][0]["path"], "note.txt");
    assert!(
        body["changes"][0]["absolute_path"]
            .as_str()
            .unwrap()
            .ends_with("note.txt")
    );

    let _ = std::fs::remove_dir_all(root);
}
