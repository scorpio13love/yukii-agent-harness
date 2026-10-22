-- Computer Use 审计账本：只保存窗口身份、截图摘要和动作结果，不保存截图像素。
CREATE TABLE computer_sessions (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    window_id INTEGER NOT NULL,
    pid INTEGER NOT NULL,
    application TEXT NOT NULL,
    title TEXT NOT NULL,
    screenshot_sharing INTEGER NOT NULL CHECK(screenshot_sharing IN (0,1)),
    generation INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('active','paused','ended')),
    ended_at INTEGER,
    FOREIGN KEY(task_id) REFERENCES tasks(id)
);
CREATE INDEX computer_sessions_by_session_state ON computer_sessions(session_id, state);

CREATE TABLE computer_observations (
    id TEXT PRIMARY KEY,
    control_id TEXT NOT NULL REFERENCES computer_sessions(id),
    task_id TEXT NOT NULL,
    image_sha256 TEXT NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    captured_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    consumed_at INTEGER,
    CHECK(expires_at > captured_at)
);
CREATE INDEX computer_observations_by_control ON computer_observations(control_id);

-- intent 是“即将派发”的持久记录；没有终态的动作不能自动重放。
CREATE TABLE computer_actions (
    id TEXT PRIMARY KEY,
    observation_id TEXT NOT NULL REFERENCES computer_observations(id),
    control_id TEXT NOT NULL REFERENCES computer_sessions(id),
    task_id TEXT NOT NULL,
    call_id TEXT NOT NULL,
    action TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('intent','dispatched','rejected','outcome_unknown')),
    created_at INTEGER NOT NULL,
    finished_at INTEGER,
    result TEXT
);
CREATE INDEX computer_actions_by_control ON computer_actions(control_id);
CREATE INDEX computer_actions_by_call ON computer_actions(task_id, call_id);
