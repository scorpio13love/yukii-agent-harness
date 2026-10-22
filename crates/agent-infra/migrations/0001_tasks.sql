-- IF NOT EXISTS 允许已有无版本表的早期数据库平滑登记为 v1。
CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY, payload TEXT NOT NULL, created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id TEXT NOT NULL REFERENCES tasks(id), payload TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS artifacts (
    id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(id),
    metadata TEXT NOT NULL, content TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS artifacts_task_id ON artifacts(task_id);
CREATE INDEX IF NOT EXISTS events_task_id ON events(task_id,sequence);
