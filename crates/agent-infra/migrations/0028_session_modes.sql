CREATE TABLE IF NOT EXISTS session_modes (
    session_id TEXT PRIMARY KEY,
    goal_objective TEXT,
    goal_phase TEXT NOT NULL DEFAULT 'active',
    goal_rounds INTEGER NOT NULL DEFAULT 0,
    goal_max_rounds INTEGER NOT NULL DEFAULT 6,
    plan_active INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
);
