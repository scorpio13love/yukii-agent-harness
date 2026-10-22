CREATE TABLE knowledge_documents (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    fingerprint TEXT NOT NULL UNIQUE,
    chunk_count INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE knowledge_chunks (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES knowledge_documents(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    content TEXT NOT NULL
);
CREATE INDEX knowledge_chunks_document ON knowledge_chunks(document_id,ordinal);
CREATE VIRTUAL TABLE knowledge_fts USING fts5(chunk_id UNINDEXED, terms, tokenize='unicode61');
CREATE TRIGGER knowledge_chunk_delete AFTER DELETE ON knowledge_chunks BEGIN
    DELETE FROM knowledge_fts WHERE chunk_id=old.id;
END;
CREATE TABLE installed_plugins (
    id TEXT PRIMARY KEY,
    manifest TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    source TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0,1)),
    installed_at INTEGER NOT NULL
);
CREATE TABLE plugin_audit (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    plugin_id TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    operation TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
