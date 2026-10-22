CREATE TABLE IF NOT EXISTS subagent_runs (
    id TEXT PRIMARY KEY,
    parent_task_id TEXT NOT NULL,
    parent_call_id TEXT NOT NULL,
    prompt TEXT NOT NULL,
    status TEXT NOT NULL,
    result TEXT,
    error TEXT,
    started_at INTEGER NOT NULL,
    finished_at INTEGER
);
CREATE INDEX IF NOT EXISTS subagent_runs_parent ON subagent_runs(parent_task_id, started_at);
