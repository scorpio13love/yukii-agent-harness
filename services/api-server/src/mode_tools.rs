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
    UpdatePlan,
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

#[derive(Deserialize)]
struct UpdatePlanInput {
    steps: Vec<PlanStepInput>,
}

#[derive(Deserialize)]
struct PlanStepInput {
    title: String,
    status: String,
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
            Kind::UpdatePlan,
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
            Kind::UpdatePlan => (
                "update_plan",
                "维护当前执行计划的步骤状态。设计计划后、以及每完成一步后调用；传入完整步骤数组，界面会把状态固定在输入框上方，全部 completed 后自动隐藏。",
                json!({
                    "type":"object",
                    "properties":{
                        "steps":{
                            "type":"array",
                            "minItems":1,
                            "maxItems":20,
                            "items":{
                                "type":"object",
                                "properties":{
                                    "title":{"type":"string","maxLength":200},
                                    "status":{"type":"string","enum":["pending","in_progress","completed"]}
                                },
                                "required":["title","status"],
                                "additionalProperties":false
                            }
                        }
                    },
                    "required":["steps"],
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
            Kind::UpdatePlan => {
                let input: UpdatePlanInput =
                    serde_json::from_value(arguments).map_err(|_| "计划步骤参数无效")?;
                if input.steps.is_empty() || input.steps.len() > 20 {
                    return Err("计划步骤需要 1–20 项".into());
                }
                let mut steps = Vec::with_capacity(input.steps.len());
                for step in input.steps {
                    let title = step.title.trim();
                    if title.is_empty() || title.chars().count() > 200 {
                        return Err("步骤标题需要 1–200 字符".into());
                    }
                    let status = match step.status.as_str() {
                        "pending" | "in_progress" | "completed" => step.status,
                        _ => return Err("步骤状态无效".into()),
                    };
                    steps.push(json!({"title": title, "status": status}));
                }
                let mode = self
                    .store
                    .set_plan_steps(&session, &Value::Array(steps))
                    .map_err(|_| "更新计划步骤失败")?;
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
                let current = self
                    .store
                    .session_mode(&session)
                    .map_err(|_| "读取计划步骤失败")?;
                let has_steps = current
                    .as_ref()
                    .and_then(|mode| mode["plan_steps"].as_array())
                    .is_some_and(|steps| !steps.is_empty());
                if !has_steps {
                    let parsed = parse_plan_steps(&input.plan);
                    if !parsed.is_empty() {
                        self.store
                            .set_plan_steps(&session, &Value::Array(parsed))
                            .map_err(|_| "保存计划步骤失败")?;
                    }
                }
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

/// 从计划 Markdown 的列表/编号项提取初始步骤，供输入框上方的执行计划条立即展示。
fn parse_plan_steps(plan: &str) -> Vec<Value> {
    let mut steps = Vec::new();
    for line in plan.lines() {
        let text = line.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        let parsed = if let Some(rest) = text.strip_prefix("- [ ] ") {
            Some((rest, "pending"))
        } else if let Some(rest) = text
            .strip_prefix("- [x] ")
            .or_else(|| text.strip_prefix("- [X] "))
        {
            Some((rest, "completed"))
        } else if let Some(rest) = text.strip_prefix("- ").or_else(|| text.strip_prefix("* ")) {
            Some((rest, "pending"))
        } else if let Some((head, rest)) = text.split_once(". ") {
            head.chars()
                .all(|ch| ch.is_ascii_digit())
                .then_some((rest, "pending"))
        } else if let Some((head, rest)) = text.split_once(") ") {
            head.chars()
                .all(|ch| ch.is_ascii_digit())
                .then_some((rest, "pending"))
        } else {
            None
        };
        let Some((title, status)) = parsed else {
            continue;
        };
        let title = title.trim().trim_end_matches('*').trim();
        if title.is_empty() {
            continue;
        }
        steps.push(json!({"title": title, "status": status}));
        if steps.len() >= 12 {
            break;
        }
    }
    steps
}
