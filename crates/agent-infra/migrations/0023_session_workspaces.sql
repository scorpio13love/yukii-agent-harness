CREATE TABLE IF NOT EXISTS session_workspaces (
    session_id TEXT PRIMARY KEY,
    path TEXT NOT NULL,
    name TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
