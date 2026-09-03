//! `BG_LIVE_DETAIL` 投影写入。

use crate::kit::acp_types::{ToolCardAccumulator, build_tool_card};
use crate::kit::atoms::{BG_LIVE_DETAIL, BgLiveDetail, BgLiveStatus};
use crate::kit::bg_task_identity::task_id_for_agent_id;
use crate::kit::stream_data::{TuiReasoningChunk, TuiTextChunk, TuiToolEnded, TuiToolStarted};
use crate::kit::tui_render_unit::{
    EntryStatus, FoldState, TuiAssistantBubble, TuiReasoningBlock, TuiRenderUnit,
};

fn with_live_detail<F>(task_id: &str, f: F)
where
    F: FnOnce(&mut BgLiveDetail),
{
    let live = BG_LIVE_DETAIL.state();
    let mut map = live.write();
    f(map.entry(task_id.to_string()).or_default());
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
        f(&task_id, detail);
    }
}

fn sync_tool_units(detail: &mut BgLiveDetail) {
    let mut units: im::Vector<TuiRenderUnit> = detail
        .nested_units
        .iter()
        .filter(|u| !matches!(u, TuiRenderUnit::TuiToolCard(_)))
        .cloned()
        .collect();
    for acc in &detail.tool_cards {
        let card = build_tool_card(acc, detail.status == BgLiveStatus::Running);
        units.push_back(TuiRenderUnit::TuiToolCard(card));
    }
    detail.nested_units = units;
}

pub fn init_agent_live_detail(task_id: &str, agent_id: &str, agent_name: &str) {
    with_live_detail(task_id, |d| {
        d.agent_id = Some(agent_id.to_string());
        d.agent_name = Some(agent_name.to_string());
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

pub(crate) fn handle_bg_tool_started(
    agent_id: &str,
    ts: &TuiToolStarted,
    previous_todos: Option<&crate::kit::tool_semantics::TodoSnapshot>,
) {
    with_live_detail_for_agent(agent_id, |_, detail| {
        if detail
            .tool_cards
            .iter()
            .any(|t| t.tool_id == ts.tool_id && t.output_summary.is_none())
        {
            return;
        }
        detail.tool_cards.push(ToolCardAccumulator::with_input(
            ts.tool_id.clone(),
            ts.tool_name.clone(),
            ts.input_summary.clone(),
            ts.raw_input.clone(),
            previous_todos,
        ));
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
        t.completed_duration_ms = Some(t.started_at.elapsed().as_millis() as u64);
        sync_tool_units(detail);
    });
}

pub fn append_bg_text_chunk(agent_id: &str, tc: &TuiTextChunk) {
    if tc.text.is_empty() {
        return;
    }
    with_live_detail_for_agent(agent_id, |_, detail| {
        if let Some(TuiRenderUnit::TuiAssistantBubble(b)) = detail.nested_units.back() {
            let mut b = b.clone();
            b.text.push_str(&tc.text);
            b.recompute_hash();
            let _ = detail.nested_units.pop_back();
            detail
                .nested_units
                .push_back(TuiRenderUnit::TuiAssistantBubble(b));
        } else {
            let mut bubble = TuiAssistantBubble {
                text: tc.text.clone(),
                reasoning: None,
                message_id: tc.message_id.clone(),
                started_at: Some(std::time::Instant::now()),
                duration_ms: None,
                content_hash: 0,
            };
            bubble.recompute_hash();
            detail
                .nested_units
                .push_back(TuiRenderUnit::TuiAssistantBubble(bubble));
        }
    });
}

pub fn append_bg_reasoning_chunk(agent_id: &str, rc: &TuiReasoningChunk) {
    if rc.text.is_empty() {
        return;
    }
    with_live_detail_for_agent(agent_id, |_, detail| {
        if let Some(TuiRenderUnit::TuiAssistantBubble(b)) = detail.nested_units.back() {
            let mut b = b.clone();
            let reasoning = b.reasoning.get_or_insert_with(|| TuiReasoningBlock {
                text: String::new(),
                fold: FoldState::Preview,
                status: EntryStatus::Running,
                is_running: true,
                started_at: Some(std::time::Instant::now()),
                duration_ms: None,
            });
            reasoning.text.push_str(&rc.text);
            b.recompute_hash();
            let _ = detail.nested_units.pop_back();
            detail
                .nested_units
                .push_back(TuiRenderUnit::TuiAssistantBubble(b));
            return;
        }
        let mut bubble = TuiAssistantBubble {
            text: String::new(),
            reasoning: Some(TuiReasoningBlock {
                text: rc.text.clone(),
                fold: FoldState::Preview,
                status: EntryStatus::Running,
                is_running: true,
                started_at: Some(std::time::Instant::now()),
                duration_ms: None,
            }),
            message_id: rc.message_id.clone(),
            started_at: None,
            duration_ms: None,
            content_hash: 0,
        };
        bubble.recompute_hash();
        detail
            .nested_units
            .push_back(TuiRenderUnit::TuiAssistantBubble(bubble));
    });
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
        sync_tool_units(d);
    });
}

pub fn mark_task_cancelled(task_id: &str, reason: &str) {
    with_live_detail(task_id, |d| {
        d.cancel_reason = Some(reason.to_string());
        d.status = BgLiveStatus::Cancelled;
        sync_tool_units(d);
    });
}

pub fn reconcile_live_snapshot(active_task_ids: &[String]) {
    let live = BG_LIVE_DETAIL.state();
    let mut map = live.write();
    for task_id in active_task_ids {
        map.entry(task_id.clone()).or_default().status = BgLiveStatus::Running;
    }
}
