use super::{
    mutation::sole_row,
    session_data::{not_found, RemoteSessionData},
    sql::{int_at, text_at, StatementSpec},
};
use crate::sessions::archive as query;
use peri_acp_types::{
    context_archive::*, history_page::message_id_of, session_resources::SessionResourceResult,
    thread::ThreadId,
};
use turso_serverless::Value;

fn optional_id(id: Option<peri_acp_types::messages::MessageId>) -> Value {
    id.map(|id| Value::Text(id.as_uuid().to_string()))
        .unwrap_or(Value::Null)
}

impl RemoteSessionData {
    pub(super) async fn read_archive_metadata_page(
        &self,
        id: &ThreadId,
        plan: &ArchiveMetadataQuery,
    ) -> SessionResourceResult<ArchiveMetadataPage> {
        query::validate_metadata(plan)?;
        let store = self.store().await?;
        let tail_rows = store
            .read_batch(vec![StatementSpec::new(
                query::TAIL_SQL,
                vec![
                    Value::Text(id.to_string()),
                    optional_id(plan.committed_tail),
                ],
            )])
            .await?;
        let tail_row =
            sole_row(tail_rows.into_iter().next().ok_or_else(not_found)?)?.ok_or_else(not_found)?;
        let tail = text_at(&tail_row, 0).ok_or_else(not_found)?.to_owned();
        let tail_id = message_id_of(&tail)?;
        let (bounds, rows) = store
            .read_pair(
                StatementSpec::new(
                    query::BOUNDS_SQL,
                    vec![
                        Value::Text(id.clone()),
                        Value::Text(tail.clone()),
                        optional_id(plan.first_anchor),
                        optional_id(plan.last_anchor),
                        optional_id(plan.after),
                    ],
                ),
                StatementSpec::new(
                    query::METADATA_SQL,
                    vec![
                        Value::Text(id.clone()),
                        Value::Text(tail.clone()),
                        optional_id(plan.anchor),
                        optional_id(plan.first_anchor),
                        optional_id(plan.last_anchor),
                        optional_id(plan.after),
                        Value::Integer(i64::from(plan.limit) + 1),
                        Value::Integer(ARCHIVE_MAX_METADATA_BYTES as i64),
                    ],
                ),
            )
            .await?;
        let bound = sole_row(bounds)?.ok_or_else(not_found)?;
        if int_at(&bound, 0) != Some(query::bound_identity_count(tail_id, plan)) {
            return Err(not_found());
        }
        let items = rows
            .iter()
            .map(|row| {
                query::decode_metadata(
                    text_at(row, 0).ok_or_else(not_found)?,
                    text_at(row, 1).map(str::to_owned),
                    int_at(row, 2).ok_or_else(not_found)?,
                )
            })
            .collect::<SessionResourceResult<Vec<_>>>()?;
        if plan.anchor.is_some() && items.is_empty() {
            return Err(not_found());
        }
        Ok(query::page(message_id_of(&tail)?, items, plan.limit))
    }
    pub(super) async fn read_archive_body_chunk(
        &self,
        id: &ThreadId,
        plan: &ArchiveChunkQuery,
    ) -> SessionResourceResult<ArchiveChunk> {
        query::validate_chunk(plan)?;
        let batch = ArchiveChunkBatch::new(vec![plan.clone()])
            .map_err(|_| query::invalid("invalid archive chunk bounds"))?;
        self.read_archive_body_chunks(id, &batch)
            .await?
            .pop()
            .ok_or_else(not_found)
    }

    pub(super) async fn read_archive_body_chunks(
        &self,
        id: &ThreadId,
        batch: &ArchiveChunkBatch,
    ) -> SessionResourceResult<Vec<ArchiveChunk>> {
        let statements = batch
            .queries()
            .iter()
            .map(|plan| {
                StatementSpec::new(
                    query::CHUNK_SQL,
                    vec![
                        Value::Text(id.to_string()),
                        optional_id(Some(plan.message_id)),
                        optional_id(Some(plan.committed_tail)),
                        Value::Integer(plan.offset as i64),
                        Value::Integer(i64::from(plan.max_bytes)),
                    ],
                )
            })
            .collect();
        let store = self.store().await?;
        let rows = store.read_batch(statements).await?;
        rows.into_iter()
            .zip(batch.queries())
            .map(|(rows, plan)| {
                let row = sole_row(rows)?.ok_or_else(not_found)?;
                let total = int_at(&row, 1).ok_or_else(not_found)?;
                if total < 0 || plan.offset > total as u64 {
                    return Err(query::invalid("archive offset exceeds canonical body"));
                }
                let bytes = match row.into_iter().next() {
                    Some(Value::Blob(bytes)) if bytes.len() <= plan.max_bytes as usize => bytes,
                    _ => return Err(query::invalid("archive body chunk exceeds its byte bound")),
                };
                Ok(ArchiveChunk {
                    bytes,
                    total_bytes: total as u64,
                })
            })
            .collect()
    }
}

impl RemoteSessionData {
    pub(super) async fn read_inherited_archive_root(
        &self,
        id: &ThreadId,
        source: &ThreadId,
        anchor: peri_acp_types::messages::MessageId,
    ) -> SessionResourceResult<ArchiveRoot> {
        let store = self.store().await?;
        let rows = store
            .read_batch(vec![StatementSpec::new(
                query::INHERITED_GRANT_SQL,
                vec![
                    Value::Text(id.to_owned()),
                    Value::Text(source.to_owned()),
                    optional_id(Some(anchor)),
                ],
            )])
            .await?;
        let row =
            sole_row(rows.into_iter().next().ok_or_else(not_found)?)?.ok_or_else(not_found)?;
        serde_json::from_str(text_at(&row, 0).ok_or_else(not_found)?)
            .map_err(|_| query::invalid("invalid inherited archive grant"))
    }
}
