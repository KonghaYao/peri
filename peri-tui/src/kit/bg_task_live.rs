//! `BG_LIVE_DETAIL` 投影写入。

use crate::kit::acp_types::{ToolCardAccumulator, build_tool_card};
use crate::kit::atoms::{BG_LIVE_DETAIL, BgLiveDetail, BgLiveStatus};
use crate::kit::bg_task_identity::task_id_for_agent_id;
use crate::kit::stream_data::{TuiReasoningChunk, TuiTextChunk, TuiToolEnded, TuiToolStarted};
use crate::kit::tui_render_unit::{
    EntryStatus, FoldState, TuiAssistantBubble, TuiReasoningBlock, TuiRenderUnit, tui_hash_roll,
    tui_hash_roll_update,
};

/// `BgLiveDetail` 的分层驻留计量。
///
/// `stream` 与 `nested_units` 是两份真实分配（stream 的 bubble 在投影时被
/// clone 到 nested），两者都按实际持有量计入；`tool_cards` 计入累积器自身
/// 持有的字符串、`raw_input` 与 presentation，不假设它们与展示卡共享分配。
/// `body` 是逻辑正文字节（同一正文出现在多个槽位会重复计），`reserved` 才按
/// 分配身份去重；两者口径不同，不得直接相减。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BgLiveMemoryBytes {
    pub summary_bytes: usize,
    pub output_preview_bytes: usize,
    pub subagent_result_bytes: usize,
    pub nested_units_bytes: usize,
    pub stream_bytes: usize,
    pub tool_cards_bytes: usize,
    pub body: usize,
    pub reserved: usize,
}

impl BgLiveDetail {
    /// 只读分层计量；不改变任何状态。
    pub(crate) fn memory_bytes(&self) -> BgLiveMemoryBytes {
        let summary = layer_bytes(&self.summary);
        let output_preview = self
            .output_preview
            .as_ref()
            .map(layer_bytes)
            .unwrap_or_default();
        let subagent_result = self
            .subagent_result
            .as_ref()
            .map(layer_bytes)
            .unwrap_or_default();
        let nested_units = nested_units_layer_bytes(self);
        let stream = self
            .stream
            .as_ref()
            .map(BgStream::memory_bytes)
            .unwrap_or_default();
        let tool_cards = tool_cards_layer_bytes(&self.tool_cards);
        BgLiveMemoryBytes {
            summary_bytes: summary.0,
            output_preview_bytes: output_preview.0,
            subagent_result_bytes: subagent_result.0,
            nested_units_bytes: nested_units.0,
            stream_bytes: stream.0,
            tool_cards_bytes: tool_cards.0,
            body: summary.0
                + output_preview.0
                + subagent_result.0
                + nested_units.0
                + stream.0
                + tool_cards.0,
            reserved: summary.1
                + output_preview.1
                + subagent_result.1
                + nested_units.1
                + stream.1
                + tool_cards.1,
        }
    }
}

fn layer_bytes(value: &String) -> (usize, usize) {
    (value.len(), value.capacity())
}

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

    /// stream 的 bubble 是独立分配：投影到 `nested_units` 时使用的是 clone，
    /// 原件仍由 stream 持有，因此 projected 状态也必须计入。
    fn memory_bytes(&self) -> (usize, usize) {
        let body = self.bubble.text.len()
            + self
                .bubble
                .reasoning
                .as_ref()
                .map_or(0, |block| block.text.len())
            + self.bubble.message_id.as_ref().map_or(0, String::len);
        let reserved = self.bubble.text.capacity()
            + self
                .bubble
                .reasoning
                .as_ref()
                .map_or(0, |block| block.text.capacity())
            + self.bubble.message_id.as_ref().map_or(0, String::capacity);
        (body, reserved)
    }
}

fn nested_units_layer_bytes(detail: &BgLiveDetail) -> (usize, usize) {
    let mut seen = crate::kit::retained_bytes::SeenAllocations::default();
    let reserved =
        crate::kit::retained_bytes::retained_units_bytes(detail.nested_units.iter(), &mut seen);
    // body 按出现次数累计逻辑正文；reserved 才按共享 Arc 去重。两者口径不同。
    let body = detail.nested_units.iter().map(unit_body_bytes).sum();
    (body, reserved)
}

/// 工具累积器实际持有的字段：字符串、`raw_input`、presentation。
///
/// `nested_units` 中的展示卡是另一份投影（独立分配），这里不做“去重”假设。
fn tool_cards_layer_bytes(cards: &Vec<ToolCardAccumulator>) -> (usize, usize) {
    let slot = cards.len() * std::mem::size_of::<ToolCardAccumulator>();
    let mut body = slot;
    let mut reserved = cards.capacity() * std::mem::size_of::<ToolCardAccumulator>();
    for card in cards {
        body += card.tool_id.len()
            + card.tool_name.len()
            + card.input_summary.len()
            + card.output_summary.as_ref().map_or(0, String::len);
        reserved += card.tool_id.capacity()
            + card.tool_name.capacity()
            + card.input_summary.capacity()
            + card.output_summary.as_ref().map_or(0, String::capacity);
        let raw = crate::kit::retained_bytes::json_value_bytes(&card.raw_input);
        body += raw.body;
        reserved += raw.reserved;
        let presentation = crate::kit::retained_bytes::presentation_bytes(&card.presentation);
        body += presentation.body;
        reserved += presentation.reserved;
    }
    (body, reserved)
}

fn unit_body_bytes(unit: &TuiRenderUnit) -> usize {
    match unit {
        TuiRenderUnit::TuiAssistantBubble(bubble) => {
            bubble.text.len()
                + bubble
                    .reasoning
                    .as_ref()
                    .map_or(0, |block| block.text.len())
                + bubble.message_id.as_ref().map_or(0, String::len)
        }
        TuiRenderUnit::TuiUserBubble(bubble) => bubble.text.len(),
        TuiRenderUnit::TuiToolCard(card) => {
            card.tool_id.len()
                + card.tool_name.len()
                + card.input_summary.len()
                + card.output_summary.len()
                + crate::kit::retained_bytes::presentation_bytes(&card.presentation).body
                + card.diff.as_ref().map_or(0, |diff| {
                    diff.path.len()
                        + diff
                            .hunks
                            .iter()
                            .map(|hunk| {
                                hunk.old_range.len()
                                    + hunk.new_range.len()
                                    + hunk.lines.iter().map(|line| line.text.len()).sum::<usize>()
                            })
                            .sum::<usize>()
                })
        }
        TuiRenderUnit::TuiSystemNote(note) => note.text.len(),
        TuiRenderUnit::TuiSystemReminder(reminder) => {
            reminder.summary.len()
                + reminder.body.len()
                + reminder.category.len()
                + reminder.source.len()
        }
        TuiRenderUnit::TuiSubAgentGroup(group) => {
            group.instance_id.len()
                + group.agent_id.len()
                + group.agent_name.len()
                + group.error_reason.as_ref().map_or(0, String::len)
                + group.view_models.iter().map(unit_body_bytes).sum::<usize>()
        }
        TuiRenderUnit::TuiCollapsedGroup(group) => {
            group.title.len() + group.view_models.iter().map(unit_body_bytes).sum::<usize>()
        }
        TuiRenderUnit::TuiDivider(divider) => divider.label.as_ref().map_or(0, String::len),
        TuiRenderUnit::TuiAskUserBlock(block) => {
            block.verb.len()
                + block.question.len()
                + block.result.as_ref().map_or(0, String::len)
                + block.request_id.as_ref().map_or(0, String::len)
                + block.options.iter().map(String::len).sum::<usize>()
                + block
                    .items
                    .iter()
                    .map(|item| item.header.len() + item.answer.len())
                    .sum::<usize>()
                + block.question_ids.iter().map(String::len).sum::<usize>()
        }
        TuiRenderUnit::TuiTodoSummary(summary) => summary.text.len(),
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

/// 当前权威 live 快照显示 running 时建立/恢复 live 证据。
///
/// [Why] 快照会周期性到达，已是 Running 的 detail 不重置 stream——否则正在流式输出的
/// 文本会被切成多段。Unobserved（lost 删除后）或缺失的 detail 则重建为 Running；
/// 既有 tool_cards / nested_units 保留并重新同步，恢复后未完成的工具重新显示为运行中。
pub(crate) fn seed_live_from_running_snapshot(
    task_id: &str,
    kind: &str,
    summary: &str,
    pid: Option<u32>,
) {
    let live = BG_LIVE_DETAIL.state();
    let mut map = live.write();
    let detail = map.entry(task_id.to_string()).or_default();
    detail.kind = kind.to_string();
    detail.summary = summary.to_string();
    detail.pid = pid;
    if detail.status == BgLiveStatus::Running {
        return;
    }
    flush_detail(detail);
    detail.stream = None;
    detail.status = BgLiveStatus::Running;
    sync_tool_units(detail);
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
            .find(|t| t.tool_id == te.tool_id)
        else {
            return;
        };
        if t.finish(te.output_summary.clone(), te.is_error) {
            sync_tool_units(detail);
        }
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
