use super::*;
use crate::sessions::archive as query;
use peri_acp_types::context_archive::*;

impl SqliteSessionData {
    pub(super) async fn read_archive_metadata_page(
        &self,
        id: &ThreadId,
        plan: &ArchiveMetadataQuery,
    ) -> SessionResourceResult<ArchiveMetadataPage> {
        query::validate_metadata(plan)?;
        let mut tx = self.database.pool.begin().await.map_err(|e| map_sqlx(&e))?;
        let tail: Option<(String,)> = sqlx::query_as(query::TAIL_SQL)
            .bind(id.as_str())
            .bind(plan.committed_tail.map(|id| id.as_uuid().to_string()))
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| map_sqlx(&e))?;
        let (tail,) = tail.ok_or_else(not_found)?;
        let tail_id = message_id_of(&tail)?;
        let count: (i64,) = sqlx::query_as(query::BOUNDS_SQL)
            .bind(id.as_str())
            .bind(&tail)
            .bind(plan.first_anchor.map(|id| id.as_uuid().to_string()))
            .bind(plan.last_anchor.map(|id| id.as_uuid().to_string()))
            .bind(plan.after.map(|id| id.as_uuid().to_string()))
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| map_sqlx(&e))?;
        if count.0 != query::bound_identity_count(tail_id, plan) {
            return Err(not_found());
        }
        let rows: Vec<(String, Option<String>, i64)> = sqlx::query_as(query::METADATA_SQL)
            .bind(id.as_str())
            .bind(&tail)
            .bind(plan.anchor.map(|id| id.as_uuid().to_string()))
            .bind(plan.first_anchor.map(|id| id.as_uuid().to_string()))
            .bind(plan.last_anchor.map(|id| id.as_uuid().to_string()))
            .bind(plan.after.map(|id| id.as_uuid().to_string()))
            .bind(i64::from(plan.limit) + 1)
            .bind(ARCHIVE_MAX_METADATA_BYTES as i64)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| map_sqlx(&e))?;
        let items = rows
            .into_iter()
            .map(|(id, p, s)| query::decode_metadata(&id, p, s))
            .collect::<SessionResourceResult<Vec<_>>>()?;
        if plan.anchor.is_some() && items.is_empty() {
            return Err(not_found());
        }
        tx.commit().await.map_err(|e| map_sqlx(&e))?;
        Ok(query::page(message_id_of(&tail)?, items, plan.limit))
    }
    pub(super) async fn read_archive_body_chunk(
        &self,
        id: &ThreadId,
        plan: &ArchiveChunkQuery,
    ) -> SessionResourceResult<ArchiveChunk> {
        query::validate_chunk(plan)?;
        let row: Option<(Vec<u8>, i64)> = sqlx::query_as(query::CHUNK_SQL)
            .bind(id.as_str())
            .bind(plan.message_id.as_uuid().to_string())
            .bind(plan.committed_tail.as_uuid().to_string())
            .bind(plan.offset as i64)
            .bind(i64::from(plan.max_bytes))
            .fetch_optional(&self.database.pool)
            .await
            .map_err(|e| map_sqlx(&e))?;
        let (bytes, total) = row.ok_or_else(not_found)?;
        if total < 0 || plan.offset > total as u64 {
            return Err(query::invalid("archive offset exceeds canonical body"));
        }
        Ok(ArchiveChunk {
            bytes,
            total_bytes: total as u64,
        })
    }
}

impl SqliteSessionData {
    pub(super) async fn read_inherited_archive_root(
        &self,
        id: &ThreadId,
        source: &ThreadId,
        anchor: MessageId,
    ) -> SessionResourceResult<ArchiveRoot> {
        let row: Option<(String,)> = sqlx::query_as(query::INHERITED_GRANT_SQL)
            .bind(id.as_str())
            .bind(source.as_str())
            .bind(anchor.as_uuid().to_string())
            .fetch_optional(&self.database.pool)
            .await
            .map_err(|e| map_sqlx(&e))?;
        let (json,) = row.ok_or_else(not_found)?;
        serde_json::from_str(&json).map_err(|_| corrupt("invalid inherited archive grant"))
    }
}
