-- 持久化提交与执行意图；迁移在单一事务中应用。
CREATE TABLE IF NOT EXISTS idempotency_keys (
    request_key TEXT PRIMARY KEY,
    request_payload TEXT NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(id)
);
CREATE TABLE IF NOT EXISTS execution_queue (
    task_id TEXT PRIMARY KEY REFERENCES tasks(id),
    state TEXT NOT NULL CHECK(state IN ('pending','claimed','done')),
    requested_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS execution_queue_pending ON execution_queue(state,requested_at);
CREATE TABLE IF NOT EXISTS tool_calls (
    task_id TEXT NOT NULL REFERENCES tasks(id),
    call_id TEXT NOT NULL,
    name TEXT NOT NULL,
    arguments TEXT NOT NULL,
    result TEXT,
    PRIMARY KEY(task_id,call_id)
);
