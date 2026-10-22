use crate::TaskStore;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use yukii_agent_ports::{
    knowledge::{IndexedChunk, KnowledgeRepository},
    repository::RepositoryError,
};
use yukii_contracts::knowledge::{KnowledgeDocument, KnowledgeHit};

fn database_error(_: rusqlite::Error) -> RepositoryError {
    RepositoryError::Backend
}

impl KnowledgeRepository for TaskStore {
    fn import_document(
        &self,
        title: &str,
        chunks: &[IndexedChunk],
        fingerprint: &str,
    ) -> Result<KnowledgeDocument, RepositoryError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| RepositoryError::Backend)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let existing = transaction
            .query_row(
                "SELECT id,title,chunk_count,created_at FROM knowledge_documents WHERE fingerprint=?1",
                [fingerprint],
                read_document,
            )
            .optional()
            .map_err(database_error)?;
        if let Some(document) = existing {
            return Ok(document);
        }
        let count: i64 = transaction
            .query_row("SELECT COUNT(*) FROM knowledge_chunks", [], |row| {
                row.get(0)
            })
            .map_err(database_error)?;
        if count + chunks.len() as i64 > 10000 {
            return Err(RepositoryError::InvalidRequest(
                "知识库已达到当前单实例 10000 个片段上限",
            ));
        }
        let document = KnowledgeDocument {
            id: uuid::Uuid::new_v4().to_string(),
            title: title.into(),
            chunk_count: chunks.len() as u32,
            created_at: crate::sqlite::now(),
        };
        transaction
            .execute(
                "INSERT INTO knowledge_documents(id,title,fingerprint,chunk_count,created_at)
             VALUES(?1,?2,?3,?4,?5)",
                params![
                    document.id,
                    title,
                    fingerprint,
                    chunks.len() as i64,
                    document.created_at
                ],
            )
            .map_err(database_error)?;
        for (ordinal, chunk) in chunks.iter().enumerate() {
            let id = uuid::Uuid::new_v4().to_string();
            transaction.execute(
                "INSERT INTO knowledge_chunks(id,document_id,ordinal,content) VALUES(?1,?2,?3,?4)",
                params![id, document.id, ordinal as i64, chunk.text],
            ).map_err(database_error)?;
            transaction
                .execute(
                    "INSERT INTO knowledge_fts(chunk_id,terms) VALUES(?1,?2)",
                    params![id, chunk.terms],
                )
                .map_err(database_error)?;
        }
        transaction.commit().map_err(database_error)?;
        Ok(document)
    }
    fn documents(&self) -> Result<Vec<KnowledgeDocument>, RepositoryError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| RepositoryError::Backend)?;
        let mut statement = connection.prepare(
            "SELECT id,title,chunk_count,created_at FROM knowledge_documents ORDER BY created_at DESC,rowid DESC",
        ).map_err(database_error)?;
        statement
            .query_map([], read_document)
            .map_err(database_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)
    }
    fn delete_document(&self, id: &str) -> Result<(), RepositoryError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| RepositoryError::Backend)?;
        // 外键级联与触发器在同一删除事务中清理片段和全文索引。
        if connection
            .execute("DELETE FROM knowledge_documents WHERE id=?1", [id])
            .map_err(database_error)?
            == 0
        {
            return Err(RepositoryError::NotFound);
        }
        Ok(())
    }
    fn search(&self, expression: &str, limit: usize) -> Result<Vec<KnowledgeHit>, RepositoryError> {
        if expression.is_empty() {
            return Ok(Vec::new());
        }
        let connection = self
            .connection
            .lock()
            .map_err(|_| RepositoryError::Backend)?;
        let mut statement = connection
            .prepare(
                "SELECT d.id,c.id,d.title,c.content FROM knowledge_fts f
             JOIN knowledge_chunks c ON c.id=f.chunk_id
             JOIN knowledge_documents d ON d.id=c.document_id
             WHERE knowledge_fts MATCH ?1 ORDER BY bm25(knowledge_fts),c.id LIMIT ?2",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map(params![expression, limit.min(32) as i64], |row| {
                Ok(KnowledgeHit {
                    document_id: row.get(0)?,
                    chunk_id: row.get(1)?,
                    title: row.get(2)?,
                    text: row.get(3)?,
                    citation: String::new(),
                })
            })
            .map_err(database_error)?;
        rows.enumerate()
            .map(|(index, row)| {
                let mut hit = row.map_err(database_error)?;
                hit.citation = format!("K{}", index + 1);
                Ok(hit)
            })
            .collect()
    }
}
fn read_document(row: &rusqlite::Row<'_>) -> rusqlite::Result<KnowledgeDocument> {
    Ok(KnowledgeDocument {
        id: row.get(0)?,
        title: row.get(1)?,
        chunk_count: row.get(2)?,
        created_at: row.get(3)?,
    })
}
