//! Goal / plan 模式的控制工具：模型可以在对话中直接建立长期目标或进入计划模式。
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use yukii_agent_ports::{
    model::ToolDefinition,
    repository::{RepositoryError, TaskRepository},
    tools::{ExecutionTarget, Tool, ToolCatalog, ToolContext, ToolEffect},
};

pub struct ModeTools {
    store: Arc<dyn TaskRepository>,
}

impl ModeTools {
    pub fn new(store: Arc<dyn TaskRepository>) -> Self {
        Self { store }
    }
}

struct ModeTool {
    store: Arc<dyn TaskRepository>,
    kind: Kind,
}

#[derive(Clone, Copy)]
enum Kind {
    GetGoal,
    CreateGoal,
    UpdateGoal,
    EnterPlan,
    ExitPlan,
}

#[derive(Deserialize)]
struct CreateGoalInput {
    objective: String,
    #[serde(default)]
    max_goal_rounds: Option<i64>,
}

#[derive(Deserialize)]
struct UpdateGoalInput {
    action: String,
    #[serde(default)]
    objective: Option<String>,
    #[serde(default)]
    max_goal_rounds: Option<i64>,
    #[serde(default)]
    blocked_reason: Option<String>,
}

#[derive(Deserialize)]
struct PlanInput {
    plan: String,
}

#[async_trait]
impl ToolCatalog for ModeTools {
    async fn tools(&self) -> Result<Vec<Arc<dyn Tool>>, RepositoryError> {
        Ok([
            Kind::GetGoal,
            Kind::CreateGoal,
            Kind::UpdateGoal,
            Kind::EnterPlan,
            Kind::ExitPlan,
        ]
        .into_iter()
        .map(|kind| {
            Arc::new(ModeTool {
                store: self.store.clone(),
                kind,
            }) as Arc<dyn Tool>
        })
        .collect())
    }
}

#[async_trait]
impl Tool for ModeTool {
    fn definition(&self) -> ToolDefinition {
        let (name, description, schema) = match self.kind {
            Kind::GetGoal => (
                "get_goal",
                "读取当前会话的持久目标；没有目标时返回 null。",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            Kind::CreateGoal => (
                "create_goal",
                "当用户提出需要多轮持续推进的长期目标时创建 goal；不要为普通单轮任务创建。已有未完成目标时先让用户编辑或清除。",
                json!({
                    "type":"object",
                    "properties":{
                        "objective":{"type":"string","maxLength":4000},
                        "max_goal_rounds":{"type":"integer","minimum":1,"maximum":20,"default":6}
                    },
                    "required":["objective"],
                    "additionalProperties":false
                }),
            ),
            Kind::UpdateGoal => (
                "update_goal",
                "更新当前 goal：edit 修改目标，pause/resume 暂停或恢复，complete 在验证目标完成后结束，blocked 在同一个阻塞持续至少 3 轮后报告。",
                json!({
                    "type":"object",
                    "properties":{
                        "action":{"type":"string","enum":["edit","pause","resume","complete","blocked"]},
                        "objective":{"type":"string","maxLength":4000},
                        "max_goal_rounds":{"type":"integer","minimum":1,"maximum":20},
                        "blocked_reason":{"type":"string","maxLength":1000}
                    },
                    "required":["action"],
                    "additionalProperties":false
                }),
            ),
            Kind::EnterPlan => (
                "enter_plan_mode",
                "当用户需要先探索和设计方案、再开始修改时进入计划模式。进入后不要执行有副作用的修改，整理完整方案后用 exit_plan_mode 提交。",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            Kind::ExitPlan => (
                "exit_plan_mode",
                "在计划模式中提交以 # 标题开头的完整 Markdown 计划，交给用户评审；用户批准后才会退出计划模式。只有在方案已经清晰后才调用。",
                json!({
                    "type":"object",
                    "properties":{"plan":{"type":"string","maxLength":20000}},
                    "required":["plan"],
                    "additionalProperties":false
                }),
            ),
        };
        ToolDefinition {
            name: name.into(),
            description: description.into(),
            schema,
        }
    }

    fn execution_target(&self) -> ExecutionTarget {
        ExecutionTarget::CloudOffice
    }

    async fn execute(&self, context: &ToolContext, arguments: Value) -> Result<ToolEffect, String> {
        let session = self
            .store
            .get(&context.task_id)
            .map(|task| task.conversation_id)
            .map_err(|_| "任务不存在")?;
        match self.kind {
            Kind::GetGoal => {
                let mode = self
                    .store
                    .session_mode(&session)
                    .map_err(|_| "读取目标失败")?;
                Ok(ToolEffect::Result(json!({
                    "goal": mode.as_ref().and_then(|mode| mode.get("goal").cloned()).unwrap_or(Value::Null),
                    "plan_active": mode.as_ref().and_then(|mode| mode["plan_active"].as_bool()).unwrap_or(false),
                })))
            }
            Kind::CreateGoal => {
                let input: CreateGoalInput =
                    serde_json::from_value(arguments).map_err(|_| "目标参数无效")?;
                let objective = input.objective.trim();
                if objective.is_empty() || objective.chars().count() > 4000 {
                    return Err("目标需要 1–4000 字符".into());
                }
                let max_rounds = input.max_goal_rounds.unwrap_or(6).clamp(1, 20);
                let goal = self
                    .store
                    .set_goal(&session, objective, max_rounds)
                    .map_err(|_| "创建目标失败")?;
                Ok(ToolEffect::Result(goal))
            }
            Kind::UpdateGoal => {
                let input: UpdateGoalInput =
                    serde_json::from_value(arguments).map_err(|_| "目标更新参数无效")?;
                let mode = self
                    .store
                    .session_mode(&session)
                    .map_err(|_| "读取目标失败")?
                    .ok_or("当前没有目标")?;
                let goal = mode
                    .get("goal")
                    .filter(|goal| !goal.is_null())
                    .ok_or("当前没有目标")?;
                match input.action.as_str() {
                    "edit" => {
                        let objective = input
                            .objective
                            .as_deref()
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .ok_or("edit 需要新 objective")?;
                        let max_rounds = input
                            .max_goal_rounds
                            .unwrap_or_else(|| goal["max_rounds"].as_i64().unwrap_or(6))
                            .clamp(1, 20);
                        let updated = self
                            .store
                            .set_goal(&session, objective, max_rounds)
                            .map_err(|_| "更新目标失败")?;
                        Ok(ToolEffect::Result(updated))
                    }
                    "pause" | "resume" | "complete" => {
                        let phase = match input.action.as_str() {
                            "pause" => "paused",
                            "resume" => "active",
                            _ => "complete",
                        };
                        let updated = self
                            .store
                            .set_goal_phase(&session, phase)
                            .map_err(|_| "更新目标状态失败")?;
                        Ok(ToolEffect::Result(updated))
                    }
                    "blocked" => {
                        let rounds = goal["rounds"].as_i64().unwrap_or(0);
                        if rounds < 3 {
                            return Err("同一个阻塞条件至少持续 3 轮后才能标记 blocked".into());
                        }
                        if input
                            .blocked_reason
                            .as_deref()
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .is_none()
                        {
                            return Err("blocked 需要具体阻塞原因".into());
                        }
                        let updated = self
                            .store
                            .set_goal_phase(&session, "blocked")
                            .map_err(|_| "更新目标状态失败")?;
                        Ok(ToolEffect::Result(updated))
                    }
                    _ => Err("action 无效".into()),
                }
            }
            Kind::EnterPlan => {
                let mode = self
                    .store
                    .set_plan_active(&session, true)
                    .map_err(|_| "进入计划模式失败")?;
                Ok(ToolEffect::Result(mode))
            }
            Kind::ExitPlan => {
                let input: PlanInput =
                    serde_json::from_value(arguments).map_err(|_| "计划参数无效")?;
                if !input.plan.trim().starts_with('#') {
                    return Err("计划必须是以 # 标题开头的 Markdown".into());
                }
                self.store
                    .set_plan_pending(&session, &input.plan)
                    .map_err(|_| "提交计划评审失败")?;
                Ok(ToolEffect::Result(json!({
                    "approved": false,
                    "pending_review": true,
                    "plan": input.plan,
                    "note": "计划已提交给用户评审；不要继续执行，等待用户批准或反馈。"
                })))
            }
        }
    }
}
