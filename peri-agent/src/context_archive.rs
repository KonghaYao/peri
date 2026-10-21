//! Session-bound, read-only canonical recovery. The service never holds a transcript.
use async_trait::async_trait;
use peri_acp_types::{
    context_archive::*,
    messages::MessageId,
    session_resources::{SessionResourceError, SessionResourceErrorKind, SessionResources},
    thread::ThreadId,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub struct ContextArchiveReader {
    resources: Arc<dyn SessionResources>,
    session_id: ThreadId,
}
impl ContextArchiveReader {
    pub fn new(resources: Arc<dyn SessionResources>, session_id: ThreadId) -> Self {
        Self {
            resources,
            session_id,
        }
    }

    /// Confirm every byte needed for strong eviction through the same bounded recovery port.
    /// Call after the canonical persistence barrier, before the atomic projection commit.
    pub async fn verify_catalog(
        &self,
        catalog: &ArchiveCatalog,
    ) -> Result<MessageId, ArchiveError> {
        catalog.validate()?;
        if catalog.source_thread_id != self.session_id {
            return Err(ArchiveError::Forbidden);
        }
        let page = self
            .resources
            .read_archive_metadata(
                &self.session_id,
                &ArchiveMetadataQuery {
                    anchor: Some(catalog.anchor_message_id),
                    limit: 1,
                    ..Default::default()
                },
            )
            .await
            .map_err(map_error)?;
        for member in &catalog.members {
            for (index, digest) in member.chunk_digests.iter().enumerate() {
                let chunk = self
                    .resources
                    .read_archive_chunk(
                        &self.session_id,
                        &ArchiveChunkQuery {
                            message_id: member.message_id,
                            committed_tail: page.committed_tail,
                            offset: (index * ARCHIVE_CHUNK_BYTES) as u64,
                            max_bytes: ARCHIVE_CHUNK_BYTES as u32,
                        },
                    )
                    .await
                    .map_err(map_error)?;
                if chunk.total_bytes != member.canonical_bytes
                    || content_digest(&chunk.bytes) != *digest
                {
                    return Err(ArchiveError::ContentMismatch);
                }
            }
        }
        Ok(page.committed_tail)
    }
    async fn authorize(
        &self,
        source: &ThreadId,
        anchor: MessageId,
    ) -> Result<Option<ArchiveRoot>, ArchiveError> {
        if source == &self.session_id {
            return Ok(None);
        }
        let root = self
            .resources
            .inherited_archive_root(&self.session_id, source, anchor)
            .await
            .map_err(|error| match error.kind() {
                SessionResourceErrorKind::NotFound => ArchiveError::Forbidden,
                _ => map_error(error),
            })?;
        if &root.source_thread_id != source {
            return Err(ArchiveError::Forbidden);
        }
        Ok(Some(root))
    }
    async fn catalog(
        &self,
        source: &ThreadId,
        anchor: MessageId,
        digest: &str,
        tail: Option<MessageId>,
    ) -> Result<(ArchiveCatalog, MessageId), ArchiveError> {
        let grant_tail = self
            .authorize(source, anchor)
            .await?
            .map(|root| root.committed_tail);
        if grant_tail.is_some() && tail.is_some() && grant_tail != tail {
            return Err(ArchiveError::Forbidden);
        }
        let page = self
            .resources
            .read_archive_metadata(
                source,
                &ArchiveMetadataQuery {
                    anchor: Some(anchor),
                    committed_tail: grant_tail.or(tail),
                    limit: 1,
                    ..Default::default()
                },
            )
            .await
            .map_err(map_error)?;
        let catalog = page
            .items
            .into_iter()
            .next()
            .and_then(|item| item.projection)
            .and_then(|p| p.archive_catalog)
            .ok_or(ArchiveError::NotFound)?;
        if &catalog.source_thread_id != source
            || catalog.anchor_message_id != anchor
            || catalog.digest()? != digest
        {
            return Err(ArchiveError::ContentMismatch);
        }
        Ok((catalog, page.committed_tail))
    }
}

#[derive(Serialize, Deserialize, Default)]
struct Cursor {
    reference_digest: String,
    member: usize,
    offset: u64,
    after: Option<MessageId>,
    tail: Option<MessageId>,
}
impl Cursor {
    fn parse(request: &ArchiveReadRequest) -> Result<Self, ArchiveError> {
        let mut cursor: Self = if let Some(raw) = &request.cursor {
            if raw.len() > 2048 {
                return Err(ArchiveError::InvalidRequest("cursor too long".into()));
            }
            serde_json::from_str(raw)
                .map_err(|_| ArchiveError::InvalidRequest("invalid archive cursor".into()))?
        } else {
            Self::default()
        };
        let digest = content_digest(request.archive_ref.as_bytes());
        if !cursor.reference_digest.is_empty() && cursor.reference_digest != digest {
            return Err(ArchiveError::ContentMismatch);
        }
        cursor.reference_digest = digest;
        Ok(cursor)
    }
    fn encode(&self) -> Result<String, ArchiveError> {
        serde_json::to_string(self).map_err(|_| ArchiveError::ContentMismatch)
    }
}

#[async_trait]
impl ContextArchiveReadPort for ContextArchiveReader {
    async fn read(&self, request: ArchiveReadRequest) -> Result<ArchiveReadResponse, ArchiveError> {
        let reference = ArchiveReference::decode(&request.archive_ref)?;

        let tokens = request.budget_tokens.unwrap_or(ARCHIVE_DEFAULT_TOKENS);
        if !(256..=ARCHIVE_MAX_TOKENS).contains(&tokens) {
            return Err(ArchiveError::InvalidRequest(
                "budget_tokens must be 256..4096 (metadata and cursor included)".into(),
            ));
        }
        let mut cursor = Cursor::parse(&request)?;
        let mut result = ArchiveReadResponse {
            archive_ref: request.archive_ref.clone(),
            complete: false,
            next_cursor: None,
            entries: Vec::new(),
        };
        match reference {
            ArchiveReference::Range { root } => {
                if cursor.tail.is_some_and(|tail| tail != root.committed_tail) {
                    return Err(ArchiveError::ContentMismatch);
                }
                let grant = self
                    .authorize(&root.source_thread_id, root.first_anchor)
                    .await?;
                let end_grant = self
                    .authorize(&root.source_thread_id, root.last_anchor)
                    .await?;
                if grant != end_grant {
                    return Err(ArchiveError::Forbidden);
                }
                if let Some(grant) = grant {
                    // A previously published narrower root remains readable under a later,
                    // frozen superset grant. Validate its fixed tail below the grant's tail.
                    self.resources
                        .read_archive_metadata(
                            &root.source_thread_id,
                            &ArchiveMetadataQuery {
                                anchor: Some(root.committed_tail),
                                committed_tail: Some(grant.committed_tail),
                                limit: 1,
                                ..Default::default()
                            },
                        )
                        .await
                        .map_err(map_error)?;
                }
                let page = self
                    .resources
                    .read_archive_metadata(
                        &root.source_thread_id,
                        &ArchiveMetadataQuery {
                            first_anchor: Some(root.first_anchor),
                            last_anchor: Some(root.last_anchor),
                            after: cursor.after,
                            committed_tail: Some(root.committed_tail),
                            limit: 8,
                            anchor: None,
                        },
                    )
                    .await
                    .map_err(map_error)?;
                let mut next_after = page.next_after;
                let mut items = page.items.into_iter().peekable();
                while let Some(item) = items.next() {
                    let Some(catalog) = item.projection.and_then(|p| p.archive_catalog) else {
                        continue;
                    };
                    if catalog.source_thread_id != root.source_thread_id {
                        return Err(ArchiveError::ContentMismatch);
                    }
                    result.entries.push(ArchiveReadEntry {
                        message_id: item.message_id,
                        content_digest: catalog.digest()?,
                        kind: "archive_catalog".into(),
                        block_types: Vec::new(),
                        archive_ref: catalog.reference()?,
                        offset: 0,
                        total_bytes: catalog.members.len() as u64,
                        data: String::new(),
                        encoding: "catalog".into(),
                        complete: false,
                    });
                    if items.peek().is_some() {
                        next_after = Some(item.message_id);
                    }
                    break;
                }
                cursor.after = next_after;
                cursor.tail = Some(root.committed_tail);
                result.complete = next_after.is_none();
            }
            ArchiveReference::Catalog {
                source_thread_id,
                anchor_message_id,
                digest,
                member,
            } => {
                let (catalog, tail) = self
                    .catalog(&source_thread_id, anchor_message_id, &digest, cursor.tail)
                    .await?;
                cursor.tail = Some(tail);
                let index = member.unwrap_or(cursor.member);
                let source = catalog.members.get(index).ok_or(ArchiveError::NotFound)?;
                if cursor.offset >= source.canonical_bytes {
                    return Err(ArchiveError::InvalidRequest(
                        "cursor exceeds archived body".into(),
                    ));
                }
                let chunk_index = cursor.offset as usize / ARCHIVE_CHUNK_BYTES;
                let chunk_offset = chunk_index * ARCHIVE_CHUNK_BYTES;
                let chunk = self
                    .resources
                    .read_archive_chunk(
                        &source_thread_id,
                        &ArchiveChunkQuery {
                            message_id: source.message_id,
                            committed_tail: tail,
                            offset: chunk_offset as u64,
                            max_bytes: ARCHIVE_CHUNK_BYTES as u32,
                        },
                    )
                    .await
                    .map_err(map_error)?;
                if chunk.total_bytes != source.canonical_bytes
                    || content_digest(&chunk.bytes) != source.chunk_digests[chunk_index]
                {
                    return Err(ArchiveError::ContentMismatch);
                }
                let start = cursor.offset as usize - chunk_offset;
                if start >= chunk.bytes.len() {
                    return Err(ArchiveError::ContentMismatch);
                }
                let available = &chunk.bytes[start..];
                // Leave space for stable identities, digest and continuation. Escape expansion is
                // included by the final serialized-size check below.
                let max_data = (tokens as usize * 4).saturating_sub(1200).max(16);
                let mut end = max_data.min(available.len());
                while end > 0
                    && (std::str::from_utf8(&available[..end]).is_err()
                        || std::str::from_utf8(&available[..end])
                            .ok()
                            .and_then(|text| serde_json::to_string(text).ok())
                            .is_some_and(|json| json.len() > max_data))
                {
                    end -= 1;
                }
                let (data, encoding, consumed) = if end > 0 {
                    (
                        std::str::from_utf8(&available[..end])
                            .map_err(|_| ArchiveError::ContentMismatch)?
                            .to_owned(),
                        "utf8",
                        end,
                    )
                } else {
                    let count = max_data.min(4).min(available.len());
                    (
                        available[..count]
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect(),
                        "hex",
                        count,
                    )
                };
                let offset = cursor.offset;
                cursor.offset += consumed as u64;
                let finished = cursor.offset == source.canonical_bytes;
                result.entries.push(ArchiveReadEntry {
                    message_id: source.message_id,
                    content_digest: source.content_digest.clone(),
                    kind: source.kind.clone(),
                    block_types: source.block_types.clone(),
                    archive_ref: catalog.member_ref(index)?,
                    offset,
                    total_bytes: source.canonical_bytes,
                    data,
                    encoding: encoding.into(),
                    complete: offset == 0 && finished,
                });
                result.complete =
                    finished && (member.is_some() || index + 1 == catalog.members.len());
                if finished && !result.complete {
                    cursor.member = index + 1;
                    cursor.offset = 0;
                }
            }
        }
        if !result.complete {
            result.next_cursor = Some(cursor.encode()?);
        }
        let bytes = serde_json::to_vec(&result).map_err(|_| ArchiveError::ContentMismatch)?;
        if bytes.len() > tokens as usize * 4 {
            return Err(ArchiveError::InvalidRequest(
                "return budget too small for archive metadata/fragment; request a larger budget"
                    .into(),
            ));
        }
        Ok(result)
    }
}

fn map_error(error: SessionResourceError) -> ArchiveError {
    match error.kind() {
        SessionResourceErrorKind::NotFound => ArchiveError::NotFound,
        SessionResourceErrorKind::Unsupported => ArchiveError::Unsupported,
        SessionResourceErrorKind::Corrupt { .. } => ArchiveError::ContentMismatch,
        _ => ArchiveError::Unavailable(error.to_string()),
    }
}

#[cfg(test)]
#[path = "context_archive_test.rs"]
mod tests;
