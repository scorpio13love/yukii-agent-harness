CREATE TABLE IF NOT EXISTS tool_call_images (
    task_id TEXT NOT NULL,
    call_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    media_type TEXT NOT NULL,
    base64 TEXT NOT NULL,
    width INTEGER NOT NULL DEFAULT 0,
    height INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(task_id, call_id, ordinal)
);
