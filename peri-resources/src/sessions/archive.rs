//! Shared, bounded SQL for canonical archive access. No schema or execution state.
use peri_acp_types::{
    context_archive::*,
    messages::MessageId,
    session_resources::{SessionResourceError, SessionResourceErrorKind, SessionResourceResult},
};

pub(crate) const TAIL_SQL: &str = "SELECT message_id FROM messages WHERE thread_id = ?1 AND (?2 IS NULL OR message_id = ?2) ORDER BY rowid DESC LIMIT 1";
pub(crate) const METADATA_SQL: &str = "SELECT message_id,
 CASE WHEN length(CAST(projection AS BLOB)) <= ?8 THEN projection ELSE NULL END,
 coalesce(length(CAST(projection AS BLOB)), 0)
 FROM messages WHERE thread_id = ?1
 AND rowid <= (SELECT rowid FROM messages WHERE thread_id = ?1 AND message_id = ?2)
 AND (?3 IS NULL OR message_id = ?3)
 AND (?4 IS NULL OR rowid >= (SELECT rowid FROM messages WHERE thread_id = ?1 AND message_id = ?4))
 AND (?5 IS NULL OR rowid <= (SELECT rowid FROM messages WHERE thread_id = ?1 AND message_id = ?5))
 AND (?6 IS NULL OR rowid > (SELECT rowid FROM messages WHERE thread_id = ?1 AND message_id = ?6))

 ORDER BY rowid LIMIT ?7";
pub(crate) const CHUNK_SQL: &str =
    "SELECT substr(CAST(content AS BLOB), ?4 + 1, ?5), length(CAST(content AS BLOB))
 FROM messages WHERE thread_id = ?1 AND message_id = ?2
 AND rowid <= (SELECT rowid FROM messages WHERE thread_id = ?1 AND message_id = ?3)";

pub(crate) fn validate_metadata(query: &ArchiveMetadataQuery) -> SessionResourceResult<()> {
    if !(1..=8).contains(&query.limit)
        || (query.anchor.is_some()
            && (query.first_anchor.is_some()
                || query.last_anchor.is_some()
                || query.after.is_some()))
    {
        return Err(invalid("invalid archive metadata bounds"));
    }
    Ok(())
}
pub(crate) fn validate_chunk(query: &ArchiveChunkQuery) -> SessionResourceResult<()> {
    if query.max_bytes == 0
        || query.max_bytes as usize > ARCHIVE_CHUNK_BYTES
        || query.offset > i64::MAX as u64 - ARCHIVE_CHUNK_BYTES as u64
    {
        return Err(invalid("invalid archive chunk bounds"));
    }
    Ok(())
}
pub(crate) fn invalid(detail: &str) -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::InvalidInput {
        detail: detail.into(),
    })
}
pub(crate) fn decode_metadata(
    message_id: &str,
    projection: Option<String>,
    size: i64,
) -> SessionResourceResult<ArchiveMetadataItem> {
    if size < 0 || size as usize > ARCHIVE_MAX_METADATA_BYTES {
        return Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        ));
    }
    let message_id = peri_acp_types::history_page::message_id_of(message_id)?;
    let projection = projection
        .map(|value| serde_json::from_str(&value))
        .transpose()
        .map_err(|_| {
            SessionResourceError::new(SessionResourceErrorKind::Corrupt {
                detail: "invalid archive metadata".into(),
            })
        })?;
    Ok(ArchiveMetadataItem {
        message_id,
        projection,
    })
}
pub(crate) fn page(
    tail: MessageId,
    mut items: Vec<ArchiveMetadataItem>,
    limit: u32,
) -> ArchiveMetadataPage {
    let has_more = items.len() > limit as usize;
    items.truncate(limit as usize);
    let next_after = if has_more {
        items.last().map(|item| item.message_id)
    } else {
        None
    };
    ArchiveMetadataPage {
        committed_tail: tail,
        items,
        next_after,
    }
}

/// Grants are read only from the current session's frozen inherited flags. The
/// requested anchor must be inside the persisted root range and committed tail.
pub(crate) const INHERITED_GRANT_SQL: &str = "SELECT root.value
 FROM threads child, json_each(child.inherited_context, '$.flags') flag,
 json_each(flag.value, '$.projection.archive_roots') root
 JOIN messages anchor ON anchor.thread_id = ?2 AND anchor.message_id = ?3
 JOIN messages first ON first.thread_id = ?2 AND first.message_id = json_extract(root.value, '$.first_anchor')
 JOIN messages last ON last.thread_id = ?2 AND last.message_id = json_extract(root.value, '$.last_anchor')
 JOIN messages tail ON tail.thread_id = ?2 AND tail.message_id = json_extract(root.value, '$.committed_tail')
 WHERE child.id = ?1 AND json_extract(root.value, '$.source_thread_id') = ?2
 AND anchor.rowid BETWEEN first.rowid AND last.rowid AND anchor.rowid <= tail.rowid
 AND length(CAST(root.value AS BLOB)) <= 4096
 ORDER BY tail.rowid DESC, last.rowid DESC, first.rowid ASC LIMIT 1";

/// Validate range identities in the same read transaction as the bounded rows.
pub(crate) const BOUNDS_SQL: &str =
    "SELECT COUNT(*) FROM messages WHERE thread_id = ?1 AND message_id IN (?2, ?3, ?4, ?5)";
pub(crate) fn bound_identity_count(tail: MessageId, plan: &ArchiveMetadataQuery) -> i64 {
    [Some(tail), plan.first_anchor, plan.last_anchor, plan.after]
        .into_iter()
        .flatten()
        .collect::<std::collections::HashSet<_>>()
        .len() as i64
}
