//! Bounds for disposable background display projections, not task results or RSS.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{BgLiveDetail, BgLiveStatus, TuiRenderUnit};

pub(super) const PREVIEW_BYTES: usize = 4 * 1024 * 1024;
pub(super) const DETAIL_BYTES: usize = 256 * 1024;
pub(super) const FIELD_BYTES: usize = 16 * 1024;
pub(super) const STREAM_BYTES: usize = 32 * 1024;
pub(super) const TERMINAL_BYTES: usize = 1024 * 1024;
pub(super) const TERMINAL_ENTRIES: usize = 512;
static NEXT_UPDATE: AtomicU64 = AtomicU64::new(1);

pub(super) fn touch(detail: &mut BgLiveDetail) {
    detail.preview_updated = NEXT_UPDATE.fetch_add(1, Ordering::Relaxed);
}

/// Copy only the retained UTF-8 tail. Never clone a large source before clipping.
pub(super) fn text(value: &str, limit: usize, omitted: &mut bool) -> String {
    let mut start = value.len().saturating_sub(limit);
    while !value.is_char_boundary(start) {
        start += 1;
    }
    *omitted |= start != 0;
    value[start..].to_owned()
}

pub(super) fn append(value: &mut String, chunk: &str, omitted: &mut bool) {
    let incoming = value.len().saturating_add(chunk.len());
    if incoming <= STREAM_BYTES {
        if value.capacity() < incoming {
            let capacity = incoming
                .max(value.capacity().saturating_mul(2))
                .min(STREAM_BYTES);
            value.reserve_exact(capacity - value.len());
        }
        value.push_str(chunk);
        return;
    }
    *omitted = true;
    if chunk.len() >= STREAM_BYTES {
        *value = text(chunk, STREAM_BYTES, omitted);
        return;
    }
    let keep = STREAM_BYTES - chunk.len();
    let mut start = value.len().saturating_sub(keep);
    while !value.is_char_boundary(start) {
        start += 1;
    }
    let mut next = String::with_capacity(value.len() - start + chunk.len());
    next.push_str(&value[start..]);
    next.push_str(chunk);
    *value = next;
}

/// Include container storage, unlike a serialized JSON byte count. Object nodes
/// are conservatively charged per entry; this is a display admission estimate.
pub(super) fn json_reserved(value: &serde_json::Value) -> usize {
    use serde_json::Value;
    match value {
        Value::String(s) => s.capacity(),
        Value::Array(items) => {
            items.capacity() * std::mem::size_of::<Value>()
                + items.iter().map(json_reserved).sum::<usize>()
        }
        Value::Object(items) => items
            .iter()
            .map(|(key, value)| {
                128 + std::mem::size_of::<Value>() + key.capacity() + json_reserved(value)
            })
            .sum(),
        _ => 0,
    }
}

pub(super) fn raw_input(value: &serde_json::Value, omitted: &mut bool) -> serde_json::Value {
    if json_reserved(value) > FIELD_BYTES {
        *omitted = true;
        // A partial JSON object must never masquerade as the complete input.
        serde_json::Value::Null
    } else {
        value.clone()
    }
}

pub(super) fn presentation_reserved(
    value: &crate::kit::tui_render_unit::TuiToolPresentation,
) -> usize {
    crate::kit::retained_bytes::presentation_bytes(value).reserved
}

pub(super) fn finish(detail: &mut BgLiveDetail) {
    super::finalize_nested_reasoning(detail);
    super::sync_tool_units(detail);
    // The immutable projected cards are sufficient after termination. Keeping
    // accumulators also retains raw_input and a second copy of tool outputs.
    detail.tool_cards = Vec::new();
    detail.stream = None;
}

fn clear_body(detail: &mut BgLiveDetail) {
    detail.preview_omitted = true;
    detail.summary = String::new();
    detail.output_preview = None;
    detail.cancel_reason = None;
    detail.subagent_result = None;
    detail.nested_units = im::Vector::new();
    detail.tool_cards = Vec::new();
    detail.stream = None;
}

fn trim_detail(detail: &mut BgLiveDetail) {
    while detail.preview_reserved() > DETAIL_BYTES {
        let Some(unit) = detail.nested_units.pop_front() else {
            clear_body(detail);
            break;
        };
        detail.preview_omitted = true;
        if let TuiRenderUnit::TuiToolCard(card) = unit {
            detail
                .tool_cards
                .retain(|tool| tool.tool_id != card.tool_id);
            detail.tool_cards.shrink_to_fit();
        }
        if detail.nested_units.is_empty()
            && let Some(stream) = detail.stream.as_mut()
        {
            stream.projected = false;
            stream.dirty = true;
        }
    }
}

pub(super) fn enforce(map: &mut HashMap<String, BgLiveDetail>) {
    for detail in map.values_mut() {
        trim_detail(detail);
    }
    let mut bytes: usize = map.values().map(BgLiveDetail::preview_reserved).sum();
    while bytes > PREVIEW_BYTES {
        let candidate = map
            .iter()
            .filter(|(_, detail)| detail.preview_reserved() > 0)
            .min_by_key(|(_, detail)| {
                (
                    detail.status == BgLiveStatus::Running,
                    detail.preview_updated,
                )
            })
            .map(|(id, _)| id.clone());
        let Some(id) = candidate else { break };
        let detail = map.get_mut(&id).unwrap();
        bytes = bytes.saturating_sub(detail.preview_reserved());
        clear_body(detail);
    }
    loop {
        let (count, bytes) = map
            .iter()
            .filter(|(_, detail)| detail.status != BgLiveStatus::Running)
            .fold((0, 0), |(count, bytes), (id, detail)| {
                (
                    count + 1,
                    bytes + id.capacity() + detail.memory_bytes().reserved,
                )
            });
        if count <= TERMINAL_ENTRIES && bytes <= TERMINAL_BYTES {
            break;
        }
        let candidate = map
            .iter()
            .filter(|(_, detail)| detail.status != BgLiveStatus::Running)
            .min_by_key(|(_, detail)| detail.preview_updated)
            .map(|(id, _)| id.clone());
        let Some(id) = candidate else { break };
        map.remove(&id);
    }
    // Removing entries alone leaves a peak-sized hash table allocation behind.
    if map.capacity() > map.len().saturating_mul(2).saturating_add(64) {
        map.shrink_to_fit();
    }
}

#[cfg(test)]
#[path = "bg_task_live_preview_test.rs"]
mod tests;
