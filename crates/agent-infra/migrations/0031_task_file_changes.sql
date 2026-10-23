CREATE TABLE IF NOT EXISTS task_file_changes (
    task_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    path TEXT NOT NULL,
    old_text TEXT NOT NULL,
    new_text TEXT NOT NULL,
    diff TEXT NOT NULL,
    added INTEGER NOT NULL DEFAULT 0,
    removed INTEGER NOT NULL DEFAULT 0,
    change_type TEXT NOT NULL DEFAULT 'modify',
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (task_id, path)
);
