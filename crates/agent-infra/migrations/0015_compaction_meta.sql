ALTER TABLE conversation_compactions ADD COLUMN trigger TEXT NOT NULL DEFAULT 'manual';
ALTER TABLE conversation_compactions ADD COLUMN retained_turns INTEGER NOT NULL DEFAULT 0;
ALTER TABLE conversation_compactions ADD COLUMN estimated_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE conversation_compactions ADD COLUMN summary_source TEXT NOT NULL DEFAULT 'llm';
