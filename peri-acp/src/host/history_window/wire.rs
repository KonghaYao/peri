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

use super::IssuedWindowTicket;

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
