-- 向量按端点/模型/维度/预处理配置隔离。原文删除时向量及失败记录随外键回收。
CREATE TABLE embedding_profiles (
    id TEXT PRIMARY KEY,
    model TEXT NOT NULL,
    dimensions INTEGER NOT NULL CHECK(dimensions BETWEEN 1 AND 4096)
);
CREATE TABLE knowledge_vectors (
    profile_id TEXT NOT NULL REFERENCES embedding_profiles(id),
    chunk_id TEXT NOT NULL REFERENCES knowledge_chunks(id) ON DELETE CASCADE,
    vector BLOB NOT NULL,
    PRIMARY KEY(profile_id,chunk_id)
);
CREATE TABLE embedding_failures (
    profile_id TEXT NOT NULL,
    chunk_id TEXT NOT NULL REFERENCES knowledge_chunks(id) ON DELETE CASCADE,
    error TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(profile_id,chunk_id)
);
