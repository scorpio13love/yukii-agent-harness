//! Computer Use 审计账本：控制会话、截图摘要与动作意图。截图像素不写入执行端数据库。
use crate::{
    StoreError, TaskStore,
    sqlite::{append_event, ensure_active, now, read_task},
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use yukii_contracts::desktop::{ControlSession, ControlState, DesktopObservation, GuiAction};

type SessionRow = (String, String, i64, i64, String, String, bool, i64, i64);

impl TaskStore {
    /// 宿主在用户同意后登记控制会话。任务此时可以仍是 queued，真正动作在运行中才允许。
    /// 同一执行进程只应有一个活动会话；执行端内存状态负责互斥，本表保留审计。
    pub fn start_computer_session(
        &self,
        session_id: &str,
        control: &ControlSession,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let task = read_task(&transaction, &control.task_id)?;
        if matches!(
            task.status,
            yukii_contracts::TaskStatus::Completed
                | yukii_contracts::TaskStatus::Failed
                | yukii_contracts::TaskStatus::Cancelled
        ) {
            return Err(yukii_agent_domain::TaskError::InvalidTransition.into());
        }
        if control.state != ControlState::Active {
            return Err(StoreError::ControlUnavailable);
        }
        transaction.execute(
            "INSERT INTO computer_sessions(id,session_id,task_id,window_id,pid,application,title,
                screenshot_sharing,generation,created_at,expires_at,state)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'active')",
            params![
                control.control_id,
                session_id,
                control.task_id,
                control.window.window_id,
                control.window.pid,
                control.window.application,
                control.window.title,
                control.screenshot_sharing,
                control.generation as i64,
                now(),
                control.expires_at
            ],
        )?;
        append_event(
            &transaction,
            &task,
            &format!(
                "用户已同意控制窗口“{}”（{}），截图分享={}，期限至 {}。",
                control.window.title,
                control.window.application,
                if control.screenshot_sharing {
                    "是"
                } else {
                    "否"
                },
                control.expires_at
            ),
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// 暂停/恢复/结束只改审计状态；执行端内存状态是动作门禁，避免数据库损坏后静默放行。
    pub fn set_computer_session_state(
        &self,
        control_id: &str,
        state: ControlState,
    ) -> Result<ControlSession, StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: Option<SessionRow> = transaction
            .query_row(
                "SELECT session_id,task_id,window_id,pid,application,title,screenshot_sharing,generation,expires_at
                 FROM computer_sessions WHERE id=?1",
                [control_id],
                |row| {
                    Ok((
                        row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
                        row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?,
                    ))
                },
            )
            .optional()?;
        let Some((_, task_id, window_id, pid, application, title, sharing, generation, expires_at)) =
            row
        else {
            return Err(StoreError::ControlUnavailable);
        };
        let label = match state {
            ControlState::Active => "active",
            ControlState::Paused => "paused",
            ControlState::Ended => "ended",
        };
        let changed = transaction.execute(
            "UPDATE computer_sessions SET state=?1, ended_at=CASE WHEN ?1='ended' THEN ?2 ELSE ended_at END
             WHERE id=?3 AND state!='ended'",
            params![label, now(), control_id],
        )?;
        if changed != 1 {
            return Err(StoreError::ControlUnavailable);
        }
        let task = read_task(&transaction, &task_id)?;
        let message = match state {
            ControlState::Active => "桌面控制已恢复，旧截图不再有效，需要重新观察。",
            ControlState::Paused => "桌面控制已暂停，将拒绝新的观察和动作。",
            ControlState::Ended => "桌面控制已结束。",
        };
        append_event(&transaction, &task, message)?;
        transaction.commit()?;
        Ok(ControlSession {
            control_id: control_id.into(),
            task_id,
            window: yukii_contracts::desktop::DesktopWindow {
                window_id: window_id as u32,
                pid: pid as i32,
                application,
                title,
            },
            screenshot_sharing: sharing,
            generation: generation as u64,
            expires_at,
            state,
        })
    }

    /// 只保存截图摘要和尺寸；像素仅回到创建私有管道的宿主。
    pub fn record_computer_observation(
        &self,
        control_id: &str,
        observation: &DesktopObservation,
        image_sha256: &str,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (task_id, control_state): (String, String) = transaction
            .query_row(
                "SELECT task_id,state FROM computer_sessions WHERE id=?1",
                [control_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(StoreError::ControlUnavailable)?;
        if control_state != "active" {
            return Err(StoreError::ControlUnavailable);
        }
        transaction.execute(
            "INSERT INTO computer_observations(id,control_id,task_id,image_sha256,width,height,captured_at,expires_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                observation.observation_id,
                control_id,
                task_id,
                image_sha256,
                observation.width,
                observation.height,
                observation.captured_at,
                observation.expires_at
            ],
        )?;
        append_event(
            &transaction,
            &read_task(&transaction, &task_id)?,
            "已捕获受控窗口画面，截图仅交付当前受信宿主。",
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// 动作派发前先原子消费观察并登记 intent；状态写入成功后才可能产生外部副作用。
    /// 返回动作 ID；调用方必须在派发后写入终态，不能把 intent 当作已执行。
    pub fn begin_computer_action(
        &self,
        control_id: &str,
        observation_id: &str,
        task_id: &str,
        call_id: &str,
        action: &GuiAction,
        action_id: &str,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let observation: Option<(String, String, i64, i64, Option<i64>)> = transaction
            .query_row(
                "SELECT control_id,task_id,captured_at,expires_at,consumed_at
                 FROM computer_observations WHERE id=?1",
                [observation_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((stored_control, stored_task, captured_at, expires_at, consumed_at)) = observation
        else {
            return Err(StoreError::ObservationUnavailable);
        };
        let now = now();
        if stored_control != control_id
            || stored_task != task_id
            || consumed_at.is_some()
            || captured_at > now
            || expires_at <= now
            || expires_at.saturating_sub(captured_at)
                > yukii_agent_domain::computer::MAX_OBSERVATION_SECONDS
        {
            return Err(StoreError::ObservationUnavailable);
        }
        let task = read_task(&transaction, task_id)?;
        ensure_active(&task)?;
        let changed = transaction.execute(
            "UPDATE computer_observations SET consumed_at=?1 WHERE id=?2 AND consumed_at IS NULL",
            params![now, observation_id],
        )?;
        if changed != 1 {
            return Err(StoreError::ObservationUnavailable);
        }
        transaction.execute(
            "INSERT INTO computer_actions(id,observation_id,control_id,task_id,call_id,action,state,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,'intent',?7)",
            params![
                action_id,
                observation_id,
                control_id,
                task_id,
                call_id,
                serde_json::to_string(action)?,
                now
            ],
        )?;
        append_event(&transaction, &task, "GUI 动作已登记，准备派发。")?;
        transaction.commit()?;
        Ok(())
    }

    /// 终态保存失败时调用方必须把结果当作未知，不能重放同一次点击。
    pub fn finish_computer_action(
        &self,
        action_id: &str,
        state: &str,
        result: Option<&str>,
    ) -> Result<(), StoreError> {
        if !matches!(state, "dispatched" | "rejected" | "outcome_unknown") {
            return Err(StoreError::ControlUnavailable);
        }
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE computer_actions SET state=?1,finished_at=?2,result=?3 WHERE id=?4 AND state='intent'",
            params![state, now(), result, action_id],
        )?;
        if changed != 1 {
            return Err(StoreError::IdempotencyConflict);
        }
        let task_id: String = transaction.query_row(
            "SELECT task_id FROM computer_actions WHERE id=?1",
            [action_id],
            |row| row.get(0),
        )?;
        let message = match state {
            "dispatched" => "GUI 动作已派发；系统投递不代表目标已达成，需要重新观察。",
            "rejected" => "GUI 动作未派发，执行端返回错误。",
            _ => "GUI 动作结果不确定，禁止自动重放，请人工核对。",
        };
        append_event(&transaction, &read_task(&transaction, &task_id)?, message)?;
        transaction.commit()?;
        Ok(())
    }

    /// 执行进程重启后旧的 active/paused 记录不再有内存门禁，必须显式结束而不是恢复。
    /// 受信宿主对数据目录有排他锁，因此同一数据库不应存在另一个活跃执行进程；
    /// 这里关闭所有遗留会话，避免换用新 session_id 后旧记录永久显示为活动。
    pub fn close_stale_computer_sessions(&self, _session_id: &str) -> Result<usize, StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Lock)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows = {
            let mut statement = transaction.prepare(
                "SELECT id,task_id FROM computer_sessions
                 WHERE state IN ('active','paused')",
            )?;
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (control_id, task_id) in &rows {
            transaction.execute(
                "UPDATE computer_sessions SET state='ended',ended_at=?1 WHERE id=?2",
                params![now(), control_id],
            )?;
            if let Ok(task) = read_task(&transaction, task_id) {
                append_event(
                    &transaction,
                    &task,
                    "执行端重启，旧桌面控制会话已结束，需要重新征得同意。",
                )?;
            }
        }
        // intent 表示“可能已派发但没有终态”。重启后不得重跑，只能标记未知供核对。
        let intents = {
            let mut statement = transaction
                .prepare("SELECT id,task_id FROM computer_actions WHERE state='intent'")?;
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (action_id, task_id) in &intents {
            transaction.execute(
                "UPDATE computer_actions SET state='outcome_unknown',finished_at=?1,result=?2
                 WHERE id=?3 AND state='intent'",
                params![
                    now(),
                    "执行进程重启，无法确认动作是否已派发；禁止自动重放。",
                    action_id
                ],
            )?;
            if let Ok(task) = read_task(&transaction, task_id) {
                append_event(
                    &transaction,
                    &task,
                    "执行端重启，未完成 GUI 动作结果未知，禁止自动重放。",
                )?;
            }
        }
        transaction.commit()?;
        Ok(rows.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yukii_agent_domain::computer::MAX_OBSERVATION_SECONDS;
    use yukii_contracts::{
        CreateTask, ExecutionTarget, TaskStatus,
        desktop::{DesktopWindow, GuiAction},
    };

    fn store() -> TaskStore {
        TaskStore::open(":memory:").unwrap()
    }

    fn window() -> DesktopWindow {
        DesktopWindow {
            window_id: 7,
            pid: 99,
            application: "Test".into(),
            title: "测试窗口".into(),
        }
    }

    fn control(task_id: &str) -> ControlSession {
        ControlSession {
            control_id: "control-1".into(),
            task_id: task_id.into(),
            window: window(),
            screenshot_sharing: true,
            generation: 1,
            expires_at: now() + 600,
            state: ControlState::Active,
        }
    }

    fn running_task(store: &TaskStore) -> String {
        let task = store
            .create_local_once(
                CreateTask {
                    conversation_id: None,
                    prompt: "测试".into(),
                    target: ExecutionTarget::LocalComputer,
                },
                "key",
                true,
            )
            .unwrap()
            .task;
        store.claim_next().unwrap().unwrap();
        task.id
    }

    fn observation(id: &str, _task_id: &str) -> DesktopObservation {
        DesktopObservation {
            observation_id: id.into(),
            window: window(),
            png_base64: "iVBORw0KGgo=".into(),
            width: 100,
            height: 50,
            frame: [0.0, 0.0, 100.0, 50.0],
            elements: Vec::new(),
            captured_at: now(),
            expires_at: now() + MAX_OBSERVATION_SECONDS,
        }
    }

    #[test]
    fn observation_is_single_use_and_action_states_are_terminal() {
        let store = store();
        let task_id = running_task(&store);
        store
            .start_computer_session("session", &control(&task_id))
            .unwrap();
        store
            .record_computer_observation("control-1", &observation("obs", &task_id), "digest")
            .unwrap();
        store
            .begin_computer_action(
                "control-1",
                "obs",
                &task_id,
                "call",
                &GuiAction::Click { x: 1.0, y: 2.0 },
                "action",
            )
            .unwrap();
        assert!(matches!(
            store.begin_computer_action(
                "control-1",
                "obs",
                &task_id,
                "call-2",
                &GuiAction::Click { x: 1.0, y: 2.0 },
                "action-2",
            ),
            Err(StoreError::ObservationUnavailable)
        ));
        store
            .finish_computer_action("action", "dispatched", Some("ok"))
            .unwrap();
        assert!(
            store
                .finish_computer_action("action", "dispatched", Some("ok"))
                .is_err()
        );
        assert_eq!(store.get(&task_id).unwrap().status, TaskStatus::Running);
    }

    #[test]
    fn cancelled_task_and_paused_control_cannot_dispatch_new_actions() {
        let store = store();
        let task_id = running_task(&store);
        store
            .start_computer_session("session", &control(&task_id))
            .unwrap();
        store
            .record_computer_observation("control-1", &observation("obs", &task_id), "digest")
            .unwrap();
        store
            .set_computer_session_state("control-1", ControlState::Paused)
            .unwrap();
        assert!(matches!(
            store.record_computer_observation(
                "control-1",
                &observation("obs-2", &task_id),
                "digest"
            ),
            Err(StoreError::ControlUnavailable)
        ));
        store
            .set_computer_session_state("control-1", ControlState::Active)
            .unwrap();
        store.cancel(&task_id).unwrap();
        assert!(matches!(
            store.begin_computer_action(
                "control-1",
                "obs",
                &task_id,
                "call",
                &GuiAction::Click { x: 1.0, y: 2.0 },
                "action",
            ),
            Err(StoreError::Validation(_))
        ));
    }

    #[test]
    fn interrupted_action_intent_becomes_outcome_unknown_on_restart() {
        let store = store();
        let task_id = running_task(&store);
        store
            .start_computer_session("session", &control(&task_id))
            .unwrap();
        store
            .record_computer_observation("control-1", &observation("obs", &task_id), "digest")
            .unwrap();
        store
            .begin_computer_action(
                "control-1",
                "obs",
                &task_id,
                "call",
                &GuiAction::TypeText {
                    text: "未完成".into(),
                },
                "action",
            )
            .unwrap();
        assert_eq!(store.close_stale_computer_sessions("session").unwrap(), 1);
        let connection = store.connection.lock().unwrap();
        let state: String = connection
            .query_row(
                "SELECT state FROM computer_actions WHERE id='action'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "outcome_unknown");
        drop(connection);
        assert!(
            store
                .events(&task_id)
                .unwrap()
                .iter()
                .any(|event| { event.message.contains("未完成 GUI 动作结果未知") })
        );
    }

    #[test]
    fn stale_sessions_are_ended_on_restart_without_resuming_control() {
        let store = store();
        let task_id = running_task(&store);
        store
            .start_computer_session("session", &control(&task_id))
            .unwrap();
        assert_eq!(store.close_stale_computer_sessions("session").unwrap(), 1);
        assert!(
            store
                .set_computer_session_state("control-1", ControlState::Active)
                .is_err()
        );
        assert_eq!(store.close_stale_computer_sessions("session").unwrap(), 0);
    }
}
