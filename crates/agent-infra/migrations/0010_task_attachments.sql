CREATE TABLE IF NOT EXISTS task_attachments (
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    file_id TEXT NOT NULL,
    metadata TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (task_id, file_id)
);
CREATE INDEX IF NOT EXISTS task_attachments_task ON task_attachments(task_id);
