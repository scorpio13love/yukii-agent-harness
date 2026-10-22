CREATE TABLE IF NOT EXISTS scheduled_tasks (
    id TEXT PRIMARY KEY,
    conversation_id TEXT,
    prompt TEXT NOT NULL,
    target TEXT NOT NULL,
    run_at INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    task_id TEXT,
    error TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS scheduled_tasks_due ON scheduled_tasks(status, run_at);
