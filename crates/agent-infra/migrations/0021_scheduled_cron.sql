CREATE TABLE IF NOT EXISTS scheduled_cron (
    schedule_id TEXT PRIMARY KEY,
    expression TEXT NOT NULL
);
