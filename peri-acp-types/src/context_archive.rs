//! Durable context archive identities. References locate data; they never grant access.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

use crate::{
    messages::MessageId,
    projection::MessageProjectionDirective,
    store::{serialize_persisted_payload, PersistedPayload},
    thread::ThreadId,
};

#[path = "context_archive/batch.rs"]
mod batch;
pub use batch::{ArchiveChunkBatch, ARCHIVE_CHUNKS_PER_BATCH};

pub const ARCHIVE_FORMAT_VERSION: u32 = 1;
pub const ARCHIVE_CHUNK_BYTES: usize = 4096;
pub const ARCHIVE_MAX_METADATA_BYTES: usize = 65_536;
pub const ARCHIVE_MAX_MEMBERS: usize = 32;
pub const ARCHIVE_DEFAULT_TOKENS: u32 = 2048;
pub const ARCHIVE_MAX_TOKENS: u32 = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveMember {
    pub message_id: MessageId,
    pub content_digest: String,
    pub canonical_bytes: u64,
    pub chunk_digests: Vec<String>,
    /// The serialized canonical envelope preserves all block types and tool inputs/results.
    pub kind: String,
    /// Distinct block kinds present; canonical bytes retain per-block order and detail.
    #[serde(default)]
    pub block_types: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveCatalog {
    pub format_version: u32,
    pub group_id: String,
    pub source_thread_id: ThreadId,
    pub anchor_message_id: MessageId,
    pub members: Vec<ArchiveMember>,
    pub receipt: String,
    #[serde(default)]
    pub dependencies: Vec<MessageId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveRoot {
    pub source_thread_id: ThreadId,
    pub first_anchor: MessageId,
    pub last_anchor: MessageId,
    /// Stable ID, resolved inside the scoped read transaction; never a reused row position.
    pub committed_tail: MessageId,
}

/// Merge ranges by canonical history order, including earlier anchors discovered by
/// a later projection upgrade. Endpoints released by Full are an older prefix.
pub fn coalesce_archive_roots(
    roots: Vec<ArchiveRoot>,
    history_ids: &[MessageId],
) -> Vec<ArchiveRoot> {
    let positions: HashMap<_, _> = history_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, i))
        .collect();
    let earliest = |old: MessageId, new: MessageId| match (positions.get(&old), positions.get(&new))
    {
        (Some(old_pos), Some(new_pos)) if new_pos < old_pos => new,
        (Some(_), None) => new,
        _ => old,
    };
    let latest = |old: MessageId, new: MessageId| match (positions.get(&old), positions.get(&new)) {
        (Some(old_pos), Some(new_pos)) if new_pos > old_pos => new,
        (None, Some(_)) | (None, None) => new,
        _ => old,
    };
    let orderable = |a: MessageId, b: MessageId| {
        a == b || positions.contains_key(&a) || positions.contains_key(&b)
    };
    let mut merged: Vec<ArchiveRoot> = Vec::new();
    for root in roots {
        if let Some(existing) = merged.iter_mut().find(|old| {
            old.source_thread_id == root.source_thread_id
                && orderable(old.first_anchor, root.first_anchor)
                && orderable(old.last_anchor, root.last_anchor)
                && orderable(old.committed_tail, root.committed_tail)
        }) {
            existing.first_anchor = earliest(existing.first_anchor, root.first_anchor);
            existing.last_anchor = latest(existing.last_anchor, root.last_anchor);
            existing.committed_tail = latest(existing.committed_tail, root.committed_tail);
        } else {
            merged.push(root);
        }
    }
    merged
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArchiveReference {
    Catalog {
        source_thread_id: ThreadId,
        anchor_message_id: MessageId,
        digest: String,
        member: Option<usize>,
    },
    Range {
        root: ArchiveRoot,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("archive_invalid_request: {0}")]
    InvalidRequest(String),
    #[error("archive_forbidden: reference is outside the current session's authorized history")]
    Forbidden,
    #[error("archive_not_found: archived content is no longer retained")]
    NotFound,
    #[error("archive_content_mismatch: canonical content or catalog identity changed")]
    ContentMismatch,
    #[error("archive_unsupported: bounded archive recovery is unavailable")]
    Unsupported,
    #[error("archive_unavailable: {0}")]
    Unavailable(String),
}

/// The bounded metadata reader returns the complete persisted directive, so its
/// entries, group links and roots share the catalog's single byte budget.
pub fn validate_projection_metadata(
    directive: &MessageProjectionDirective,
) -> Result<(), ArchiveError> {
    let bytes = serde_json::to_vec(directive).map_err(|_| ArchiveError::ContentMismatch)?;
    if bytes.len() > ARCHIVE_MAX_METADATA_BYTES {
        return Err(ArchiveError::Unsupported);
    }
    Ok(())
}

pub fn content_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn build_catalog(
    source_thread_id: ThreadId,
    group_id: String,
    anchor_message_id: MessageId,
    payloads: &[PersistedPayload],
    receipt: String,
) -> Result<ArchiveCatalog, ArchiveError> {
    if payloads.is_empty()
        || payloads.len() > ARCHIVE_MAX_MEMBERS
        || !payloads.iter().any(|p| p.id() == anchor_message_id)
    {
        return Err(ArchiveError::InvalidRequest(
            "archive group size/anchor is invalid".into(),
        ));
    }
    let members = payloads
        .iter()
        .map(|payload| {
            if let PersistedPayload::Message(message) = payload {
                if !recoverable_content(message.message_content()) {
                    return Err(ArchiveError::Unsupported);
                }
            }
            let encoded = serialize_persisted_payload(payload)
                .map_err(|e| ArchiveError::Unavailable(e.to_string()))?;
            let bytes = encoded.as_bytes();
            Ok(ArchiveMember {
                message_id: payload.id(),
                content_digest: content_digest(bytes),
                canonical_bytes: bytes.len() as u64,
                chunk_digests: bytes
                    .chunks(ARCHIVE_CHUNK_BYTES)
                    .map(content_digest)
                    .collect(),
                block_types: match payload {
                    PersistedPayload::Message(message) => block_types(message.message_content()),
                    PersistedPayload::SystemReminder { .. } => vec!["system_reminder".into()],
                },
                kind: match payload {
                    PersistedPayload::Message(crate::messages::BaseMessage::Ai { .. }) => {
                        "assistant"
                    }
                    PersistedPayload::Message(crate::messages::BaseMessage::Tool { .. }) => "tool",
                    PersistedPayload::Message(crate::messages::BaseMessage::Human { .. }) => "user",
                    PersistedPayload::Message(crate::messages::BaseMessage::System { .. }) => {
                        "system"
                    }
                    PersistedPayload::SystemReminder { .. } => "system_reminder",
                }
                .into(),
            })
        })
        .collect::<Result<Vec<_>, ArchiveError>>()?;
    let catalog = ArchiveCatalog {
        format_version: ARCHIVE_FORMAT_VERSION,
        group_id,
        source_thread_id,
        anchor_message_id,
        members,
        receipt,
        dependencies: Vec::new(),
    };
    catalog.validate()?;
    Ok(catalog)
}

impl ArchiveCatalog {
    pub fn validate(&self) -> Result<(), ArchiveError> {
        if self.format_version != ARCHIVE_FORMAT_VERSION {
            return Err(ArchiveError::Unsupported);
        }
        if self.members.is_empty()
            || self.members.len() > ARCHIVE_MAX_MEMBERS
            || serde_json::to_vec(self)
                .map_err(|_| ArchiveError::ContentMismatch)?
                .len()
                > ARCHIVE_MAX_METADATA_BYTES
        {
            return Err(ArchiveError::Unsupported);
        }
        let mut ids = std::collections::HashSet::new();
        if !self.members.iter().all(|m| {
            ids.insert(m.message_id)
                && m.canonical_bytes > 0
                && m.chunk_digests.len() as u64
                    == m.canonical_bytes.div_ceil(ARCHIVE_CHUNK_BYTES as u64)
        }) || !ids.contains(&self.anchor_message_id)
        {
            return Err(ArchiveError::ContentMismatch);
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String, ArchiveError> {
        self.validate()?;
        Ok(content_digest(
            &serde_json::to_vec(self).map_err(|_| ArchiveError::ContentMismatch)?,
        ))
    }
    pub fn member_ref(&self, member: usize) -> Result<String, ArchiveError> {
        if member >= self.members.len() {
            return Err(ArchiveError::NotFound);
        }
        ArchiveReference::Catalog {
            source_thread_id: self.source_thread_id.clone(),
            anchor_message_id: self.anchor_message_id,
            digest: self.digest()?,
            member: Some(member),
        }
        .encode()
    }
    pub fn reference(&self) -> Result<String, ArchiveError> {
        ArchiveReference::Catalog {
            source_thread_id: self.source_thread_id.clone(),
            anchor_message_id: self.anchor_message_id,
            digest: self.digest()?,
            member: None,
        }
        .encode()
    }
    pub fn remap(
        &self,
        target: ThreadId,
        ids: &HashMap<MessageId, MessageId>,
        payloads: &HashMap<MessageId, PersistedPayload>,
    ) -> Result<Self, ArchiveError> {
        let mapped = self
            .members
            .iter()
            .map(|m| {
                ids.get(&m.message_id)
                    .and_then(|id| payloads.get(id))
                    .cloned()
                    .ok_or(ArchiveError::NotFound)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let anchor = *ids
            .get(&self.anchor_message_id)
            .ok_or(ArchiveError::NotFound)?;
        let mut result = build_catalog(
            target,
            self.group_id.clone(),
            anchor,
            &mapped,
            self.receipt.clone(),
        )?;
        result.dependencies = self
            .dependencies
            .iter()
            .map(|id| ids.get(id).copied().ok_or(ArchiveError::NotFound))
            .collect::<Result<_, _>>()?;
        Ok(result)
    }
}

impl ArchiveReference {
    pub fn encode(&self) -> Result<String, ArchiveError> {
        let scope = self
            .source_thread_id()
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        Ok(match self {
            Self::Catalog {
                anchor_message_id,
                digest,
                member,
                ..
            } => format!(
                "pa1:c:{scope}:{}:{digest}:{}",
                anchor_message_id.as_uuid(),
                member.map(|i| i.to_string()).unwrap_or_else(|| "-".into())
            ),
            Self::Range { root } => format!(
                "pa1:r:{scope}:{}:{}:{}:{}",
                root.first_anchor.as_uuid(),
                root.last_anchor.as_uuid(),
                root.committed_tail.as_uuid(),
                content_digest(
                    &serde_json::to_vec(root).map_err(|_| ArchiveError::ContentMismatch)?
                )
            ),
        })
    }
    pub fn decode(value: &str) -> Result<Self, ArchiveError> {
        let invalid = || ArchiveError::InvalidRequest("malformed archive reference".into());
        let parts = value.split(':').collect::<Vec<_>>();
        if value.len() > 2048 || !(6..=7).contains(&parts.len()) || parts[0] != "pa1" {
            return Err(invalid());
        }
        let hex = parts[2];
        if hex.len() % 2 != 0 || !hex.is_ascii() {
            return Err(invalid());
        }
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| invalid()))
            .collect::<Result<Vec<_>, _>>()?;
        let source_thread_id = String::from_utf8(bytes).map_err(|_| invalid())?;
        let id = |s: &str| crate::history_page::message_id_of(s).map_err(|_| invalid());
        match parts[1] {
            "c" if parts.len() == 6 => Ok(Self::Catalog {
                source_thread_id,
                anchor_message_id: id(parts[3])?,
                digest: parts[4].into(),
                member: if parts[5] == "-" {
                    None
                } else {
                    Some(parts[5].parse().map_err(|_| invalid())?)
                },
            }),
            "r" if parts.len() == 7 => {
                let root = ArchiveRoot {
                    source_thread_id,
                    first_anchor: id(parts[3])?,
                    last_anchor: id(parts[4])?,
                    committed_tail: id(parts[5])?,
                };
                if content_digest(
                    &serde_json::to_vec(&root).map_err(|_| ArchiveError::ContentMismatch)?,
                ) != parts[6]
                {
                    return Err(ArchiveError::ContentMismatch);
                }
                Ok(Self::Range { root })
            }
            _ => Err(invalid()),
        }
    }
    pub fn source_thread_id(&self) -> &ThreadId {
        match self {
            Self::Catalog {
                source_thread_id, ..
            } => source_thread_id,
            Self::Range { root } => &root.source_thread_id,
        }
    }
}

impl ArchiveRoot {
    pub fn reference(&self) -> Result<String, ArchiveError> {
        ArchiveReference::Range { root: self.clone() }.encode()
    }
}

/// Adapter read plans are host-created. No model input can choose a session ID.
#[derive(Clone, Debug, Default)]
pub struct ArchiveMetadataQuery {
    pub anchor: Option<MessageId>,
    pub first_anchor: Option<MessageId>,
    pub last_anchor: Option<MessageId>,
    pub after: Option<MessageId>,
    pub committed_tail: Option<MessageId>,
    pub limit: u32,
}
#[derive(Clone, Debug)]
pub struct ArchiveMetadataItem {
    pub message_id: MessageId,
    pub projection: Option<MessageProjectionDirective>,
}
#[derive(Clone, Debug)]
pub struct ArchiveMetadataPage {
    pub committed_tail: MessageId,
    pub items: Vec<ArchiveMetadataItem>,
    pub next_after: Option<MessageId>,
}
#[derive(Clone, Debug)]
pub struct ArchiveChunkQuery {
    pub message_id: MessageId,
    pub committed_tail: MessageId,
    pub offset: u64,
    pub max_bytes: u32,
}
#[derive(Clone, Debug)]
pub struct ArchiveChunk {
    pub bytes: Vec<u8>,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveReadRequest {
    pub archive_ref: String,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub budget_tokens: Option<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArchiveReadResponse {
    pub archive_ref: String,
    pub complete: bool,
    pub next_cursor: Option<String>,
    pub entries: Vec<ArchiveReadEntry>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArchiveReadEntry {
    pub message_id: MessageId,
    pub content_digest: String,
    pub kind: String,
    pub block_types: Vec<String>,
    pub archive_ref: String,
    pub offset: u64,
    pub total_bytes: u64,
    /// Exact canonical JSON fragment. Partial fragments are never complete tool inputs; concatenate pages by byte offset. Media remains the original serialized block.
    pub data: String,
    pub encoding: String,
    pub complete: bool,
}
#[async_trait]
pub trait ContextArchiveReadPort: Send + Sync {
    async fn read(&self, request: ArchiveReadRequest) -> Result<ArchiveReadResponse, ArchiveError>;
}

fn recoverable_content(content: &crate::messages::MessageContent) -> bool {
    use crate::messages::{ContentBlock, DocumentSource, ImageSource, MessageContent};
    fn block(item: &ContentBlock) -> bool {
        match item {
            ContentBlock::Image {
                source: ImageSource::Url { .. },
            }
            | ContentBlock::Document {
                source: DocumentSource::Url { .. },
                ..
            } => false,
            ContentBlock::ToolResult { content, .. } => content.iter().all(block),
            _ => true,
        }
    }
    match content {
        MessageContent::Text(_) => true,
        MessageContent::Blocks(blocks) => blocks.iter().all(block),
        MessageContent::Raw(values) => values.iter().all(|value| {
            // Unrecognized provider blocks stay protected by the provider closure policy.
            // A recognized URL source still cannot claim durable media recovery.
            serde_json::from_value::<ContentBlock>(value.clone())
                .map(|b| block(&b))
                .unwrap_or(false)
        }),
    }
}

fn block_types(content: &crate::messages::MessageContent) -> Vec<String> {
    let mut types = content_block_types(content);
    types.sort_unstable();
    types.dedup();
    types
}

fn content_block_types(content: &crate::messages::MessageContent) -> Vec<String> {
    use crate::messages::{ContentBlock, MessageContent};
    match content {
        MessageContent::Text(_) => vec!["text".into()],
        MessageContent::Raw(values) => values
            .iter()
            .map(|value| {
                value
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned()
            })
            .collect(),
        MessageContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| {
                match block {
                    ContentBlock::Text { .. } => "text",
                    ContentBlock::Image { .. } => "image",
                    ContentBlock::Document { .. } => "document",
                    ContentBlock::ToolUse { .. } => "tool_use",
                    ContentBlock::ToolResult { .. } => "tool_result",
                    ContentBlock::Reasoning { .. } => "reasoning",
                    ContentBlock::Unknown(_) => "unknown",
                }
                .to_owned()
            })
            .collect(),
    }
}
