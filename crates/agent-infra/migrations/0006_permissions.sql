-- scope 与工具参数快照由受信宿主提供/仓储读取，不接受模型自授予权限。
CREATE TABLE permissions (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    call_id TEXT NOT NULL,
    binding TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    arguments TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('pending','approved','denied','claimed','revoked')),
    claim_id TEXT UNIQUE,
    FOREIGN KEY(task_id,call_id) REFERENCES tool_calls(task_id,call_id),
    UNIQUE(task_id,call_id)
);
CREATE TABLE permission_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    permission_id TEXT NOT NULL REFERENCES permissions(id),
    state TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
