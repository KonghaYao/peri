//! V1 响应项/游标/正文分块的唯一 wire 构造。
//!
//! 权威形状是 `items[].body` 内联 descriptor；不存在顶层 `bodyDescriptors`。

use serde_json::{json, Value};

use peri_acp_types::history_window::{
    HistoryBodyChunk, HistoryBodyDescriptor, HistoryEntryId, HistoryInheritedSnapshot,
    HistorySource, HistoryWindowCursor, HistoryWindowDirection, HistoryWindowMetadata,
    HISTORY_WINDOW_DEFAULT_CHUNK_SIZE, HISTORY_WINDOW_PROJECTION_VERSION,
};
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    HistoryWindowEntry, HistoryWindowRows, InheritedHistoryEntry,
};
use peri_acp_types::PeriCaps;

use crate::host::history_page::page_item;

use super::{
    anchor_unavailable, resource_window_error, AcpError, AcpServerConfig, IssuedWindowTicket,
};

// 在票据登记和 wire descriptor 构造之前过滤；游标仍按存储行推进。
pub(super) async fn filter_rows(
    cfg: &AcpServerConfig,
    thread_id: &str,
    source: &HistorySource,
    ticket: &peri_acp_types::history_window::HistoryReadTicket,
    result: Result<HistoryWindowRows, AcpError>,
) -> Result<HistoryWindowRows, AcpError> {
    let mut rows = result?;
    let mut visible = Vec::new();
    for id in oversize_ids(Some(&rows)) {
        if body_visible(
            cfg,
            thread_id,
            source,
            rows.committed_tail.as_ref(),
            ticket,
            id,
        )
        .await?
        {
            visible.push(id);
        }
    }
    rows.entries.retain(|entry| match entry {
        HistoryWindowEntry::Oversized { entry_id, .. } => visible.contains(entry_id),
        HistoryWindowEntry::Loaded(item) => item.payload.displayable_to_client(),
    });
    rows.inherited_entries.retain(|entry| match entry {
        InheritedHistoryEntry::Oversized { entry_id, .. } => visible.contains(entry_id),
        InheritedHistoryEntry::Loaded(item) => item.payload.displayable_to_client(),
    });
    Ok(rows)
}

pub(super) async fn body_visible(
    cfg: &AcpServerConfig,
    thread_id: &str,
    source: &HistorySource,
    tail: Option<&peri_acp_types::history_window::HistoryCommittedTail>,
    ticket: &peri_acp_types::history_window::HistoryReadTicket,
    entry_id: MessageId,
) -> Result<bool, AcpError> {
    // 同步 serde reader 在 blocking pool 中消费容量为 1 的通道；不聚合大正文。
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    let parsing = tokio::task::spawn_blocking(move || {
        peri_acp_types::history_page::history_body_displayable_to_client(BodyReader {
            receiver,
            current: std::io::Cursor::new(Vec::new()),
        })
    });
    let mut offset = 0;
    let mut failure = None;
    loop {
        let chunk = cfg
            .session_resources
            .load_history_body_chunk(
                &thread_id.to_owned(),
                source.clone(),
                entry_id,
                offset,
                HISTORY_WINDOW_DEFAULT_CHUNK_SIZE,
                tail,
                ticket,
            )
            .await
            .map_err(resource_window_error);
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                failure = Some(error);
                break;
            }
        };
        if let Err(error) = chunk.validate(
            &HistoryEntryId::new(&thread_id.to_owned(), entry_id).encode(),
            offset,
            HISTORY_WINDOW_DEFAULT_CHUNK_SIZE,
        ) {
            failure = Some(super::window_error(error));
            break;
        }
        let next = chunk.next_offset;
        if sender.send(chunk.bytes).await.is_err() {
            break;
        }
        match next {
            Some(next) => offset = next,
            None => break,
        }
    }
    drop(sender);
    let parsed = parsing.await.map_err(|error| {
        tracing::error!(%error, "history audience parser failed");
        anchor_unavailable("history audience could not be verified")
    })?;
    if let Some(error) = failure {
        return Err(error);
    }
    parsed.map_err(|_| {
        tracing::warn!("history audience decoding failed");
        anchor_unavailable("history audience could not be verified")
    })
}

struct BodyReader {
    receiver: tokio::sync::mpsc::Receiver<Vec<u8>>,
    current: std::io::Cursor<Vec<u8>>,
}

impl std::io::Read for BodyReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let count = std::io::Read::read(&mut self.current, buffer)?;
            if count != 0 || buffer.is_empty() {
                return Ok(count);
            }
            match self.receiver.blocking_recv() {
                Some(bytes) => self.current = std::io::Cursor::new(bytes),
                None => return Ok(0),
            }
        }
    }
}

pub(super) fn oversize_ids(rows: Option<&HistoryWindowRows>) -> Vec<MessageId> {
    rows.map(|rows| {
        let mut ids: Vec<MessageId> = rows
            .entries
            .iter()
            .filter_map(|entry| match entry {
                HistoryWindowEntry::Oversized { entry_id, .. } => Some(*entry_id),
                HistoryWindowEntry::Loaded(_) => None,
            })
            .collect();
        ids.extend(
            rows.inherited_entries
                .iter()
                .filter_map(|entry| match entry {
                    InheritedHistoryEntry::Oversized { entry_id, .. } => Some(*entry_id),
                    InheritedHistoryEntry::Loaded(_) => None,
                }),
        );
        ids
    })
    .unwrap_or_default()
}

pub(super) fn response(
    session_id: &str,
    thread_id: &str,
    metadata: &HistoryWindowMetadata,
    issued: &IssuedWindowTicket,
    source: &HistorySource,
    rows: HistoryWindowRows,
    caps: &PeriCaps,
) -> Value {
    let mut items = Vec::new();
    for entry in rows.entries {
        match entry {
            HistoryWindowEntry::Loaded(item) => {
                if let Some(mut value) = page_item(&item, thread_id, caps) {
                    value["source"] = json!(source);
                    items.push(value);
                }
            }
            HistoryWindowEntry::Oversized {
                entry_id,
                position,
                total_bytes,
            } => {
                let body = HistoryBodyDescriptor::new(
                    total_bytes,
                    HISTORY_WINDOW_DEFAULT_CHUNK_SIZE,
                    metadata.snapshot_token.clone(),
                    issued.opaque.clone(),
                )
                .expect("oversize descriptor values are bounded by the window plan");
                items.push(json!({
                    "entryId": HistoryEntryId::new(&thread_id.to_owned(), entry_id).encode(),
                    "source": source,
                    "position": position,
                    "oversize": true,
                    "body": body,
                }));
            }
        }
    }
    for entry in rows.inherited_entries {
        match entry {
            InheritedHistoryEntry::Loaded(item) => {
                if let Some(mut value) = page_item(&item, thread_id, caps) {
                    value["position"] = Value::Null;
                    value["source"] = json!(source);
                    items.push(value);
                }
            }
            InheritedHistoryEntry::Oversized {
                entry_id,
                total_bytes,
            } => {
                let body = HistoryBodyDescriptor::new(
                    total_bytes,
                    HISTORY_WINDOW_DEFAULT_CHUNK_SIZE,
                    metadata.snapshot_token.clone(),
                    issued.opaque.clone(),
                )
                .expect("inherited oversize descriptor values are bounded by the window plan");
                items.push(json!({
                    "entryId": HistoryEntryId::new(&thread_id.to_owned(), entry_id).encode(),
                    "source": source,
                    "position": null,
                    "oversize": true,
                    "body": body,
                }));
            }
        }
    }
    let before_cursor = rows.before_cursor_anchor.map(|anchor| {
        cursor_value(
            thread_id,
            metadata,
            source,
            anchor,
            HistoryWindowDirection::Before,
        )
    });
    let after_cursor = rows.after_cursor_anchor.map(|anchor| {
        cursor_value(
            thread_id,
            metadata,
            source,
            anchor,
            HistoryWindowDirection::After,
        )
    });
    json!({
        "sessionId": session_id,
        "sessionGeneration": metadata.session_generation,
        "structureVersion": metadata.structure_version,
        "snapshotToken": metadata.snapshot_token,
        "committedTail": metadata.committed_tail.as_ref().map(|tail| tail.encode()),
        "checkpointId": metadata.checkpoint_id,
        "readTicket": issued.opaque.as_str(),
        "beforeCursor": before_cursor,
        "afterCursor": after_cursor,
        "items": items,
        "inherited": inherited_value(issued.inherited.as_ref(), thread_id),
        "inheritedUnsupported": issued.inherited_unsupported,
        "rewindInvalidation": { "status": "active", "reopenRequired": false },
    })
}

fn inherited_value(snapshot: Option<&HistoryInheritedSnapshot>, child_thread_id: &str) -> Value {
    match snapshot {
        None => Value::Null,
        Some(snapshot) => json!({
            "snapshotId": snapshot.snapshot_id,
            "ancestorThreadId": snapshot.ancestor_thread_id,
            "firstEntryId": snapshot
                .first_entry
                .map(|id| HistoryEntryId::new(&child_thread_id.to_owned(), id).encode()),
            "lastEntryId": snapshot
                .last_entry
                .map(|id| HistoryEntryId::new(&child_thread_id.to_owned(), id).encode()),
            "entryCount": snapshot.entry_count,
            "supported": true,
        }),
    }
}

fn cursor_value(
    thread_id: &str,
    metadata: &HistoryWindowMetadata,
    source: &HistorySource,
    anchor: MessageId,
    direction: HistoryWindowDirection,
) -> Value {
    Value::String(
        HistoryWindowCursor {
            thread_id: thread_id.to_owned(),
            session_generation: metadata.session_generation.clone(),
            structure_version: metadata.structure_version,
            snapshot_token: metadata.snapshot_token.clone(),
            committed_tail: metadata.committed_tail.clone(),
            source: source.clone(),
            anchor,
            direction,
            projection_version: HISTORY_WINDOW_PROJECTION_VERSION,
        }
        .encode(),
    )
}

pub(super) fn chunk_value(chunk: HistoryBodyChunk) -> Value {
    serde_json::to_value(chunk).expect("body chunk is a plain JSON object")
}
