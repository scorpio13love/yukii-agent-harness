//! 本机知识库使用 f32 小端向量和精确检索；不依赖 SQLite 扩展或近似索引。
use crate::TaskStore;
use rusqlite::{TransactionBehavior, params};
use yukii_agent_ports::{
    embedding::{EmbeddingProfile, normalize},
    knowledge::{EmbeddingChunk, IndexCounts, StoredVector, VectorRepository},
    repository::RepositoryError,
};
use yukii_contracts::knowledge::KnowledgeHit;

fn db(_: rusqlite::Error) -> RepositoryError {
    RepositoryError::Backend
}

impl VectorRepository for TaskStore {
    fn index_counts(&self, profile: &str) -> Result<IndexCounts, RepositoryError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| RepositoryError::Backend)?;
        connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM knowledge_chunks),
             (SELECT COUNT(*) FROM knowledge_vectors WHERE profile_id=?1),
             (SELECT COUNT(*) FROM embedding_failures WHERE profile_id=?1)",
                [profile],
                |row| {
                    Ok(IndexCounts {
                        total: row.get::<_, i64>(0)? as usize,
                        indexed: row.get::<_, i64>(1)? as usize,
                        failed: row.get::<_, i64>(2)? as usize,
                    })
                },
            )
            .map_err(db)
    }
    fn pending_embeddings(
        &self,
        profile: &str,
        limit: usize,
    ) -> Result<Vec<EmbeddingChunk>, RepositoryError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| RepositoryError::Backend)?;
        let mut statement = connection.prepare(
            "SELECT c.id,d.title,c.content FROM knowledge_chunks c JOIN knowledge_documents d ON d.id=c.document_id
             WHERE NOT EXISTS(SELECT 1 FROM knowledge_vectors v WHERE v.chunk_id=c.id AND v.profile_id=?1)
             AND NOT EXISTS(SELECT 1 FROM embedding_failures f WHERE f.chunk_id=c.id AND f.profile_id=?1)
             ORDER BY d.created_at,d.id,c.ordinal LIMIT ?2",
        ).map_err(db)?;
        statement
            .query_map(params![profile, limit.min(10) as i64], |row| {
                Ok(EmbeddingChunk {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    text: row.get(2)?,
                })
            })
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)
    }
    fn save_embeddings(
        &self,
        profile: &EmbeddingProfile,
        chunks: &[EmbeddingChunk],
        vectors: &[Vec<f32>],
    ) -> Result<(), RepositoryError> {
        if chunks.len() != vectors.len() {
            return Err(RepositoryError::Backend);
        }
        let encoded = vectors
            .iter()
            .map(|vector| {
                normalize(vector, profile.dimensions).map(|normalized| {
                    normalized
                        .iter()
                        .flat_map(|v| v.to_le_bytes())
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| RepositoryError::Backend)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db)?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO embedding_profiles(id,model,dimensions) VALUES(?1,?2,?3)",
                params![profile.id, profile.model, profile.dimensions as i64],
            )
            .map_err(db)?;
        let stored: (String, usize) = transaction
            .query_row(
                "SELECT model,dimensions FROM embedding_profiles WHERE id=?1",
                [&profile.id],
                |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as usize)),
            )
            .map_err(db)?;
        if stored != (profile.model.clone(), profile.dimensions) {
            return Err(RepositoryError::Backend);
        }
        for (chunk, bytes) in chunks.iter().zip(encoded) {
            // 网络调用期间原文可能已被删除；条件写入防止复活删除资料或产生孤立向量。
            transaction
                .execute(
                    "INSERT OR IGNORE INTO knowledge_vectors(profile_id,chunk_id,vector)
                 SELECT ?1,id,?3 FROM knowledge_chunks WHERE id=?2",
                    params![profile.id, chunk.id, bytes],
                )
                .map_err(db)?;
            transaction
                .execute(
                    "DELETE FROM embedding_failures WHERE profile_id=?1 AND chunk_id=?2",
                    params![profile.id, chunk.id],
                )
                .map_err(db)?;
        }
        transaction.commit().map_err(db)
    }
    fn fail_embeddings(
        &self,
        profile: &str,
        chunks: &[EmbeddingChunk],
        error: &str,
    ) -> Result<(), RepositoryError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| RepositoryError::Backend)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db)?;
        for chunk in chunks {
            transaction.execute(
                "INSERT OR REPLACE INTO embedding_failures(profile_id,chunk_id,error,created_at)
                 SELECT ?1,id,?3,?4 FROM knowledge_chunks WHERE id=?2
                 AND NOT EXISTS(SELECT 1 FROM knowledge_vectors WHERE profile_id=?1 AND chunk_id=?2)",
                params![profile,chunk.id,error,crate::sqlite::now()],
            ).map_err(db)?;
        }
        transaction.commit().map_err(db)
    }
    fn retry_embeddings(&self, profile: &str) -> Result<(), RepositoryError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| RepositoryError::Backend)?;
        connection
            .execute(
                "DELETE FROM embedding_failures WHERE profile_id=?1",
                [profile],
            )
            .map_err(db)?;
        Ok(())
    }
    fn vectors(&self, profile: &EmbeddingProfile) -> Result<Vec<StoredVector>, RepositoryError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| RepositoryError::Backend)?;
        let mut statement = connection.prepare(
            "SELECT d.id,c.id,d.title,c.content,v.vector FROM knowledge_vectors v
             JOIN knowledge_chunks c ON c.id=v.chunk_id JOIN knowledge_documents d ON d.id=c.document_id
             WHERE v.profile_id=?1 ORDER BY c.id",
        ).map_err(db)?;
        let rows = statement
            .query_map([&profile.id], |row| {
                let bytes: Vec<u8> = row.get(4)?;
                Ok((
                    KnowledgeHit {
                        document_id: row.get(0)?,
                        chunk_id: row.get(1)?,
                        title: row.get(2)?,
                        text: row.get(3)?,
                        citation: String::new(),
                    },
                    bytes,
                ))
            })
            .map_err(db)?;
        rows.map(|row| {
            let (source, bytes) = row.map_err(db)?;
            if bytes.len() != profile.dimensions * 4 {
                return Err(RepositoryError::Backend);
            }
            let vector = bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|v| f32::from_le_bytes(*v))
                .collect::<Vec<_>>();
            if vector.iter().any(|v| !v.is_finite()) {
                return Err(RepositoryError::Backend);
            }
            Ok(StoredVector { source, vector })
        })
        .collect()
    }
}
