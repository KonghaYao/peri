//! `BG_LIVE_DETAIL` 投影写入。

use crate::kit::acp_types::{ToolCardAccumulator, build_tool_card};
use crate::kit::atoms::{BG_LIVE_DETAIL, BgLiveDetail, BgLiveStatus};
use crate::kit::bg_task_identity::task_id_for_agent_id;
use crate::kit::stream_data::{TuiReasoningChunk, TuiTextChunk, TuiToolEnded, TuiToolStarted};
use crate::kit::tui_render_unit::{
    EntryStatus, FoldState, TuiAssistantBubble, TuiReasoningBlock, TuiRenderUnit, tui_hash_roll,
    tui_hash_roll_update,
};

#[derive(Debug, Clone)]
pub(crate) struct BgStream {
    bubble: TuiAssistantBubble,
    text_hash: u64,
    reasoning_hash: u64,
    projected: bool,
    dirty: bool,
}

impl BgStream {
    fn new(detail: &BgLiveDetail, message_id: Option<String>) -> Self {
        let (bubble, projected) = match detail.nested_units.back() {
            Some(TuiRenderUnit::TuiAssistantBubble(bubble)) => ((**bubble).clone(), true),
            _ => (
                TuiAssistantBubble {
                    text: String::new(),
                    reasoning: None,
                    message_id,
                    started_at: None,
                    duration_ms: None,
                    content_hash: 0,
                },
                false,
            ),
        };
        Self {
            text_hash: tui_hash_roll(&bubble.text),
            reasoning_hash: bubble
                .reasoning
                .as_ref()
                .map(|block| tui_hash_roll(&block.text))
                .unwrap_or(0),
            bubble,
            projected,
            dirty: false,
        }
    }
}

fn flush_detail(detail: &mut BgLiveDetail) -> bool {
    let Some(stream) = detail.stream.as_mut().filter(|stream| stream.dirty) else {
        return false;
    };
    stream.bubble.content_hash = TuiAssistantBubble::compute_hash_from_rolls(
        stream.text_hash,
        stream.reasoning_hash,
        stream.bubble.reasoning.as_ref(),
        stream.bubble.duration_secs(),
        stream.bubble.started_at.is_none(),
    );
    if stream.projected {
        detail.nested_units.pop_back();
    }
    detail
        .nested_units
        .push_back(TuiRenderUnit::TuiAssistantBubble(
            stream.bubble.clone().into(),
        ));
    stream.projected = true;
    stream.dirty = false;
    true
}

pub(crate) fn has_pending_streams() -> bool {
    BG_LIVE_DETAIL
        .state()
        .read()
        .values()
        .any(|detail| detail.stream.as_ref().is_some_and(|stream| stream.dirty))
}

pub(crate) fn publish_pending_streams() {
    if !has_pending_streams() {
        return;
    }
    let live = BG_LIVE_DETAIL.state();
    let mut map = live.write();
    for detail in map.values_mut() {
        flush_detail(detail);
    }
}

fn append_stream(agent_id: &str, message_id: Option<String>, chunk: &str, reasoning: bool) {
    let Some(task_id) = task_id_for_agent_id(agent_id) else {
        return;
    };
    let live = BG_LIVE_DETAIL.state();
    let mut map = live.write_no_update();
    let Some(detail) = map.get_mut(&task_id) else {
        return;
    };
    if detail.stream.is_none() {
        detail.stream = Some(BgStream::new(detail, message_id));
    }
    let stream = detail.stream.as_mut().unwrap();
    if reasoning {
        let block = stream
            .bubble
            .reasoning
            .get_or_insert_with(|| TuiReasoningBlock {
                text: String::new(),
                fold: FoldState::Preview,
                status: EntryStatus::Running,
                is_running: true,
                started_at: Some(peri_time::monotonic_now()),
                duration_ms: None,
            });
        block.text.push_str(chunk);
        stream.reasoning_hash = tui_hash_roll_update(stream.reasoning_hash, chunk);
    } else {
        stream
            .bubble
            .started_at
            .get_or_insert_with(peri_time::monotonic_now);
        stream.bubble.text.push_str(chunk);
        stream.text_hash = tui_hash_roll_update(stream.text_hash, chunk);
    }
    stream.dirty = true;
}

fn with_live_detail<F>(task_id: &str, f: F)
where
    F: FnOnce(&mut BgLiveDetail),
{
    let live = BG_LIVE_DETAIL.state();
    let mut map = live.write();
    let detail = map.entry(task_id.to_string()).or_default();
    flush_detail(detail);
    detail.stream = None;
    f(detail);
}

fn with_live_detail_for_agent<F>(agent_id: &str, f: F)
where
    F: FnOnce(&str, &mut BgLiveDetail),
{
    let Some(task_id) = task_id_for_agent_id(agent_id) else {
        return;
    };
    let live = BG_LIVE_DETAIL.state();
    let mut map = live.write();
    if let Some(detail) = map.get_mut(&task_id) {
        flush_detail(detail);
        detail.stream = None;
        f(&task_id, detail);
    }
}

fn sync_tool_units(detail: &mut BgLiveDetail) {
    let mut seen = std::collections::HashSet::new();
    let mut units = detail.nested_units.clone();
    for unit in units.iter_mut() {
        let TuiRenderUnit::TuiToolCard(existing) = unit else {
            continue;
        };
        let Some(acc) = detail
            .tool_cards
            .iter()
            .find(|acc| acc.tool_id == existing.tool_id)
        else {
            continue;
        };
        seen.insert(acc.tool_id.clone());
        *unit = TuiRenderUnit::TuiToolCard(detail_tool_card(detail, acc));
    }
    for acc in &detail.tool_cards {
        if seen.insert(acc.tool_id.clone()) {
            units.push_back(TuiRenderUnit::TuiToolCard(detail_tool_card(detail, acc)));
        }
    }
    detail.nested_units = units;
}

fn detail_tool_card(
    detail: &BgLiveDetail,
    tool: &ToolCardAccumulator,
) -> crate::kit::tui_render_unit::TuiToolCard {
    let mut card = build_tool_card(tool, detail.status == BgLiveStatus::Running);
    if detail.status == BgLiveStatus::Unobserved && tool.output_summary.is_none() {
        card.is_error = true;
        card.output_summary = crate::i18n::tr("shell-detail-status-unobserved");
        card.fold = crate::kit::tui_render_unit::fold_for_status(
            crate::kit::tui_render_unit::FoldTarget::Tool,
            EntryStatus::Error,
        );
        card.recompute_hash();
    }
    card
}

fn finalize_nested_reasoning(detail: &mut BgLiveDetail) {
    for unit in detail.nested_units.iter_mut() {
        let TuiRenderUnit::TuiAssistantBubble(bubble) = unit else {
            continue;
        };
        let mut bubble = (**bubble).clone();
        if let Some(reasoning) = bubble.reasoning.as_mut() {
            reasoning.duration_ms = reasoning.duration_ms.or_else(|| {
                reasoning
                    .started_at
                    .map(|started| peri_time::elapsed_since(started).as_millis() as u64)
            });
            reasoning.started_at = None;
            reasoning.is_running = false;
            reasoning.status = EntryStatus::Completed;
            reasoning.fold = FoldState::Collapsed;
        }
        bubble.recompute_hash();
        *unit = TuiRenderUnit::TuiAssistantBubble(bubble.into());
    }
}

pub fn init_agent_live_detail(
    task_id: &str,
    agent_id: &str,
    agent_name: &str,
    subagent_instance_id: Option<&str>,
) {
    with_live_detail(task_id, |d| {
        d.agent_id = Some(agent_id.to_string());
        d.agent_name = Some(agent_name.to_string());
        d.subagent_instance_id = subagent_instance_id.map(str::to_string);
        d.status = BgLiveStatus::Running;
    });
}

pub fn seed_live_from_started(task_id: &str, kind: &str, summary: &str, pid: Option<u32>) {
    with_live_detail(task_id, |d| {
        d.kind = kind.to_string();
        d.summary = summary.to_string();
        d.pid = pid;
        d.status = BgLiveStatus::Running;
    });
}

pub fn seed_live_from_terminal_snapshot(
    task_id: &str,
    kind: &str,
    summary: &str,
    pid: Option<u32>,
    status: &str,
) {
    with_live_detail(task_id, |detail| {
        detail.kind = kind.to_string();
        detail.summary = summary.to_string();
        detail.pid = pid;
        detail.status = match status {
            "completed" => BgLiveStatus::Succeeded,
            "failed" => BgLiveStatus::Failed,
            "cancelled" => BgLiveStatus::Cancelled,
            _ => return,
        };
        finalize_nested_reasoning(detail);
        sync_tool_units(detail);
    });
}

pub(crate) fn handle_bg_tool_started(
    agent_id: &str,
    ts: &TuiToolStarted,
    previous_todos: Option<&crate::kit::tool_semantics::TodoSnapshot>,
) {
    with_live_detail_for_agent(agent_id, |_, detail| {
        let tool = ToolCardAccumulator::with_input(
            ts.tool_id.clone(),
            ts.tool_name.clone(),
            ts.input_summary.clone(),
            ts.raw_input.clone(),
            previous_todos,
        );
        if let Some(existing) = detail
            .tool_cards
            .iter_mut()
            .find(|existing| existing.tool_id == ts.tool_id)
        {
            if existing.upgrade_input(tool) {
                sync_tool_units(detail);
            }
            return;
        }
        detail.tool_cards.push(tool);
        sync_tool_units(detail);
    });
}

pub fn handle_bg_tool_ended(agent_id: &str, te: &TuiToolEnded) {
    with_live_detail_for_agent(agent_id, |_, detail| {
        let Some(t) = detail
            .tool_cards
            .iter_mut()
            .find(|t| t.tool_id == te.tool_id && t.output_summary.is_none())
        else {
            return;
        };
        t.output_summary = Some(te.output_summary.clone());
        t.is_error = te.is_error;
        t.completed_duration_ms = Some(peri_time::elapsed_since(t.started_at).as_millis() as u64);
        sync_tool_units(detail);
    });
}

pub fn append_bg_text_chunk(agent_id: &str, tc: &TuiTextChunk) {
    if tc.text.is_empty() {
        return;
    }
    append_stream(agent_id, tc.message_id.clone(), &tc.text, false);
}

pub fn append_bg_reasoning_chunk(agent_id: &str, rc: &TuiReasoningChunk) {
    if rc.text.is_empty() {
        return;
    }
    append_stream(agent_id, rc.message_id.clone(), &rc.text, true);
}

pub fn handle_bg_subagent_stopped(agent_id: &str, result: &str, is_error: bool) {
    with_live_detail_for_agent(agent_id, |_, detail| {
        detail.subagent_result = Some(result.to_string());
        detail.subagent_is_error = is_error;
        detail.status = if is_error {
            BgLiveStatus::Failed
        } else {
            BgLiveStatus::Succeeded
        };
        finalize_nested_reasoning(detail);
        sync_tool_units(detail);
    });
}

pub fn mark_task_completed(
    task_id: &str,
    success: bool,
    duration_ms: u64,
    output_preview: Option<String>,
) {
    with_live_detail(task_id, |d| {
        d.duration_ms = Some(duration_ms);
        d.output_preview = output_preview.filter(|s| !s.is_empty());
        d.status = if success {
            BgLiveStatus::Succeeded
        } else {
            BgLiveStatus::Failed
        };
        finalize_nested_reasoning(d);
        sync_tool_units(d);
    });
}

pub fn mark_task_cancelled(task_id: &str, reason: &str) {
    with_live_detail(task_id, |d| {
        d.cancel_reason = Some(reason.to_string());
        d.status = BgLiveStatus::Cancelled;
        finalize_nested_reasoning(d);
        sync_tool_units(d);
    });
}

pub fn seed_unobserved_snapshot(task_id: &str, kind: &str, summary: &str, pid: Option<u32>) {
    with_live_detail(task_id, |detail| {
        detail.kind = kind.to_string();
        detail.summary = summary.to_string();
        detail.pid = pid;
        detail.status = BgLiveStatus::Unobserved;
        finalize_nested_reasoning(detail);
        sync_tool_units(detail);
    });
}

#[cfg(test)]
#[path = "bg_task_live_test.rs"]
mod tests;
