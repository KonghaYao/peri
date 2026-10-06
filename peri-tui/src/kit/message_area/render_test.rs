//! Tests —— [Slice 3] 统一网格 / user entry / 垂直节奏 / 各变体行渲染器。
//!
//! 约定：
//! - 渲染宽度一律用 `GridSpec::grid_for(term)`（content = term-6），wrap_map
//!   以 `term` 为宽构建——行渲染器保证「行宽 ≤ term_width - 1 < term，
//!   不折行」（metadata 右对齐到消息区右缘，§6.4）。
//! - 前缀结构（§3.1）：首行 `[outer 空][accent 符号][gap]`，续行 `[outer 空][│][gap]`。

use super::helpers::sym;
use super::*;
use crate::kit::diff_parser::parse_unified_diff;
use crate::kit::message_area::selection::build_wrap_map;
use crate::kit::tui_render_unit::{
    EntryStatus, FoldState, TuiAskUserBlock, TuiAssistantBubble, TuiCollapsedGroup, TuiDiffBlock,
    TuiDivider, TuiNoteLevel, TuiReasoningBlock, TuiRenderUnit, TuiSkillPresentation,
    TuiSubAgentGroup, TuiSystemNote, TuiSystemReminder, TuiTodoChange, TuiTodoChangeKind,
    TuiTodoItem, TuiTodoPresentation, TuiTodoStatus, TuiTodoSummary, TuiToolCard,
    TuiToolPresentation, TuiUserBubble,
};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

/// 拼接渲染行全部 span 文本。
fn line_text(line: &Line<'static>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// 拼接所有行文本。
fn all_text(lines: &[Line<'static>]) -> String {
    lines.iter().map(line_text).collect::<Vec<_>>().join("\n")
}

/// 空白分隔行判定：无 spans，或 spans 全部为网格前缀列（outer 空 / `│` / gap），
/// 无正文内容——§3.2 turn 节拍空行现在带竖线前缀，不再是 `spans.is_empty()`。
fn is_blank_separator(line: &Line<'static>) -> bool {
    line.spans.iter().all(|s| {
        let c = s.content.as_ref();
        c.trim().is_empty() || c == "\u{2502}"
    })
}

/// 行是否有正文内容（前缀列不算正文）。
fn has_body_content(line: &Line<'static>) -> bool {
    !is_blank_separator(line)
}

/// 空白分隔行 + 竖线前缀（§3.2：turn 节拍空行不断链左缘时间线）。
fn is_rail_blank(line: &Line<'static>) -> bool {
    is_blank_separator(line) && line.spans.iter().any(|s| s.content.as_ref() == "\u{2502}")
}

/// 首个非空渲染行的文本（跳过 leading 空行）。
fn header_of(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .find(|l| has_body_content(l))
        .map(line_text)
        .unwrap_or_default()
}

/// reasoning 正文行（竖线前缀）计数——排除首个非空 header 行：header 与
/// 正文统一竖线后（用户需求），仅按竖线计数会把 header 混入正文。
fn body_vline_count(lines: &[Line<'static>]) -> usize {
    lines
        .iter()
        .skip_while(|l| !has_body_content(l))
        .skip(1)
        .filter(|l| l.spans.iter().any(|s| s.content.as_ref() == "\u{2502}"))
        .count()
}

/// 第 n 个非空行（0-based）。
fn nth_nonempty_line(lines: &[Line<'static>], n: usize) -> Line<'static> {
    lines
        .iter()
        .filter(|l| has_body_content(l))
        .nth(n)
        .cloned()
        .unwrap_or_default()
}

/// running 符号帧判定：unicode 终端 braille 动画帧或 ASCII 降级 `*`（§8.2）。
/// 动画帧由壁钟 tick 推进，测试不依赖具体帧值。
fn is_running_frame(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(
            c,
            '⠋' | '⠙' | '⠹' | '⠸' | '⠼' | '⠴' | '⠦' | '⠧' | '⠇' | '⠏' | '*'
        )
    })
}

/// 构造 reasoning 块（默认 completed + collapsed）。
fn reasoning_block(text: &str) -> TuiReasoningBlock {
    TuiReasoningBlock {
        text: text.to_string(),
        fold: FoldState::Collapsed,
        status: EntryStatus::Completed,
        is_running: false,
        started_at: None,
        duration_ms: Some(12_000),
    }
}

fn tool_card(tool_name: &str, summary: &str, is_error: bool, is_running: bool) -> TuiToolCard {
    TuiToolCard {
        tool_id: format!("tc-{tool_name}"),
        tool_name: tool_name.to_string(),
        input_summary: summary.to_string(),
        output_summary: if is_error {
            "Error: something went wrong".into()
        } else {
            "done".into()
        },
        is_error,
        is_running,
        running_duration_ms: None,
        completed_duration_ms: if is_running { None } else { Some(400) },
        diff: None,
        presentation: TuiToolPresentation::Generic,
        fold: if is_error {
            FoldState::Expanded
        } else if is_running {
            FoldState::Preview
        } else {
            FoldState::Collapsed
        },
        user_modified: false,
        tool_calls_count: 0,
        content_hash: 1,
    }
}

#[path = "render_entry_layout_test.rs"]
mod entry_layout_tests;

#[path = "render_user_entry_test.rs"]
mod user_entry_tests;

#[path = "render_reasoning_test.rs"]
mod reasoning_tests;

fn edit_card_with_diff(text: &str) -> TuiToolCard {
    let mut card = tool_card("Edit", "src/main.rs", false, false);
    card.output_summary = text.to_string();
    card.diff = parse_unified_diff(text, Some("src/main.rs"));
    card.fold = FoldState::Expanded;
    card.recompute_hash();
    card
}

#[path = "render_tool_activity_test.rs"]
mod tool_activity_tests;

fn subagent_group(
    children: im::Vector<TuiRenderUnit>,
    is_running: bool,
    is_error: bool,
    error_reason: Option<&str>,
) -> TuiRenderUnit {
    TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        instance_id: "instance-agent-1".into(),
        agent_id: "agent-1".into(),
        agent_name: "Agent explorer".into(),
        view_models: children,
        collapsed: false,
        is_running,
        is_error,
        error_reason: error_reason.map(String::from),
        fold: FoldState::Collapsed,
        user_modified: false,
        content_hash: 0,
    })
}

#[path = "render_subagent_groups_test.rs"]
mod subagent_groups_tests;

fn assistant_bubble_with_text(text: &str, hash: u64) -> TuiRenderUnit {
    TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
            started_at: None,
            duration_ms: None,
            text: text.to_string(),
            reasoning: None,
            message_id: None,
            content_hash: hash,
        }
        .into(),
    )
}

#[path = "render_markdown_layout_test.rs"]
mod markdown_layout_tests;

fn pending_permission_block() -> TuiAskUserBlock {
    let mut b = TuiAskUserBlock {
        items: vec![],
        is_error: false,
        kind: crate::kit::tui_render_unit::InteractionKind::Permission,
        pending: true,
        verb: "Bash".into(),
        question: "Bash wants to run: cargo test".into(),
        options: vec!["Allow once".into(), "Deny".into()],
        result: None,
        request_id: Some("rid-1".into()),
        owner: None,
        question_ids: vec![],
        fold: FoldState::Expanded,
        user_modified: false,
        content_hash: 0,
    };
    b.recompute_hash();
    b
}

fn completed_block(result: &str) -> TuiAskUserBlock {
    let mut b = pending_permission_block();
    b.pending = false;
    b.result = Some(result.to_string());
    b.fold = FoldState::Collapsed;
    b.recompute_hash();
    b
}

#[path = "render_interaction_test.rs"]
mod interaction_tests;

#[path = "render_semantic_copy_test.rs"]
mod semantic_copy_tests;
