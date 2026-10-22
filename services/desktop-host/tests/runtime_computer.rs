//! 本地 Agent 循环 + Computer Use 工具目录 + 图像回执的集成测试。
//! 使用假模型与假桌面控制，不访问真实屏幕或网络。
use async_trait::async_trait;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use yukii_agent_core::runner::AgentLimits;
use yukii_agent_infra::TaskStore;
use yukii_agent_ports::{
    model::{LanguageModel, Message, ModelError, ModelReply, ToolCall, ToolDefinition, Usage},
    tools::{DesktopControl, DesktopObservationResult},
};
use yukii_agent_runtime::{Components, Executor};
use yukii_contracts::{CreateTask, ExecutionTarget, TaskStatus, desktop::GuiAction};
use yukii_desktop_tools::ComputerTools;

struct FakeModel {
    turn: AtomicUsize,
}

#[async_trait]
impl LanguageModel for FakeModel {
    async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<ModelReply, ModelError> {
        assert!(system.contains("observe_screen"));
        assert!(tools.iter().any(|tool| tool.name == "click"));
        let turn = self.turn.fetch_add(1, Ordering::SeqCst);
        let calls = match turn {
            0 => vec![ToolCall {
                id: "call-observe".into(),
                name: "observe_screen".into(),
                arguments: serde_json::json!({}),
            }],
            1 => {
                let observation_id = last_observation(messages).expect("模型必须收到截图与观察 ID");
                assert!(
                    messages.iter().any(|message| match message {
                        Message::ToolResults(results) =>
                            results.iter().any(|result| result.images.len() == 1
                                && result.images[0].media_type == "image/png"),
                        _ => false,
                    }),
                    "runtime 必须把截图保留在工具结果中"
                );
                vec![ToolCall {
                    id: "call-click".into(),
                    name: "click".into(),
                    arguments: serde_json::json!({
                        "observation_id": observation_id,
                        "x": 10.0,
                        "y": 20.0
                    }),
                }]
            }
            _ => {
                let acted = messages.iter().any(|message| match message {
                    Message::ToolResults(results) => results
                        .iter()
                        .any(|result| result.content.contains("verification_required")),
                    _ => false,
                });
                assert!(acted, "模型必须看到动作派发但未验证的结果");
                return Ok(ModelReply {
                    text: "已观察到测试窗口并派发点击；尚未重新观察确认目标。".into(),
                    calls: Vec::new(),
                    usage: Usage::default(),
                });
            }
        };
        Ok(ModelReply {
            text: String::new(),
            calls,
            usage: Usage::default(),
        })
    }
}

fn last_observation(messages: &[Message]) -> Option<String> {
    for message in messages.iter().rev() {
        if let Message::ToolResults(results) = message {
            for result in results {
                let value: serde_json::Value = serde_json::from_str(&result.content).ok()?;
                if let Some(id) = value["observation_id"].as_str() {
                    return Some(id.to_owned());
                }
            }
        }
    }
    None
}

#[derive(Default)]
struct FakeControl {
    actions: Mutex<Vec<GuiAction>>,
}

#[async_trait]
impl DesktopControl for FakeControl {
    async fn observe(&self, _: &str) -> Result<DesktopObservationResult, String> {
        Ok(DesktopObservationResult {
            observation_id: "observation-1".into(),
            // 1x1 PNG：只验证图像内容经过模型消息，不解析像素。
            png_base64: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=".into(),
            width: 800,
            height: 600,
            elements: Vec::new(),
        })
    }

    async fn act(
        &self,
        _: &str,
        _: &str,
        observation_id: &str,
        action: GuiAction,
    ) -> Result<String, String> {
        if observation_id != "observation-1" {
            return Err("观察 ID 不匹配".into());
        }
        self.actions.lock().unwrap().push(action.clone());
        Ok("{\"action_id\":\"action-1\",\"verification_required\":true}".into())
    }
}

#[tokio::test]
async fn local_runtime_runs_observe_then_act_and_persists_screenshot_receipt() {
    let store = Arc::new(TaskStore::open(":memory:").unwrap());
    let created = store
        .create_local_once(
            CreateTask {
                conversation_id: None,
                prompt: "在测试窗口点击坐标 10,20".into(),
                target: ExecutionTarget::LocalComputer,
            },
            "runtime-computer",
            false,
        )
        .unwrap();
    let task_id = created.task.id.clone();
    let control = Arc::new(FakeControl::default());
    let executor = Executor::with_components(
        store.clone(),
        Components {
            model: Some(Arc::new(FakeModel {
                turn: AtomicUsize::new(0),
            })),
            tools: Arc::new(ComputerTools::new(control.clone())),
            contexts: Vec::new(),
            system_prompt: None,
            context_window_tokens: 128_000,
            max_output_tokens: 8_192,
            compaction_headroom_tokens: 16_384,
        },
        AgentLimits {
            timeout: Duration::from_secs(10),
            ..Default::default()
        },
    );
    let worker = tokio::spawn(executor.clone().serve());
    executor.start(task_id.clone()).await.unwrap();
    let task = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let task = store.get(&task_id).unwrap();
            if matches!(
                task.status,
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
            ) {
                break task;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("任务必须在测试时限内结束");
    executor.shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(5), worker).await;

    assert_eq!(task.status, TaskStatus::Completed, "{:?}", task.error);
    assert!(task.result.unwrap().contains("尚未重新观察"));
    assert_eq!(control.actions.lock().unwrap().len(), 1);
    let events = store.events(&task_id).unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.message.contains("工具执行完成，回执已保存"))
    );
}
