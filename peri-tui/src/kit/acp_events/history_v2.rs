//! H5c：History V2 状态 → `committed` 前缀投影。
//!
//! V2 激活时，历史正文的唯一来源是 [`HistoryV2State`] 的保留窗口；本模块只做
//! 纯投影，不请求、不合并第二份窗口状态。`committed` 被切成两段：
//!
//! - `[0, HISTORY_V2_PREFIX_LEN)`：由 V2 窗口整体重建，翻页/seek/reopen 都替换它；
//! - 其后：既有 live 路径追加的单元（进行中的 turn、本地回显）。
//!
//! 导航代次（`paging.view_epoch`）变化意味着 seek/reopen 换了一组窗口内容，此时
//! live 尾部已由新窗口覆盖，必须一并丢弃；同代次的 before/after 页合并只替换前缀，
//! 保留 live 尾部。展示单元仍调用历史页同一批构造函数，不复制展示规则。正文只发布
//! descriptor 与占位 note，完整 JSON/正文由详情面板按需经 `_peri/history` 读取。

use std::collections::HashMap;

use peri_acp_types::history_v2::{
    HistoryBodyDescriptor, HistoryEntry, HistorySource as V2Source, HistorySubItem,
};

use super::BridgeState;
#[cfg(test)]
use crate::kit::acp_types::AcpEventData;
use crate::kit::atoms;
use crate::kit::history_detail_binding::{
    DetailDescriptor, HistoryDetailTarget, HistorySource as TuiSource, HistoryV2BodyBinding,
};
use crate::kit::history_v2::body::HistoryV2BodyLocator;
use crate::kit::history_v2::{HistoryV2State, HistoryV2Window, sub_item_descriptor, sub_item_id};
use crate::kit::tui_render_unit::{
    TuiNoteLevel, TuiRenderUnit, TuiSystemNote, TuiToolCard, TuiUserBubble, tui_hash_str,
};

/// 超大工具结果的延迟补读提示；V2 详情面板按 descriptor 读取正文。
pub(crate) const DEFERRED_TOOL_RESULT_TEXT: &str = "结果正文较大，展开后按需加载";

/// 应用一次 V2 整体状态发布；调用方已保证事件串行。
pub(super) fn handle_view(state: &mut BridgeState, v2: &HistoryV2State) {
    let Some(session_id) = v2.session_id.as_deref() else {
        clear_prefix(state);
        return;
    };
    if state.active_session_id != session_id {
        // 迟到的旧会话视图：不得改写当前会话的 committed。
        return;
    }
    let projected = match (v2.is_ready(), v2.window.as_ref()) {
        (true, Some(window)) => project(window),
        // 失败/未就绪：清空 V2 前缀，失败事实由 session load 的失败路径展示。
        _ => Projection::default(),
    };
    let epoch = v2
        .window
        .as_ref()
        .map(|window| window.paging.view_epoch)
        .unwrap_or_default();
    let replace_all = epoch != atoms::HISTORY_V2_VIEW_EPOCH.get();
    if replace_all {
        // 新窗口替换整段历史后，旧 V2 详情的 window/snapshot/bodyRef 全部作废。
        crate::kit::panels::history_detail::close_v2_history_detail();
    }
    let previous = state.committed.clone();
    let prefix_len = replace_prefix(state, projected.units, replace_all);
    atoms::HISTORY_V2_PREFIX_LEN.set(prefix_len);
    atoms::HISTORY_V2_VIEW_EPOCH.set(epoch);
    atoms::HISTORY_DETAIL_TARGETS.set(projected.targets);
    if state.committed != previous {
        atoms::HISTORY_PREPEND_EPOCH.set(atoms::HISTORY_PREPEND_EPOCH.get().wrapping_add(1));
        super::render::push_acp_state(state);
    }
}

fn clear_prefix(state: &mut BridgeState) {
    crate::kit::panels::history_detail::close_v2_history_detail();
    let previous = state.committed.clone();
    let prefix_len = replace_prefix(state, Vec::new(), false);
    atoms::HISTORY_V2_PREFIX_LEN.set(prefix_len);
    atoms::HISTORY_DETAIL_TARGETS.set(Vec::new());
    atoms::HISTORY_V2_VIEW_EPOCH.set(0);
    if state.committed != previous {
        super::render::push_acp_state(state);
    }
}

/// 用 `units` 替换 V2 前缀；`replace_all` 时仍保留其后的 live 尾部。
///
/// seek/reopen 换的是历史页，不是正在输出的 live 内容：未持久化尾部必须留在末尾
/// （设计 §12.2、§9.5）。新页若已包含同一消息，只裁掉重复的尾部单元；无法确认的
/// 流式内容原样保留。`replace_all` 时插入一条 live 标记，提示末尾内容尚未写入历史。
fn replace_prefix(state: &mut BridgeState, units: Vec<TuiRenderUnit>, replace_all: bool) -> usize {
    let previous = atoms::HISTORY_V2_PREFIX_LEN
        .get()
        .min(state.committed.len());
    let suffix: Vec<TuiRenderUnit> = state.committed.iter().skip(previous).cloned().collect();
    let suffix = merge_live_tail(&units, suffix, replace_all);
    let prefix_len = units.len();
    let mut next: im::Vector<TuiRenderUnit> = units.into_iter().collect();
    next.extend(suffix);
    state.committed = next;
    prefix_len
}

/// live 尾部标记；与页面单元分开存放，替换前缀时不会把它算进 `PREFIX_LEN`。
const LIVE_TAIL_MARKER: &str = "实时内容 · 尚未写入历史";
/// 只与新页最后若干单元比对，避免把更早的同文本消息误判为重复。
const TAIL_MATCH_WINDOW: usize = 12;

fn live_marker_unit() -> TuiRenderUnit {
    system_note(LIVE_TAIL_MARKER.to_string(), TuiNoteLevel::Info)
}

fn is_live_marker(unit: &TuiRenderUnit) -> bool {
    matches!(unit, TuiRenderUnit::TuiSystemNote(note) if note.text == LIVE_TAIL_MARKER)
}

/// 保留 live 尾部，并按身份/指纹裁掉新页已经包含的重复前缀。
fn merge_live_tail(
    page: &[TuiRenderUnit],
    suffix: Vec<TuiRenderUnit>,
    replace_all: bool,
) -> Vec<TuiRenderUnit> {
    if suffix.is_empty() {
        return suffix;
    }
    let mut live = suffix;
    let had_marker = live.first().is_some_and(is_live_marker);
    if had_marker {
        live.remove(0);
    }
    if live.is_empty() {
        return live;
    }

    let window_start = page.len().saturating_sub(TAIL_MATCH_WINDOW);
    let mut cursor = window_start;
    let mut duplicate = 0;
    for unit in &live {
        let Some(offset) = page[cursor..]
            .iter()
            .position(|candidate| live_unit_matches(candidate, unit))
        else {
            break;
        };
        cursor += offset + 1;
        duplicate += 1;
    }
    live.drain(..duplicate);
    if live.is_empty() {
        return live;
    }
    if replace_all || had_marker {
        live.insert(0, live_marker_unit());
    }
    live
}

/// 持久条目与 live 单元是否表示同一内容。
///
/// 优先用稳定身份（`messageId` / 工具 `toolCallId`↔`invocationKey`）；缺失时用
/// `(role, text/output)` 指纹。流式 assistant 文本可以是持久页文本的前缀——页更
/// 完整时删除 live 重复；页没有的尾部内容保留。
fn live_unit_matches(page: &TuiRenderUnit, live: &TuiRenderUnit) -> bool {
    match (page, live) {
        (TuiRenderUnit::TuiUserBubble(page), TuiRenderUnit::TuiUserBubble(live)) => {
            same_identity(page.message_id.as_deref(), live.message_id.as_deref())
                || (identity_absent(page.message_id.as_deref(), live.message_id.as_deref())
                    && page.text == live.text)
        }
        (TuiRenderUnit::TuiAssistantBubble(page), TuiRenderUnit::TuiAssistantBubble(live)) => {
            same_identity(page.message_id.as_deref(), live.message_id.as_deref())
                || (identity_absent(page.message_id.as_deref(), live.message_id.as_deref())
                    && assistant_matches(page, live))
        }
        (TuiRenderUnit::TuiToolCard(page), TuiRenderUnit::TuiToolCard(live)) => {
            if !page.tool_id.is_empty() && !live.tool_id.is_empty() {
                page.tool_id == live.tool_id
            } else {
                page.tool_name == live.tool_name && page.input_summary == live.input_summary
            }
        }
        (TuiRenderUnit::TuiSystemNote(page), TuiRenderUnit::TuiSystemNote(live)) => {
            page.text == live.text
        }
        _ => false,
    }
}

fn same_identity(page: Option<&str>, live: Option<&str>) -> bool {
    matches!((page, live), (Some(page), Some(live)) if !live.is_empty() && page == live)
}

/// 至少一方缺少稳定身份时，才允许退回内容指纹。双方身份都存在且不同，
/// 说明是两条不同消息，文本/摘要相同也不能当作重复删除。
fn identity_absent(page: Option<&str>, live: Option<&str>) -> bool {
    match (page, live) {
        (Some(page), Some(live)) => page.is_empty() || live.is_empty(),
        _ => true,
    }
}

fn assistant_matches(
    page: &crate::kit::tui_render_unit::TuiAssistantBubble,
    live: &crate::kit::tui_render_unit::TuiAssistantBubble,
) -> bool {
    let page_reasoning = page
        .reasoning
        .as_ref()
        .map(|block| block.text.as_str())
        .unwrap_or("");
    let live_reasoning = live
        .reasoning
        .as_ref()
        .map(|block| block.text.as_str())
        .unwrap_or("");
    if page.text == live.text && page_reasoning == live_reasoning {
        return true;
    }
    if live.text.is_empty() && live_reasoning.is_empty() {
        return false;
    }
    let text_covered = page.text == live.text || page.text.starts_with(&live.text);
    let reasoning_covered =
        page_reasoning == live_reasoning || page_reasoning.starts_with(live_reasoning);
    text_covered && reasoning_covered
}

#[derive(Default)]
struct Projection {
    units: Vec<TuiRenderUnit>,
    targets: Vec<HistoryDetailTarget>,
}

fn project(window: &HistoryV2Window) -> Projection {
    let mut projection = Projection::default();
    // invocationKey → (entry 下标, 单元下标)：结果可在后续条目原地升级调用卡片。
    let mut calls: HashMap<String, (usize, usize)> = HashMap::new();
    let mut entries: Vec<Vec<TuiRenderUnit>> = Vec::with_capacity(window.items.len());

    for entry in &window.items {
        let entry_index = entries.len();
        entries.push(Vec::new());
        for sub_item in &entry.sub_items {
            project_sub_item(
                window,
                entry,
                sub_item,
                entry_index,
                &mut entries,
                &mut calls,
                &mut projection.targets,
            );
        }
        if entries[entry_index].is_empty()
            && let Some(first) = entry.sub_items.first()
            && let Some((unit, target)) = body_placeholder(window, entry, first)
        {
            // 整条条目只有外置正文（含 opaque）：占位本身就是该条目的展示形式。
            entries[entry_index].push(unit);
            projection.targets.push(target);
        }
    }

    for units in entries {
        projection.units.extend(units);
    }
    if let Some(note) = paging_note(window) {
        projection.units.push(note);
    }
    projection
}

fn project_sub_item(
    window: &HistoryV2Window,
    entry: &HistoryEntry,
    sub_item: &HistorySubItem,
    entry_index: usize,
    entries: &mut [Vec<TuiRenderUnit>],
    calls: &mut HashMap<String, (usize, usize)>,
    targets: &mut Vec<HistoryDetailTarget>,
) {
    match sub_item {
        HistorySubItem::Text(item) => match (&item.text, &item.body) {
            (Some(text), _) if item.role == "user" => {
                entries[entry_index].push(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
                    text.clone(),
                )));
            }
            (Some(text), _) => {
                entries[entry_index].push(super::turn::committed_assistant_unit(text, &None));
            }
            (None, Some(_)) => {
                push_body_placeholder(window, entry, sub_item, &mut entries[entry_index], targets)
            }
            (None, None) => {}
        },
        HistorySubItem::Reasoning(item) => match (&item.text, &item.body) {
            (Some(text), _) => {
                let reasoning = Some(text.clone());
                entries[entry_index].push(super::turn::committed_assistant_unit("", &reasoning));
            }
            (None, Some(_)) => {
                push_body_placeholder(window, entry, sub_item, &mut entries[entry_index], targets)
            }
            (None, None) => {}
        },
        HistorySubItem::ToolCall(item) => {
            let (input_summary, raw_input) = match &item.input {
                Some(input) => (item.title.clone(), input.clone()),
                None => (
                    "输入较大，展开后按需加载".to_string(),
                    serde_json::Value::Null,
                ),
            };
            let card = as_tool_card(super::tool::replay_tool_started_unit(
                &item.invocation_key,
                &item.title,
                &input_summary,
                &raw_input,
                None,
            ));
            let slot = entries[entry_index].len();
            push_tool_card(
                &mut entries[entry_index],
                card,
                targets,
                window,
                entry,
                sub_item,
            );
            calls.insert(item.invocation_key.clone(), (entry_index, slot));
        }
        HistorySubItem::ToolResult(item) => {
            let is_error = item.status != "completed";
            let (summary, body) = match &item.output {
                Some(output) => (output.clone(), None),
                None => (
                    DEFERRED_TOOL_RESULT_TEXT.to_string(),
                    binding_for(window, entry, sub_item),
                ),
            };
            if let Some((call_entry, call_slot)) = calls.get(&item.invocation_key).copied() {
                // 跨条目：按稳定 invocationKey 原地升级更早条目里的调用卡片；渲染
                // 顺序不变（结果子项不再单独占位）。
                let call = as_tool_card(entries[call_entry][call_slot].clone());
                let completed = super::tool::completed_tool_card(&call, &summary, is_error);
                entries[call_entry][call_slot] = TuiRenderUnit::TuiToolCard(completed);
                if let Some(binding) = body {
                    push_target(
                        entries[call_entry][call_slot].content_hash(),
                        binding,
                        window,
                        targets,
                    );
                }
                return;
            }
            let card = as_tool_card(super::tool::replay_tool_started_unit(
                &item.invocation_key,
                "tool",
                "",
                &serde_json::Value::Null,
                None,
            ));
            let completed = super::tool::completed_tool_card(&card, &summary, is_error);
            push_tool_card_with_body(&mut entries[entry_index], completed, body, targets, window);
        }
        HistorySubItem::Reminder(item) | HistorySubItem::Summary(item) => {
            match (&item.text, &item.body) {
                (Some(text), _) => {
                    let text = if item.title.is_empty() {
                        text.clone()
                    } else {
                        format!("{} · {text}", item.title)
                    };
                    entries[entry_index].push(system_note(text, TuiNoteLevel::Info));
                }
                (None, Some(_)) => push_body_placeholder(
                    window,
                    entry,
                    sub_item,
                    &mut entries[entry_index],
                    targets,
                ),
                (None, None) => {}
            }
        }
        HistorySubItem::Attachment(item) => {
            entries[entry_index].push(system_note(
                format!("附件 · {} · {}", item.label, item.media_type),
                TuiNoteLevel::Info,
            ));
        }
        HistorySubItem::Opaque(_) => {
            push_body_placeholder(window, entry, sub_item, &mut entries[entry_index], targets);
        }
    }
}

fn as_tool_card(unit: TuiRenderUnit) -> TuiToolCard {
    match unit {
        TuiRenderUnit::TuiToolCard(card) => card,
        _ => unreachable!("tool projection always receives a tool card"),
    }
}

fn push_tool_card(
    units: &mut Vec<TuiRenderUnit>,
    card: TuiToolCard,
    targets: &mut Vec<HistoryDetailTarget>,
    window: &HistoryV2Window,
    entry: &HistoryEntry,
    sub_item: &HistorySubItem,
) {
    let binding = if sub_item_descriptor(sub_item).is_some() {
        binding_for(window, entry, sub_item)
    } else {
        None
    };
    push_tool_card_with_body(units, card, binding, targets, window);
}

fn push_tool_card_with_body(
    units: &mut Vec<TuiRenderUnit>,
    card: TuiToolCard,
    binding: Option<HistoryV2BodyBinding>,
    targets: &mut Vec<HistoryDetailTarget>,
    window: &HistoryV2Window,
) {
    let unit = TuiRenderUnit::TuiToolCard(card);
    if let Some(binding) = binding {
        push_target(unit.content_hash(), binding, window, targets);
    }
    units.push(unit);
}

fn push_body_placeholder(
    window: &HistoryV2Window,
    entry: &HistoryEntry,
    sub_item: &HistorySubItem,
    units: &mut Vec<TuiRenderUnit>,
    targets: &mut Vec<HistoryDetailTarget>,
) {
    let Some((unit, target)) = body_placeholder(window, entry, sub_item) else {
        return;
    };
    units.push(unit);
    targets.push(target);
}

fn body_placeholder(
    window: &HistoryV2Window,
    entry: &HistoryEntry,
    sub_item: &HistorySubItem,
) -> Option<(TuiRenderUnit, HistoryDetailTarget)> {
    let binding = binding_for(window, entry, sub_item)?;
    let descriptor = window.body_descriptor(&locator(entry, &binding.sub_item_id))?;
    let hash = v2_placeholder_hash(
        &entry.source,
        &entry.entry_id,
        &binding.sub_item_id,
        descriptor.total_bytes.get(),
    );
    let text = format!(
        "超大内容 · {:.1} KiB · 按需加载详情",
        descriptor.total_bytes.get() as f64 / 1024.0
    );
    let unit = TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text,
        level: TuiNoteLevel::Info,
        content_hash: hash,
    });
    let target = HistoryDetailTarget {
        content_hash: hash,
        descriptor: descriptor_for(window, &entry.entry_id, &entry.source, &descriptor, binding),
    };
    Some((unit, target))
}

fn push_target(
    content_hash: u64,
    binding: HistoryV2BodyBinding,
    window: &HistoryV2Window,
    targets: &mut Vec<HistoryDetailTarget>,
) {
    if targets
        .iter()
        .any(|target| target.content_hash == content_hash)
    {
        return;
    }
    let Some(descriptor) = window.body_descriptor(&HistoryV2BodyLocator {
        source: binding.source.clone(),
        entry_id: binding.entry_id.clone(),
        sub_item_id: binding.sub_item_id.clone(),
    }) else {
        return;
    };
    let entry_id = binding.entry_id.clone();
    let source = binding.source.clone();
    targets.push(HistoryDetailTarget {
        content_hash,
        descriptor: descriptor_for(window, &entry_id, &source, &descriptor, binding),
    });
}

fn binding_for(
    window: &HistoryV2Window,
    entry: &HistoryEntry,
    sub_item: &HistorySubItem,
) -> Option<HistoryV2BodyBinding> {
    let descriptor = sub_item_descriptor(sub_item)?;
    Some(HistoryV2BodyBinding {
        session_id: window.session_id.clone(),
        window_id: window.window_id.clone(),
        snapshot_token: window.snapshot_token.clone(),
        read_ticket: window.read_ticket.clone(),
        source: entry.source.clone(),
        entry_id: entry.entry_id.clone(),
        sub_item_id: sub_item_id(sub_item).to_owned(),
        body_ref: descriptor.body_ref.clone(),
        representation: descriptor.representation,
        media_type: descriptor.media_type.clone(),
    })
}

fn descriptor_for(
    window: &HistoryV2Window,
    entry_id: &str,
    source: &V2Source,
    descriptor: &HistoryBodyDescriptor,
    binding: HistoryV2BodyBinding,
) -> DetailDescriptor {
    DetailDescriptor::new(
        map_source(source),
        entry_id.to_string(),
        descriptor.total_bytes.get().max(1),
        descriptor.chunk_bytes,
        window.snapshot_token.clone(),
        window.read_ticket.clone(),
    )
    .with_v2(binding)
}

fn locator(entry: &HistoryEntry, sub_item_id: &str) -> HistoryV2BodyLocator {
    HistoryV2BodyLocator {
        source: entry.source.clone(),
        entry_id: entry.entry_id.clone(),
        sub_item_id: sub_item_id.to_string(),
    }
}

fn v2_placeholder_hash(
    source: &V2Source,
    entry_id: &str,
    sub_item_id: &str,
    total_bytes: u64,
) -> u64 {
    let source = match source {
        V2Source::Own => "own".to_string(),
        V2Source::Inherited(origin) => format!("snapshot:{}", origin.snapshot_id),
    };
    tui_hash_str(&format!(
        "v2|{source}|{entry_id}|{sub_item_id}|{total_bytes}"
    ))
}

fn map_source(source: &V2Source) -> TuiSource {
    match source {
        V2Source::Own => TuiSource::Own,
        V2Source::Inherited(origin) => TuiSource::inherited_with_ancestor(
            origin.snapshot_id.clone(),
            Some(origin.ancestor_session_id.clone()),
        ),
    }
}

fn system_note(text: String, level: TuiNoteLevel) -> TuiRenderUnit {
    let content_hash = tui_hash_str(&text);
    TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text,
        level,
        content_hash,
    })
}

fn paging_note(window: &HistoryV2Window) -> Option<TuiRenderUnit> {
    use crate::kit::history_v2::paging::HistoryV2PagingStatus;
    if window.paging.live.detached {
        return Some(system_note(
            "实时流缺口，需重新加载 · 已显示内容可能不完整".to_string(),
            TuiNoteLevel::Warning,
        ));
    }
    match &window.paging.status {
        HistoryV2PagingStatus::Ready => None,
        HistoryV2PagingStatus::Retryable { reason } => Some(system_note(
            format!("历史加载失败 · {reason} · 继续滚动重试"),
            TuiNoteLevel::Warning,
        )),
        HistoryV2PagingStatus::ReopenRequired { reason } => Some(system_note(
            format!("历史窗口已失效 · {reason} · 重新打开后继续"),
            TuiNoteLevel::Warning,
        )),
    }
    .or_else(|| {
        (!window.gaps.is_empty()).then(|| {
            system_note(
                "历史存在未加载区间 · 滚动到缺口处继续读取".to_string(),
                TuiNoteLevel::Info,
            )
        })
    })
}

/// 测试用：直接构造 V2 视图事件，避免测试依赖 consumer 的通道。
#[cfg(test)]
pub(crate) fn view_event(state: HistoryV2State) -> AcpEventData {
    AcpEventData::HistoryV2ViewChanged {
        state: Box::new(state),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kit::tui_render_unit::{
        FoldState, TuiAssistantBubble, TuiToolCard, TuiToolPresentation, TuiUserBubble,
    };

    fn user(text: &str, id: Option<&str>) -> TuiRenderUnit {
        let mut bubble = TuiUserBubble::new(text.to_owned());
        bubble.message_id = id.map(str::to_owned);
        TuiRenderUnit::TuiUserBubble(bubble)
    }

    fn assistant(text: &str, id: Option<&str>) -> TuiRenderUnit {
        TuiRenderUnit::TuiAssistantBubble(
            TuiAssistantBubble {
                text: text.to_owned(),
                reasoning: None,
                message_id: id.map(str::to_owned),
                started_at: None,
                duration_ms: None,
                content_hash: 0,
            }
            .into(),
        )
    }

    fn tool(id: &str, name: &str, summary: &str) -> TuiRenderUnit {
        TuiRenderUnit::TuiToolCard(TuiToolCard {
            tool_id: id.to_owned(),
            tool_name: name.to_owned(),
            input_summary: summary.to_owned(),
            output_summary: String::new(),
            is_error: false,
            is_running: false,
            running_duration_ms: None,
            completed_duration_ms: None,
            diff: None,
            presentation: TuiToolPresentation::Generic,
            fold: FoldState::Collapsed,
            user_modified: false,
            tool_calls_count: 0,
            content_hash: 0,
        })
    }

    #[test]
    fn same_text_with_distinct_identity_is_not_deduped() {
        assert!(!live_unit_matches(
            &user("继续", Some("msg-2")),
            &user("继续", Some("msg-1")),
        ));
    }

    #[test]
    fn same_text_without_identity_still_dedupes_local_echo() {
        assert!(live_unit_matches(
            &user("继续", Some("msg-1")),
            &user("继续", None),
        ));
    }

    #[test]
    fn same_assistant_text_with_distinct_identity_is_not_deduped() {
        assert!(!live_unit_matches(
            &assistant("继续", Some("msg-2")),
            &assistant("继续", Some("msg-1")),
        ));
    }

    #[test]
    fn same_tool_summary_with_distinct_invocation_is_not_deduped() {
        assert!(!live_unit_matches(
            &tool("call-2", "Bash", "ls"),
            &tool("call-1", "Bash", "ls"),
        ));
    }

    #[test]
    fn same_tool_invocation_is_deduped() {
        assert!(live_unit_matches(
            &tool("call-1", "Bash", "ls"),
            &tool("call-1", "Bash", "pwd"),
        ));
    }
}
