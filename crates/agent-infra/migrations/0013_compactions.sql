CREATE TABLE IF NOT EXISTS conversation_compactions (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    through_turn INTEGER NOT NULL,
    summary TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS conversation_compactions_conversation
    ON conversation_compactions(conversation_id, created_at);
