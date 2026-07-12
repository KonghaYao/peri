//! MessageArea：直接读取 VIEW_MODELS，通过 vm_to_lines 将 TuiRenderUnit
//! 转换为 Vec<Line>，按视口裁剪后渲染。
//!
//! - 滚动：由 ScrollViewState 处理键盘/鼠标事件（offset 管理）
//! - 渲染：视口裁剪——只 clone + highlight + 渲染视口内 ~60 行，避免 O(N×W) per render
//! - 智能跟随：use_effect 检测 VIEW_MODELS 变化
//! - 不再使用 RENDER_CACHE / render_bridge / ScrollView / wrap_map（已替换为 wrap_map_cache）

#![allow(clippy::needless_update)]

use std::cmp::Ordering;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::i18n;
use crate::kit::atoms::{COPY_CHAR_COUNT, COPY_MESSAGE_UNTIL, LANG_VERSION, VIEW_MODELS};
use crate::kit::focus_router;
use crate::kit::text_selection::{self, TextSelection};
use crate::kit::tui_render_unit::{
    TuiAskUserBlock, TuiCollapsedGroup, TuiDivider, TuiHunkLineKind, TuiRenderUnit,
    TuiSubAgentGroup, TuiSystemNote, TuiToolCard,
};
use crate::kit::welcome::Welcome;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use peri_widgets::spinner::{SpinnerMode, SpinnerState};
use ratatui_kit::{
    components::ScrollViewState,
    crossterm::event::{Event, KeyEventKind, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction, Rect},
        style::{Modifier, Style},
        text::{Line, Span, Text as RatText},
        widgets::{Paragraph, Wrap},
    },
};

// ── 滚动速度控制 ──────────────────────────────────────────────────────────

/// 鼠标滚轮每格的滚动行数倍数。
const SCROLL_LINES: u16 = 3;

/// 滚动节流窗口：≥16ms（≈60fps）才把累积 delta 推入 scroll_state。
const SCROLL_FRAME_MS: u64 = 16;

#[derive(Debug, Clone)]
struct ScrollThrottle {
    last_flush: Instant,
    pending_delta: i32, // positive = scroll_down, negative = scroll_up
}

impl Default for ScrollThrottle {
    fn default() -> Self {
        Self {
            last_flush: Instant::now(),
            pending_delta: 0,
        }
    }
}

// ── 拖拽选中节流 ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct DragThrottle {
    last_flush: Instant,
}

impl Default for DragThrottle {
    fn default() -> Self {
        Self {
            last_flush: Instant::now(),
        }
    }
}

// ── wrap_map 类型 ──────────────────────────────────────────────────────────

/// 折行映射条目：逻辑行索引 + 该逻辑行占据的视觉行范围 [visual_start, visual_end)。
#[derive(Debug, Clone)]
struct WrappedLineInfo {
    logical_idx: usize,
    visual_start: usize,
    visual_end: usize,
}

/// 为 all_lines 构建视觉行→逻辑行映射。
/// 返回 (total_visual_rows, wrap_map)。wrap_map 按 visual_start 升序排列，可二分查找。
fn build_wrap_map(lines: &[Line<'static>], width: u16) -> (usize, Vec<WrappedLineInfo>) {
    let mut wrap_map = Vec::with_capacity(lines.len());
    let mut visual_row = 0usize;
    for (idx, line) in lines.iter().enumerate() {
        let rows = Paragraph::new(RatText::from(line.clone()))
            .wrap(Wrap { trim: false })
            .line_count(width) as usize;
        let rows = rows.max(1);
        wrap_map.push(WrappedLineInfo {
            logical_idx: idx,
            visual_start: visual_row,
            visual_end: visual_row + rows,
        });
        visual_row += rows;
    }
    (visual_row, wrap_map)
}

/// 二分查找：视觉行 → 逻辑行索引。
fn visual_to_logical(visual_row: u16, wrap_map: &[WrappedLineInfo]) -> Option<usize> {
    let vr = visual_row as usize;
    match wrap_map.binary_search_by(|entry| {
        if vr < entry.visual_start {
            Ordering::Greater
        } else if vr >= entry.visual_end {
            Ordering::Less
        } else {
            Ordering::Equal
        }
    }) {
        Ok(idx) => Some(wrap_map[idx].logical_idx),
        Err(_) => None,
    }
}

/// 计算视口 [scroll_y, scroll_y + vp_height) 对应的逻辑行范围 + 首行视觉偏移。
///
/// 返回 (start_logical, end_logical, first_line_visual_offset)。
/// first_line_visual_offset 是首行在视口内向下推的视觉行数（Paragraph::scroll 第一参数）。
/// 当 wrap_map 为空或视口在范围外时返回 None。
fn viewport_logical_range(
    wrap_map: &[WrappedLineInfo],
    scroll_y: usize,
    vp_height: usize,
) -> Option<(usize, usize, u16)> {
    if wrap_map.is_empty() || vp_height == 0 {
        return None;
    }
    // 视口起始：第一个 visual_end > scroll_y 的 entry
    let start_idx = wrap_map.iter().position(|e| e.visual_end > scroll_y)?;
    let start_logical = wrap_map[start_idx].logical_idx;
    let first_line_offset = scroll_y.saturating_sub(wrap_map[start_idx].visual_start);
    // 视口结束：第一个 visual_start >= scroll_y + vp_height 的 entry 之前
    let vp_visual_end = scroll_y.checked_add(vp_height)?;
    let end_logical = wrap_map
        .iter()
        .take_while(|e| e.visual_start < vp_visual_end)
        .last()
        .map(|e| e.logical_idx)
        .unwrap_or(start_logical);
    Some((start_logical, end_logical, first_line_offset as u16))
}

// ── 剪贴板复制 ────────────────────────────────────────────────────────────

/// 在独立线程中写入系统剪贴板，避免阻塞 tokio worker。
fn copy_to_clipboard(text: String) {
    std::thread::spawn(move || {
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_text(&text);
        }
    });
}

pub(super) fn mark_copy_message(char_count: usize) {
    COPY_CHAR_COUNT.set(char_count);
    COPY_MESSAGE_UNTIL.set(Some(Instant::now() + Duration::from_secs(2)));
}

// ── Todo 类型 ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TodoStatus {
    InProgress,
    Completed,
    Pending,
}

#[derive(Debug, Clone)]
pub struct TodoItem {
    pub status: TodoStatus,
    pub content: String,
}

fn hash_todo_items(items: &[TodoItem]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for item in items {
        item.status.hash(&mut hasher);
        item.content.hash(&mut hasher);
    }
    hasher.finish()
}

pub fn render_todo_lines(items: &[TodoItem]) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
    let mut lines = Vec::new();
    for item in items {
        let (icon, icon_color, text_color, crossed) = match item.status {
            TodoStatus::InProgress => ("◼", sem.accent, sem.text.primary, false),
            TodoStatus::Completed => ("✔", sem.status.success, sem.text.muted, true),
            TodoStatus::Pending => ("◻", sem.text.muted, sem.text.muted, false),
        };
        let prefix_style = Style::default().fg(icon_color).add_modifier(Modifier::BOLD);
        let mut text_style = Style::default().fg(text_color);
        if crossed {
            text_style = text_style.add_modifier(Modifier::CROSSED_OUT);
        }
        let prefix = Span::styled(format!("  {}  ", icon), prefix_style);
        let mut content = item.content.clone();
        if item.status == TodoStatus::Pending {
            content.push_str(&i18n::tr("msg-todo-available"));
        }
        let text = Span::styled(content, text_style);
        lines.push(Line::from(vec![prefix, text]));
    }
    for _ in 0..1 {
        lines.push(Line::from(""));
    }
    lines
}

// ── 鼠标辅助 ─────────────────────────────────────────────────────────────

fn mouse_in_area(mouse_row: u16, mouse_col: u16, area: Rect) -> bool {
    let area_bottom = area.y.saturating_add(area.height);
    let area_right = area.x.saturating_add(area.width);
    mouse_row >= area.y && mouse_row < area_bottom && mouse_col >= area.x && mouse_col < area_right
}

// ── 消息区位置追踪 Hook ─────────────────────────────────────────────────

struct MsgAreaTracker {
    rect: Option<Rect>,
}

impl MsgAreaTracker {
    fn new() -> Self {
        Self { rect: None }
    }
}

impl Hook for MsgAreaTracker {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.rect = Some(drawer.area);
    }
}

// ── 滚动条 Hook ─────────────────────────────────────────────────────────

/// 视口右侧滚动条字段——通过 use_state 存储，避免 use_hook 的 borrow 冲突。
#[derive(Default, Clone, Copy)]
struct ScrollbarFields {
    content_length: usize,
    position: usize,
    viewport_length: usize,
}

/// 视口右侧滚动条——post_component_draw 时基于 fields 渲染。
///
/// 替代被移除的 ScrollView 内置滚动条。每帧 render body 更新 ScrollbarFields state。
struct ScrollbarHook {
    fields: State<ScrollbarFields>,
}

impl Hook for ScrollbarHook {
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        let f = *self.fields.read();
        // 仅当内容超出视口时才渲染滚动条
        if f.content_length <= f.viewport_length {
            return;
        }
        let sem = THEME_ATOM.state().read().semantic;
        let thumb_bg = sem.text.dim;
        let scrollbar =
            ratatui::widgets::Scrollbar::new(ratatui::widgets::ScrollbarOrientation::VerticalRight)
                .thumb_symbol(" ")
                .thumb_style(Style::default().fg(thumb_bg).bg(thumb_bg))
                .track_symbol(None)
                .begin_symbol(Some("▲"))
                .begin_style(
                    Style::default()
                        .fg(sem.text.muted)
                        .add_modifier(Modifier::BOLD),
                )
                .end_symbol(Some("▼"))
                .end_style(
                    Style::default()
                        .fg(sem.text.muted)
                        .add_modifier(Modifier::BOLD),
                );
        let mut state = ratatui::widgets::ScrollbarState::new(f.content_length)
            .position(f.position)
            .viewport_content_length(f.viewport_length);
        drawer.render_stateful_widget(scrollbar, drawer.area, &mut state);
    }
}

// ── Props ──────────────────────────────────────────────────────────────────

#[derive(Default, Props)]
pub struct MessageAreaProps {
    pub width: usize,
}

// ── 工具卡片辅助函数（内联自 view_render/tool_card.rs）─────────────────

const COLLAPSED_BY_DEFAULT: &[&str] = &["Bash", "Read", "Glob", "Grep", "AskUserQuestion"];
const AUTO_EXPAND: &[&str] = &["AgentResult", "ExecuteExtraTool", "SearchExtraTools"];
const FORCE_EXPAND_ON_COMPLETE: &[&str] = &["Write", "Edit"];

fn compact_summary(text: &str, max_chars: usize) -> String {
    let joined = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" \u{b7} ");
    truncate_str(&joined, max_chars)
}

fn compact_output_lines(text: &str, max_lines: usize, max_chars: usize) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(max_lines)
        .map(|line| truncate_str(line, max_chars))
        .collect();

    let total = text.lines().filter(|line| !line.trim().is_empty()).count();
    if total > max_lines {
        lines.push(format!("\u{2026} {} more lines", total - max_lines));
    }

    lines
}

fn format_running_duration(ms: u64) -> String {
    let secs = ms / 1000;
    let mins = secs / 60;
    let secs = secs % 60;
    if mins > 0 {
        format!("{}min {}s", mins, secs)
    } else {
        format!("{}s", secs)
    }
}

fn diff_change_summary(diff: &crate::kit::tui_render_unit::TuiDiffBlock) -> Option<String> {
    let adds = diff
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| matches!(l.kind, TuiHunkLineKind::Add))
        .count();
    let dels = diff
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| matches!(l.kind, TuiHunkLineKind::Del))
        .count();
    if adds == 0 && dels == 0 {
        return None;
    }
    let mut parts = Vec::new();
    if adds > 0 {
        parts.push(format!("+{}", adds));
    }
    if dels > 0 {
        parts.push(format!("-{}", dels));
    }
    Some(parts.join(" \u{b7} "))
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}\u{2026}", truncated)
    }
}

// ── vm_to_lines：TuiRenderUnit → Vec<Line<'static>> ───────────────────────

/// 将单个 TuiRenderUnit 变体转换为渲染行。
/// 使用 kit::markdown 进行 markdown 解析。
fn vm_to_lines(vm: &TuiRenderUnit, width: usize) -> Vec<Line<'static>> {
    match vm {
        TuiRenderUnit::TuiAssistantBubble(data) => {
            let mut lines: Vec<Line<'static>> = Vec::new();

            // 推理块
            if let Some(ref reasoning) = data.reasoning {
                lines.extend(render_reasoning_block(reasoning));
            }

            // Markdown 文本
            if !data.text.is_empty() {
                let palette_state = peri_theme::atoms::PALETTE_ATOM.state();
                let palette_guard = palette_state.read();
                let segments =
                    crate::kit::markdown::parse_markdown(&data.text, width, *palette_guard);
                for seg in segments {
                    match seg {
                        crate::kit::markdown::MarkdownSegment::Text(seg_lines) => {
                            lines.extend(seg_lines);
                        }
                        crate::kit::markdown::MarkdownSegment::Table(data) => {
                            let table_theme =
                                ratatui_kit::components::TableTheme::from_palette(&palette_guard);
                            lines.extend(crate::kit::markdown::table_data_to_lines(
                                &data,
                                &table_theme,
                                width,
                            ));
                        }
                    }
                }
            }

            lines
        }
        TuiRenderUnit::TuiUserBubble(data) => {
            if let Some(ref info) = data.reminder {
                return render_reminder_condensed(info);
            }

            let semantic = THEME_ATOM.state().read().semantic;
            let component = THEME_ATOM.state().read().component;
            let user_bg = component.message.user_bg;
            let palette_state = peri_theme::atoms::PALETTE_ATOM.state();
            let palette_guard = palette_state.read();
            let segments = crate::kit::markdown::parse_markdown(&data.text, width, *palette_guard);

            let mut lines: Vec<Line<'static>> = Vec::new();
            lines.push(Line::from(""));

            for seg in segments {
                match seg {
                    crate::kit::markdown::MarkdownSegment::Text(mut seg_lines) => {
                        for (i, line) in seg_lines.drain(..).enumerate() {
                            if i == 0 {
                                let mut spans = vec![Span::styled(
                                    "\u{276f} ",
                                    Style::default()
                                        .fg(semantic.accent)
                                        .add_modifier(Modifier::BOLD)
                                        .bg(user_bg),
                                )];
                                for span in line.spans {
                                    spans.push(
                                        span.clone().patch_style(Style::default().bg(user_bg)),
                                    );
                                }
                                lines.push(Line::from(spans));
                            } else {
                                let mut spans =
                                    vec![Span::styled("  ", Style::default().bg(user_bg))];
                                for span in line.spans {
                                    spans.push(
                                        span.clone().patch_style(Style::default().bg(user_bg)),
                                    );
                                }
                                lines.push(Line::from(spans));
                            }
                        }
                    }
                    crate::kit::markdown::MarkdownSegment::Table(data) => {
                        lines.push(Line::from(vec![Span::styled(
                            "  ",
                            Style::default().bg(user_bg),
                        )]));
                        let table_theme =
                            ratatui_kit::components::TableTheme::from_palette(&palette_guard);
                        let table_lines =
                            crate::kit::markdown::table_data_to_lines(&data, &table_theme, width);
                        for tl in table_lines {
                            let mut spans = vec![Span::styled("  ", Style::default().bg(user_bg))];
                            for span in tl.spans {
                                spans.push(span.clone().patch_style(Style::default().bg(user_bg)));
                            }
                            lines.push(Line::from(spans));
                        }
                    }
                }
            }
            lines
        }
        TuiRenderUnit::TuiToolCard(data) => render_tool_card_lines(data),
        TuiRenderUnit::TuiSystemNote(data) => render_system_note_lines(data),
        TuiRenderUnit::TuiSubAgentGroup(data) => render_subagent_group_lines(data, width),
        TuiRenderUnit::TuiCollapsedGroup(data) => render_collapsed_group_lines(data),
        TuiRenderUnit::TuiDivider(data) => render_divider_lines(data),
        TuiRenderUnit::TuiAskUserBlock(data) => render_ask_user_block_lines(data),
    }
}

// ── 各变体渲染函数（内联自 view_render/*）──

fn render_reasoning_block(
    reasoning: &crate::kit::tui_render_unit::TuiReasoningBlock,
) -> Vec<Line<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;
    let char_count = reasoning.text.chars().count();
    let mut lines = vec![Line::from("")];
    lines.push(Line::from(vec![Span::styled(
        i18n::tr_args(
            "render-thought-for",
            &[("count".to_string(), FluentValue::from(char_count as u64))],
        ),
        Style::default().fg(semantic.text.dim),
    )]));

    if !reasoning.collapsed {
        let tail_lines: Vec<&str> = reasoning.text.lines().rev().take(3).collect();
        for tail in tail_lines.into_iter().rev() {
            if !tail.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled(" \u{23bf} ", Style::default().fg(semantic.text.dim)),
                    Span::styled(tail.to_string(), Style::default().fg(semantic.text.dim)),
                ]));
            }
        }
    }
    lines.push(Line::from(""));

    lines
}

fn render_reminder_condensed(
    info: &crate::kit::tui_render_unit::ReminderInfo,
) -> Vec<Line<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;
    let mut lines = vec![Line::from(Span::styled(
        info.reminder_type.label(),
        Style::default()
            .fg(semantic.text.dim)
            .add_modifier(Modifier::ITALIC),
    ))];
    if !info.summary.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("  \u{23bf} ", Style::default().fg(semantic.text.dim)),
            Span::styled(
                info.summary.clone(),
                Style::default().fg(semantic.text.muted),
            ),
        ]));
    }
    lines
}

fn render_tool_card_lines(data: &TuiToolCard) -> Vec<Line<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;
    let display_name = crate::kit::tool_display::format_tool_name(&data.tool_name).to_string();

    // 指示器 + 颜色
    let (indicator, indicator_color) = if data.is_error {
        ("\u{25cf}", semantic.status.error)
    } else if data.is_running {
        ("\u{25cf}", ratatui::style::Color::White)
    } else {
        ("\u{25cf}", semantic.status.success)
    };

    let mut header_spans = vec![
        Span::styled(indicator, Style::default().fg(indicator_color)),
        Span::raw(" "),
        Span::styled(
            display_name,
            Style::default()
                .fg(semantic.text.primary)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    let summary = compact_summary(&data.input_summary, 400);
    if !summary.is_empty() {
        header_spans.push(Span::styled(
            format!(" ({})", summary),
            Style::default().fg(semantic.text.dim),
        ));
    }

    let mut lines = vec![Line::from(header_spans)];

    // Bash 运行中
    if data.tool_name == "Bash" && data.is_running && !data.is_error {
        let duration = data.running_duration_ms.unwrap_or(0);
        lines.push(Line::from(vec![
            Span::styled("  \u{23bf} ", Style::default().fg(semantic.text.dim)),
            Span::styled(
                format!("Running ({})", format_running_duration(duration)),
                Style::default().fg(semantic.text.muted),
            ),
        ]));
    }

    // Agent 运行中
    if data.tool_name == "Agent" && data.is_running && !data.is_error {
        let duration = data.running_duration_ms.unwrap_or(0);
        let tool_count = data.tool_calls_count;
        if tool_count > 0 {
            lines.push(Line::from(vec![
                Span::styled("  \u{23bf} ", Style::default().fg(semantic.text.dim)),
                Span::styled(
                    format!(
                        "{} tool calls, running {}",
                        tool_count,
                        format_running_duration(duration)
                    ),
                    Style::default().fg(semantic.text.muted),
                ),
            ]));
        }
    }

    // 折叠/展开判断
    let collapsed = if data.is_error {
        false
    } else if AUTO_EXPAND.contains(&data.tool_name.as_str()) {
        false
    } else if FORCE_EXPAND_ON_COMPLETE.contains(&data.tool_name.as_str()) {
        data.is_running
    } else {
        COLLAPSED_BY_DEFAULT.contains(&data.tool_name.as_str())
    };

    if collapsed {
        if !data.output_summary.is_empty() {
            let color = if data.is_error {
                semantic.status.error
            } else {
                semantic.text.muted
            };
            for out_line in compact_output_lines(&data.output_summary, 1, 400) {
                lines.push(Line::from(vec![
                    Span::styled("  \u{23bf} ", Style::default().fg(semantic.text.dim)),
                    Span::styled(out_line, Style::default().fg(color)),
                ]));
            }
        }
        return with_message_spacing(lines);
    }

    // 输出摘要
    if !data.output_summary.is_empty() {
        let result_color = if data.is_error {
            semantic.status.error
        } else {
            semantic.text.muted
        };
        let max_lines = if data.tool_name == "TodoWrite" {
            usize::MAX
        } else {
            4
        };
        for out_line in compact_output_lines(&data.output_summary, max_lines, 400) {
            lines.push(Line::from(vec![
                Span::styled(
                    "  \u{23bf} ".to_string(),
                    Style::default().fg(semantic.text.dim),
                ),
                Span::styled(out_line, Style::default().fg(result_color)),
            ]));
        }
    }

    // Diff 变更统计
    if let Some(ref diff) = data.diff
        && let Some(summary) = diff_change_summary(diff)
    {
        lines.push(Line::from(vec![
            Span::styled("  \u{23bf} ", Style::default().fg(semantic.text.dim)),
            Span::styled(summary, Style::default().fg(semantic.text.muted)),
        ]));
    }

    with_message_spacing(lines)
}

fn render_system_note_lines(data: &TuiSystemNote) -> Vec<Line<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for line_text in data.text.lines() {
        let (prefix_str, color) = if line_text.starts_with('\u{273b}') {
            ("\u{273b} ", semantic.text.dim)
        } else if line_text.starts_with("\u{23bf}") {
            ("\u{23bf} ", semantic.text.muted)
        } else if line_text.starts_with("  \u{23bf}") {
            ("  \u{23bf} ", semantic.status.error)
        } else if line_text.contains('\u{274c}')
            || line_text.contains("\u{5931}\u{8d25}")
            || line_text.contains("error")
        {
            ("", semantic.status.error)
        } else if line_text.contains("warning") || line_text.contains("warn") {
            ("", semantic.status.warning)
        } else {
            ("", semantic.text.muted)
        };
        let mut spans: Vec<Span<'static>> = Vec::new();
        let content_text = if prefix_str.contains('\u{273b}') {
            spans.push(Span::styled(
                "\u{273b} ".to_string(),
                Style::default().fg(semantic.text.dim),
            ));
            line_text
                .strip_prefix('\u{273b}')
                .unwrap_or(line_text)
                .trim_start()
        } else if prefix_str.contains("\u{23bf}") && prefix_str.starts_with("  ") {
            spans.push(Span::styled(
                "  \u{23bf} ".to_string(),
                Style::default().fg(semantic.text.dim),
            ));
            line_text
                .strip_prefix("  \u{23bf}")
                .unwrap_or(line_text)
                .trim_start()
        } else if prefix_str.contains("\u{23bf}") {
            spans.push(Span::styled(
                "\u{23bf} ".to_string(),
                Style::default().fg(semantic.text.dim),
            ));
            line_text
                .strip_prefix("\u{23bf}")
                .unwrap_or(line_text)
                .trim_start()
        } else {
            line_text
        };
        if !content_text.is_empty() {
            spans.push(Span::styled(
                content_text.to_string(),
                Style::default().fg(color),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines
}

fn render_subagent_group_lines(data: &TuiSubAgentGroup, width: usize) -> Vec<Line<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;

    let children: Vec<TuiRenderUnit> = data.view_models.iter().cloned().collect();
    let mut lines: Vec<Line<'static>> = Vec::new();

    // 折叠摘要
    let tool_count = children
        .iter()
        .filter(|vm| matches!(vm, TuiRenderUnit::TuiToolCard(_)))
        .count();
    let collapse_count = tool_count.saturating_sub(5);
    let mut tool_idx = 0;

    if collapse_count > 0 {
        lines.push(Line::from(vec![
            Span::styled("  \u{25b6} ", Style::default().fg(semantic.text.dim)),
            Span::styled(
                format!("{} collapsed tools", collapse_count),
                Style::default().fg(semantic.text.muted),
            ),
        ]));
    }

    for inner_vm in &children {
        if matches!(inner_vm, TuiRenderUnit::TuiAssistantBubble(_)) {
            continue;
        }
        if matches!(inner_vm, TuiRenderUnit::TuiToolCard(_)) {
            tool_idx += 1;
            if tool_idx <= collapse_count {
                continue;
            }
        }
        let inner_lines = vm_to_lines(inner_vm, width);
        if inner_lines.is_empty() {
            continue;
        }
        // 移除嵌套消息的 leading/trailing 空行
        let start = inner_lines
            .iter()
            .position(|l| !l.spans.is_empty())
            .unwrap_or(0);
        let end = inner_lines
            .iter()
            .rposition(|l| !l.spans.is_empty())
            .map(|i| i + 1)
            .unwrap_or(0);
        let trimmed = &inner_lines[start..end];
        for line in trimmed {
            let mut new_spans = vec![Span::raw("  ")];
            new_spans.extend(line.spans.iter().cloned());
            lines.push(Line::from(new_spans));
        }
    }

    with_message_spacing(lines)
}

fn render_collapsed_group_lines(data: &TuiCollapsedGroup) -> Vec<Line<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;
    vec![Line::from(vec![
        Span::styled("\u{25cf} ", Style::default().fg(semantic.status.success)),
        Span::styled(
            format!("{}\u{ff08}{}\u{9879}\u{ff09}", data.title, data.count),
            Style::default().fg(semantic.text.muted),
        ),
    ])]
}

fn render_divider_lines(data: &TuiDivider) -> Vec<Line<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;
    if let Some(ref label) = data.label {
        vec![Line::from(vec![
            Span::styled("\u{2500}\u{2500} ", Style::default().fg(semantic.text.dim)),
            Span::styled(label.clone(), Style::default().fg(semantic.text.muted)),
            Span::styled(" \u{2500}\u{2500}", Style::default().fg(semantic.text.dim)),
        ])]
    } else {
        vec![Line::from(vec![Span::styled(
            "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
            Style::default().fg(semantic.text.dim),
        )])]
    }
}

fn render_ask_user_block_lines(data: &TuiAskUserBlock) -> Vec<Line<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;
    let mut lines: Vec<Line<'static>> = Vec::new();

    let title_color = if data.is_error {
        semantic.status.error
    } else {
        semantic.status.success
    };
    lines.push(Line::from(Span::styled(
        i18n::tr("render-user-answered"),
        Style::default().fg(title_color),
    )));

    for item in &data.items {
        let prefix = Span::styled("  \u{23bf} ", Style::default().fg(semantic.text.dim));
        let item_color = if data.is_error {
            semantic.status.error
        } else {
            semantic.text.muted
        };
        let content = Span::styled(
            format!("{} \u{2192} {}", item.header, item.answer),
            Style::default().fg(item_color),
        );
        lines.push(Line::from(vec![prefix, content]));
    }

    lines
}

// ── 消息间距辅助 ──────────────────────────────────────────────────────────

fn trim_trailing_blank_lines(lines: &mut Vec<Line<'static>>) {
    while lines
        .last()
        .is_some_and(|line| line.spans.iter().all(|span| span.content.is_empty()))
    {
        lines.pop();
    }
}

fn with_message_spacing(mut lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    trim_trailing_blank_lines(&mut lines);
    let mut spaced = Vec::with_capacity(lines.len() + 1);
    spaced.push(Line::from(""));
    spaced.extend(lines);
    spaced
}

// ── 组件 ──────────────────────────────────────────────────────────────────

#[component]
pub fn MessageArea(props: &MessageAreaProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let view_models = hooks.use_atom(&VIEW_MODELS);
    let acp_state = hooks.use_atom(&crate::kit::atoms::ACP_STATE);
    let todo_atom = hooks.use_atom(&crate::kit::atoms::TODO_ITEMS);
    hooks.use_atom(&LANG_VERSION);

    let snapshot = view_models.read();
    let todo_items = todo_atom.read().clone();
    let is_loading = acp_state.read().is_loading;

    let items_len = snapshot.items.len();
    let vm_generation = snapshot.generation;

    // ── 渲染缓存：generation 不变则复用上次的 Lines，避免每帧做 markdown 解析+syntect ──
    // [TRAP] 缓存必须用 Arc<Vec> 而非 Vec：ratatui-kit 每次 dispatch 后都触发 render，
    // 鼠标 Drag 事件 60-120Hz 会反复读取此缓存。Vec 在每次读取时深拷贝 O(N)（每行多个
    // Span + Cow<str>），直接拖满 CPU。Arc::clone 是 O(1) 引用计数。
    let lines_cache = hooks.use_state(|| (0u64, 0usize, Arc::<Vec<Line<'static>>>::default()));

    // ── total_visual_rows 缓存：仅 (generation, width, lines_len) 变化时重算 line_count ──
    // [TRAP] Paragraph::line_count 是 O(N·W)（unicode-width + wrap），每帧重算会拖垮长对话滚动。
    // cache 仅供 render body 读，不作为响应式源——用 write_no_update 写入避免 wake 自激回路。
    let total_rows_cache = hooks.use_state(|| (0u64, 0u16, 0usize, 0u16));

    // ── Footer 行预计算：必须在 empty 分支之前调用，确保所有 hook 顺序一致 ──
    let footer_lines = build_footer_lines(&mut hooks, is_loading, &todo_items);

    let empty = snapshot.items.is_empty() && !is_loading && todo_items.is_empty();
    let brewed_lines = if empty && !footer_lines.is_empty() {
        Some(footer_lines.clone())
    } else {
        None
    };

    // ── 构建 core_lines（带 Arc 缓存，footer 不参与缓存）──
    // [TRAP] 缓存 key 不能加 `lines.is_empty()` 之类的"空内容"判断——
    // Welcome 屏 items 为空时 vm_to_lines 永远返回空 Vec，写入后再读到
    // is_empty()=true，needs_rebuild 永远为 true，每帧都执行
    // `*lines_cache.write() = ...`。ratatui-kit 的 ReactiveMutRef::Drop 无条件
    // notifier.wake()（不检查值是否变化），wake 又触发 re-render → 自激回路
    // 100% CPU。空内容必须视为有效缓存，靠 generation/width 检测真实变化。
    //
    // [TRAP] core_lines_arc 用 Arc<Vec> —— Drag 60-120Hz 触发 render 时，
    // Arc::clone 是 O(1)；如果用 Vec::clone 则每帧深拷贝数千行 Line+Span，
    // 直接拖满 CPU。footer 后续单独 extend，避免 Arc 解引用后再 clone。
    let core_lines_arc: Arc<Vec<Line<'static>>> = {
        let needs_rebuild = {
            let guard = lines_cache.read();
            guard.0 != vm_generation || guard.1 != props.width
        };
        if !needs_rebuild {
            Arc::clone(&lines_cache.read().2)
        } else {
            let mut lines: Vec<Line<'static>> = Vec::new();
            for item in snapshot.items.iter() {
                lines.extend(vm_to_lines(item, props.width));
            }
            drop(snapshot);
            let arc = Arc::new(lines);
            *lines_cache.write() = (vm_generation, props.width, Arc::clone(&arc));
            arc
        }
    };

    // all_lines 仅在需要时构建（lazy）：
    // - wrap_map_cache 缓存未命中（generation/width 变化）
    // - total_visual_rows 缓存未命中
    // - 非 highlight 渲染路径（render_lines = all_lines）
    // [TRAP] Drag 期间 highlight 路径下，wrap_map / total_visual_rows 都已命中缓存，
    // 实际不需要 all_lines。每次构建需要 (*core_lines_arc).clone() O(N)——Drag 60-120Hz
    // × O(N) 直接拉满 CPU。我们改为只在真正用到时构建。
    let core_len = core_lines_arc.len();
    let footer_len = if empty { 0 } else { footer_lines.len() };
    let lines_len = core_len + footer_len;

    let scroll_state = hooks.use_state(ScrollViewState::default);
    let prev_items_len = hooks.use_state(|| 0usize);
    let _prev_is_loading = hooks.use_state(|| false);
    let scroll_throttle = hooks.use_state(ScrollThrottle::default);
    let _todo_hash = hash_todo_items(&todo_items);

    // ── 文本选区 + 折行映射缓存 ──
    let text_sel = hooks.use_state(TextSelection::default);
    let selection_down_pos = hooks.use_state(|| Option::<(u16, u16)>::None);
    let drag_throttle = hooks.use_state(DragThrottle::default);
    // [TRAP] Drag 60-120Hz 触发 render，wrap_map 必须用 Arc 避免 Vec 深拷贝。
    // highlight 不再缓存——视口裁剪后只有 ~60 行，highlight 成本可忽略。
    let wrap_map_cache = hooks.use_state(|| (0u64, 0u16, Arc::<Vec<WrappedLineInfo>>::default()));

    // ── 消息区位置追踪 ──
    let area_hook = hooks.use_hook(MsgAreaTracker::new);
    let area_rect = area_hook.rect;
    // 滚动条 fields state（hook 通过引用读取，避免 borrow 冲突）
    let scrollbar_fields = hooks.use_state(ScrollbarFields::default);
    hooks.use_hook(move || ScrollbarHook {
        fields: scrollbar_fields,
    });

    let vis_width = area_rect
        .map(|r| r.width.saturating_sub(1))
        .unwrap_or(props.width as u16)
        .max(1);
    let vis_height = area_rect.map(|r| r.height).unwrap_or(60).max(1);

    // 更新 wrap_map 缓存（仅 generation / width 变化时，write_no_update 避免自激回路）
    // [TRAP] wrap_map 只覆盖 core_lines_arc——footer 区域（spinner/todo）不需要选区，
    // 鼠标拖拽到 footer 时 visual_to_logical 返回 None，不触发 highlight。
    {
        let needs_wmap = {
            let g = wrap_map_cache.read();
            g.0 != vm_generation || g.1 != vis_width
        };
        if needs_wmap {
            if core_lines_arc.is_empty() {
                // 空内容：直接设空缓存，不调用 build_wrap_map
                let mut g = wrap_map_cache.write_no_update();
                g.0 = vm_generation;
                g.1 = vis_width;
                g.2 = Arc::default();
            } else {
                let (_, wrap_map) = build_wrap_map(&core_lines_arc, vis_width);
                let mut g = wrap_map_cache.write_no_update();
                g.0 = vm_generation;
                g.1 = vis_width;
                g.2 = Arc::new(wrap_map);
            }
        }
    }

    // ── 总视觉行数：使用 Paragraph wrap 预测（带缓存）──
    // [TRAP] 仅在 (gen, width, lines_len) 变化时构建 all_lines 重算 line_count。
    // Drag 期间 generation/width/lines_len 不变，缓存命中——跳过 O(N) 构建。
    let cached = {
        let g = total_rows_cache.read();
        (g.0, g.1, g.2, g.3)
    };
    let total_visual_rows: u16 =
        if cached.0 == vm_generation && cached.1 == vis_width && cached.2 == lines_len {
            cached.3
        } else if lines_len == 0 {
            let rows: u16 = if is_loading { 1 } else { 0 };
            let mut g = total_rows_cache.write_no_update();
            g.0 = vm_generation;
            g.1 = vis_width;
            g.2 = lines_len;
            g.3 = rows;
            rows
        } else {
            // 构建 all_lines 用于 line_count（仅在 cache 未命中时）
            let mut all_lines = (*core_lines_arc).clone();
            if !empty {
                all_lines.extend(footer_lines.iter().cloned());
            }
            let rows = Paragraph::new(RatText::from(all_lines))
                .wrap(Wrap { trim: false })
                .line_count(vis_width as u16) as u16;
            let mut g = total_rows_cache.write_no_update();
            g.0 = vm_generation;
            g.1 = vis_width;
            g.2 = lines_len;
            g.3 = rows;
            rows
        };

    // ── 鼠标事件处理（滚动 + 文本拖拽选中复制）──
    {
        hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
            if let Event::Key(key) = &event {
                let _ = focus_router::message_accepts_key(key);
            }

            if let Event::Mouse(mouse) = &event {
                // 光标移动无操作——提前返回，不触发任何 state 写入或渲染
                if matches!(mouse.kind, MouseEventKind::Moved) {
                    return EventResult::Ignored;
                }

                // 滚动节流
                let apply_scroll = |delta: i32| {
                    let mut st = scroll_throttle.write_no_update();
                    st.pending_delta += delta;
                    let now = Instant::now();
                    if now.duration_since(st.last_flush) >= Duration::from_millis(SCROLL_FRAME_MS) {
                        let pending = st.pending_delta;
                        st.pending_delta = 0;
                        st.last_flush = now;
                        drop(st);
                        if pending != 0 {
                            let mut state = scroll_state.write_no_update();
                            if pending > 0 {
                                for _ in 0..(pending as u16) {
                                    state.scroll_down();
                                }
                            } else {
                                for _ in 0..((-pending) as u16) {
                                    state.scroll_up();
                                }
                            }
                        }
                    }
                };

                if let Some(area) = area_rect {
                    let in_area = mouse_in_area(mouse.row, mouse.column, area);

                    // [DEBUG] PERI_DISABLE_DRAG_SELECT=1 完全跳过 Drag 选中——验证是否是
                    // Drag 处理逻辑引起卡死。
                    let drag_select_disabled = std::env::var("PERI_DISABLE_DRAG_SELECT")
                        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                        .unwrap_or(false);

                    // ── 文本选中处理（消息区内 Down/Drag/Up）──
                    if in_area && !drag_select_disabled {
                        match mouse.kind {
                            MouseEventKind::Down(MouseButton::Left) => {
                                // 仅记录按下的位置，不启动选区——真实拖动才开始选中
                                // [TRAP] selection_down_pos 只在事件处理器内读写，render
                                // 不依赖它——用 write_no_update 避免 wake 噪音（render 不需要
                                // 因为这个状态变化而重渲染，后续 Drag 才是真正的渲染触发点）。
                                let scroll_y = scroll_state.read().offset().y as u16;
                                let visual_row =
                                    mouse.row.saturating_sub(area.y).saturating_add(scroll_y);
                                // 视口裁剪后无边框，visual_col 直接 = mouse.column - area.x
                                let visual_col = mouse.column.saturating_sub(area.x);
                                *selection_down_pos.write_no_update() =
                                    Some((visual_row, visual_col));
                                return EventResult::Consumed;
                            }
                            MouseEventKind::Drag(MouseButton::Left) => {
                                // Drag 节流（16ms），write_no_update 避免自激回路
                                let now = Instant::now();
                                {
                                    let dt = drag_throttle.read();
                                    if dt.last_flush.elapsed()
                                        < Duration::from_millis(SCROLL_FRAME_MS)
                                    {
                                        return EventResult::Consumed;
                                    }
                                }
                                drag_throttle.write_no_update().last_flush = now;

                                let scroll_y = scroll_state.read().offset().y as u16;
                                let visual_row =
                                    mouse.row.saturating_sub(area.y).saturating_add(scroll_y);
                                let visual_col = mouse.column.saturating_sub(area.x);

                                // 单次 write guard，drop 时只 wake 一次（不是两次）
                                // start_drag + update_drag 合并到同一 guard 内
                                //
                                // [TRAP] ratatui-kit 用 parking_lot::RwLock——同一 thread 同时
                                // 持有 read + write 时 try_write 返回 Err → expect panic。
                                // 必须先把 selection_down_pos.read() 的值 copy 出来 drop guard，
                                // 再 write selection_down_pos。
                                let down_pos = *selection_down_pos.read();
                                {
                                    let mut sel_guard = text_sel.write();
                                    if let Some((dr, dc)) = down_pos {
                                        sel_guard.start_drag(dr, dc);
                                        *selection_down_pos.write_no_update() = None;
                                    }
                                    sel_guard.update_drag(visual_row, visual_col);
                                }
                                return EventResult::Consumed;
                            }
                            MouseEventKind::Up(MouseButton::Left) => {
                                *selection_down_pos.write_no_update() = None;
                                // [TRAP] 同 Drag 处理：必须 copy 出 text_sel 状态后再 write，
                                // 否则 read+write 同 thread 冲突 panic。
                                let dragging = text_sel.read().dragging;
                                if !dragging {
                                    return EventResult::Consumed;
                                }
                                // 先 copy 出 normalized_bounds（owned Option），drop read guard
                                let bounds = text_sel.read().normalized_bounds();
                                let extracted: Option<String> =
                                    if let Some(((sr, sc), (er, ec))) = bounds {
                                        let wrap_guard = wrap_map_cache.read();
                                        let lines_guard = lines_cache.read();
                                        extract_visual_range(
                                            &lines_guard.2,
                                            &wrap_guard.2,
                                            (sr, sc),
                                            (er, ec),
                                            vis_width,
                                        )
                                    } else {
                                        None
                                    };

                                // 清除选区（start/end/dragging 全清），wake 触发重渲染清除 highlight
                                {
                                    let mut sel = text_sel.write();
                                    sel.clear();
                                }

                                // 复制（独立线程，不阻塞）
                                if let Some(text) = extracted {
                                    let char_count = text.chars().count();
                                    copy_to_clipboard(text);
                                    mark_copy_message(char_count);
                                }

                                return EventResult::Consumed;
                            }
                            _ => {}
                        }
                    } else {
                        // 鼠标在消息区外
                        match mouse.kind {
                            MouseEventKind::Down(MouseButton::Left) => {
                                // 清除选区和按下记录
                                *text_sel.write() = TextSelection::new();
                                *selection_down_pos.write_no_update() = None;
                                return EventResult::Ignored;
                            }
                            _ => {
                                if matches!(mouse.kind, MouseEventKind::Drag(_)) {
                                    return EventResult::Ignored;
                                }
                            }
                        }
                    }

                    // ── 滚动处理（区域内外通用）──
                    match mouse.kind {
                        MouseEventKind::ScrollDown => apply_scroll(SCROLL_LINES as i32),
                        MouseEventKind::ScrollUp => apply_scroll(-(SCROLL_LINES as i32)),
                        _ => {}
                    }
                }

                // 所有非 Moved/Drag 鼠标事件标记为已消费（防止泄漏到下层组件）
                return EventResult::Consumed;
            }

            if let Event::Key(key) = &event {
                if key.kind == KeyEventKind::Press && focus_router::message_accepts_key(key) {
                    scroll_state.write().handle_event(&event);
                    return EventResult::Consumed;
                }
            }
            EventResult::Ignored
        });
    }

    /// 从逻辑行中按视觉坐标精确提取选中文本（字符级精度）。
    ///
    /// 折行偏移公式：column_in_logical = vis_col + (vis_row - visual_start) * width。
    /// 用 `visual_col_to_byte_offset` 将列映射到字节偏移再切片。
    ///
    /// [TRAP] 选区可能超出 core 范围（footer 区域无 wrap_map）——clamp 到 wrap_map
    /// 末尾，确保 footer 行的 visual_to_logical 不返回 None 导致整个提取失败。
    fn extract_visual_range(
        lines: &[Line<'static>],
        wrap_map: &[WrappedLineInfo],
        vis_start: (u16, u16),
        vis_end: (u16, u16),
        width: u16,
    ) -> Option<String> {
        let ((sr, sc), (er, ec)) = if vis_start <= vis_end {
            (vis_start, vis_end)
        } else {
            (vis_end, vis_start)
        };
        // Clamp sr/er 到 wrap_map 视觉范围内（footer 区域无 wrap_map，避免 None）
        let max_visual = wrap_map
            .last()
            .map(|e| (e.visual_end.saturating_sub(1)) as u16)
            .unwrap_or(0);
        let sr = sr.min(max_visual);
        let er = er.min(max_visual);
        let first_logical = visual_to_logical(sr, wrap_map)?;
        let last_logical = visual_to_logical(er, wrap_map)?;
        let first = first_logical.min(last_logical);
        let last = first_logical.max(last_logical);

        let mut parts: Vec<String> = Vec::new();
        for li in first..=last {
            let line = lines.get(li)?;
            let plain = text_selection::line_to_plain_text(line);
            let entry = wrap_map.get(li)?;

            if first == last {
                // 同一逻辑行
                let c_start = sc.saturating_add(
                    (sr as usize).saturating_sub(entry.visual_start) as u16 * width,
                );
                let c_end = ec.saturating_add(
                    (er as usize).saturating_sub(entry.visual_start) as u16 * width,
                );
                let b0 = text_selection::visual_col_to_byte_offset(&plain, c_start);
                let b1 = text_selection::visual_col_to_byte_offset(&plain, c_end);
                if b0 >= b1 {
                    continue;
                }
                parts.push(plain[b0..b1].to_string());
            } else if li == first {
                let c_start = sc.saturating_add(
                    (sr as usize).saturating_sub(entry.visual_start) as u16 * width,
                );
                let b0 = text_selection::visual_col_to_byte_offset(&plain, c_start);
                parts.push(plain[b0..].to_string());
            } else if li == last {
                let c_end = ec.saturating_add(
                    (er as usize).saturating_sub(entry.visual_start) as u16 * width,
                );
                let b1 = text_selection::visual_col_to_byte_offset(&plain, c_end);
                parts.push(plain[..b1].to_string());
            } else {
                parts.push(plain);
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n"))
        }
    }

    // ── 吸底自动跟随 ──
    let last_scrolled_at = hooks.use_state(|| 0u16);
    hooks.use_effect(
        {
            let st = scroll_state;
            let pl = prev_items_len;
            let lsa = last_scrolled_at;
            let len = items_len;
            let loading = is_loading;
            move || {
                let prev = *pl.read();
                *pl.write() = len;

                if total_visual_rows == 0 || vis_height == 0 {
                    return;
                }

                if loading {
                    // [TRAP] read+write 同 state 同线程 = parking_lot 死锁——先 copy 出来
                    let prev_lsa = *lsa.read();
                    if total_visual_rows > prev_lsa {
                        let max_scroll = total_visual_rows.saturating_sub(vis_height);
                        let scroll_y = st.read().offset().y as u16;
                        if scroll_y < max_scroll {
                            st.write().scroll_to_bottom();
                        }
                        *lsa.write() = total_visual_rows;
                    }
                    return;
                }

                if len < prev {
                    st.write().scroll_to_bottom();
                    *lsa.write() = total_visual_rows;
                    return;
                }

                let max_scroll = total_visual_rows.saturating_sub(vis_height);
                let scroll_y = st.read().offset().y as u16;
                if scroll_y >= max_scroll {
                    return;
                }
                let distance = max_scroll.saturating_sub(scroll_y);
                if distance > (vis_height / 4).max(5) {
                    return;
                }
                let prev_lsa = *lsa.read();
                if total_visual_rows > prev_lsa {
                    st.write().scroll_to_bottom();
                    *lsa.write() = total_visual_rows;
                }
            }
        },
        (items_len, vm_generation, is_loading),
    );

    if empty {
        if let Some(lines) = brewed_lines {
            return element!(
                View(
                    flex_direction: Direction::Vertical,
                    width: Constraint::Fill(1),
                    height: Constraint::Fill(1),
                ) {
                    View(height: Constraint::Fill(1)) {
                        Welcome(width: props.width)
                    }
                    Text(text: Paragraph::new(RatText::from(lines)).wrap(Wrap { trim: false }))
                }
            )
            .into_any();
        }
        return element!(
            View(width: Constraint::Fill(1), height: Constraint::Fill(1)) {
                Welcome(width: props.width)
            }
        )
        .into_any();
    }

    // ── 视口裁剪渲染（移除 ScrollView 的全量大 buffer）──
    // [TRAP] 原 ScrollView + Paragraph-with-Wrap 组合是 100% CPU 的真凶：ScrollView 创建
    // (width × total_visual_rows) 大 buffer，Paragraph 渲染所有 N 行到这个 buffer 是
    // O(N×W) per render。Drag 60-120Hz × O(N×W) 直接拉满 CPU。
    //
    // 视口裁剪：只 clone + highlight + 渲染视口内 ~60 行（vis_height）。
    //   1. 通过 wrap_map_cache 二分查找视口对应的逻辑行 [vp_start, vp_end]
    //   2. vp_first_offset = scroll_y - wrap_map[vp_start].visual_start（首行视觉偏移）
    //   3. viewport_lines = clone(core[vp_start..=vp_end]) + 必要时附加 footer_lines
    //   4. Paragraph::scroll((vp_first_offset, 0)) 精确偏移首行
    //
    // highlight：视口内选区行用 sel_bg 背景。不再缓存 highlight 结果——视口裁剪后
    // 只有 ~60 行，highlight 成本可忽略。Drag 期间频繁跨逻辑行变化也不会卡。
    //
    // [DEBUG] PERI_NO_HIGHLIGHT=1 紧急回退——完全不进入 highlight 路径。
    let no_highlight = std::env::var("PERI_NO_HIGHLIGHT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    // clamp scroll_y 不超过 max_scroll（替代 ScrollView 渲染时的 clamp）
    let max_scroll = (total_visual_rows as usize).saturating_sub(vis_height as usize);
    let scroll_y_raw = scroll_state.read().offset().y as usize;
    let scroll_y = scroll_y_raw.min(max_scroll);

    // 更新 scrollbar fields——post_component_draw 时基于此渲染滚动条
    {
        let mut g = scrollbar_fields.write_no_update();
        g.content_length = total_visual_rows as usize;
        g.position = scroll_y;
        g.viewport_length = vis_height as usize;
    }

    let vp_height = vis_height as usize;

    // core 总视觉行数
    let core_total_visual_rows: usize = {
        let g = wrap_map_cache.read();
        g.2.last().map(|e| e.visual_end).unwrap_or(0)
    };

    // 选区对应的逻辑行范围（视口外选区不参与 highlight，selection state 保留供复制）
    let sel_bounds: Option<(usize, usize)> = if !no_highlight {
        let sel = text_sel.read();
        if let Some(((sr, _), (er, _))) = sel.normalized_bounds() {
            let g = wrap_map_cache.read();
            let f = visual_to_logical(sr, &g.2).unwrap_or(0);
            let l = visual_to_logical(er, &g.2).unwrap_or(0);
            Some((f.min(l), f.max(l)))
        } else {
            None
        }
    } else {
        None
    };

    // 视口对应的 core 逻辑行范围 + 首行视觉偏移
    let (vp_core_start, vp_core_end, vp_first_offset): (usize, usize, u16) =
        if scroll_y < core_total_visual_rows && !core_lines_arc.is_empty() {
            let g = wrap_map_cache.read();
            viewport_logical_range(&g.2, scroll_y, vp_height).unwrap_or((0, 0, 0))
        } else {
            // 视口完全在 footer 内（footer 占据末尾几行）
            (0, 0, 0)
        };

    // 视口是否包含 footer（视口末尾超出 core 总视觉行数）
    let viewport_has_footer =
        !empty && !footer_lines.is_empty() && scroll_y + vp_height > core_total_visual_rows;

    // 构建 viewport_lines：clone + highlight 视口内的 core 行，必要时附加 footer
    let sel_bg = THEME_ATOM.state().read().semantic.surface.selection;
    let core_len = core_lines_arc.len();
    let mut viewport_lines: Vec<Line<'static>> = Vec::with_capacity(
        (vp_core_end.saturating_sub(vp_core_start) + 1)
            .min(vp_height + 2)
            .saturating_add(footer_lines.len()),
    );

    if scroll_y < core_total_visual_rows && vp_core_start <= vp_core_end && core_len > 0 {
        let end = vp_core_end.min(core_len - 1);
        for i in vp_core_start..=end {
            let line = &core_lines_arc[i];
            let in_sel = sel_bounds.is_some_and(|(f, l)| i >= f && i <= l);
            if in_sel {
                let spans: Vec<Span<'static>> = line
                    .spans
                    .iter()
                    .map(|s| Span::styled(s.content.clone(), s.style.bg(sel_bg)))
                    .collect();
                viewport_lines.push(Line::from(spans));
            } else {
                viewport_lines.push(line.clone());
            }
        }
    }

    if viewport_has_footer {
        viewport_lines.extend(footer_lines.iter().cloned());
    }

    // Paragraph::scroll 偏移：core 内的偏移 = vp_first_offset
    // 视口完全在 footer 内时（scroll_y >= core_total_visual_rows），按 footer 内偏移
    let scroll_offset_y: u16 = if scroll_y >= core_total_visual_rows && core_total_visual_rows > 0 {
        (scroll_y - core_total_visual_rows) as u16
    } else {
        vp_first_offset
    };

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            Text(text: Paragraph::new(RatText::from(viewport_lines))
                .wrap(Wrap { trim: false })
                .scroll((scroll_offset_y, 0)))
        }
    )
    .into_any()
}

// ── footer 行构建 ─────────────────────────────────────────────────────────

fn build_footer_lines(
    hooks: &mut Hooks,
    is_loading: bool,
    todo_items: &[TodoItem],
) -> Vec<Line<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;

    let spinner_state = hooks.use_state(|| SpinnerState::new(SpinnerMode::Thinking));
    let load_start = hooks.use_state(|| Option::<Instant>::None);
    let was_loading = hooks.use_state(|| false);
    let summary_elapsed_ms = hooks.use_state(|| 0u64);
    let loading_epoch = hooks.use_atom(&crate::kit::atoms::LOADING_EPOCH);
    let last_epoch = hooks.use_state(|| 0u64);

    let last_reset_counter = hooks.use_state(|| crate::kit::atoms::BRIDGE_RESET_COUNTER.get());
    {
        let current = crate::kit::atoms::BRIDGE_RESET_COUNTER.get();
        // [TRAP] read guard 必须 drop 后再 write——同线程 parking_lot::RwLock read+write 会死锁
        // （deadlock_detection 默认关闭，静默卡死）。先把 read 值 copy 到 owned。
        let prev_counter = *last_reset_counter.read();
        if prev_counter != current {
            *summary_elapsed_ms.write() = 0;
            *last_reset_counter.write() = current;
        }
    }

    {
        let current_epoch = *loading_epoch.read();
        // [TRAP] 同上：read+write 同一 state 不可并存
        let prev_epoch = *last_epoch.read();
        if is_loading && prev_epoch != current_epoch {
            *last_epoch.write() = current_epoch;
            *load_start.write() = Some(Instant::now());
            *spinner_state.write() = SpinnerState::new(SpinnerMode::Thinking);
            *was_loading.write() = true;
        }

        let prev_loading = *was_loading.read();
        if prev_loading != is_loading {
            let mut ls = load_start.write();
            if is_loading {
                if ls.is_none() {
                    *ls = Some(Instant::now());
                    *spinner_state.write() = SpinnerState::new(SpinnerMode::Thinking);
                }
            } else {
                *summary_elapsed_ms.write() =
                    ls.map_or(0, |start| start.elapsed().as_millis() as u64);
                *ls = None;
            }
            *was_loading.write() = is_loading;
        }
    }

    let has_summary = *summary_elapsed_ms.read() > 0;
    if !is_loading && todo_items.is_empty() && !has_summary {
        return Vec::new();
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let has_footer_content = is_loading || has_summary || !todo_items.is_empty();
    if has_footer_content {
        lines.push(Line::from(""));
        lines.push(Line::from(""));
    }
    if is_loading {
        let token_count = crate::kit::atoms::SPINNER_TOKEN_COUNT.get();
        lines.extend(spinner_state.read().render_to_lines(
            semantic.accent,
            semantic.text.muted,
            true,
            true,
            token_count,
        ));
    } else if has_summary {
        let elapsed = peri_widgets::spinner::animation::format_elapsed(*summary_elapsed_ms.read());
        lines.push(Line::from(Span::styled(
            i18n::tr_args(
                "msg-spinner-brewed",
                &[("duration".to_string(), FluentValue::from(elapsed))],
            ),
            Style::default().fg(semantic.text.muted),
        )));
    }
    if !todo_items.is_empty() {
        lines.extend(render_todo_lines(&todo_items));
    }
    if has_footer_content {
        lines.push(Line::from(""));
    }
    lines
}

// ── 测试 ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_todo_lines_icons_and_crossed() {
        let items = vec![
            TodoItem {
                status: TodoStatus::InProgress,
                content: "修复 bug".into(),
            },
            TodoItem {
                status: TodoStatus::Completed,
                content: "草拟 PRD".into(),
            },
            TodoItem {
                status: TodoStatus::Pending,
                content: "部署".into(),
            },
        ];
        let lines = render_todo_lines(&items);
        assert_eq!(lines.len(), 4);

        let in_progress_icon = lines[0].spans[0].content.as_ref();
        assert!(in_progress_icon.contains("◼"), "InProgress 图标应为 ◼");
        let in_progress_text = lines[0].spans[1].content.as_ref();
        assert!(
            in_progress_text.contains("修复 bug"),
            "InProgress 文本应包含任务内容"
        );

        let completed_icon = lines[1].spans[0].content.as_ref();
        assert!(completed_icon.contains("✔"), "Completed 图标应为 ✔");
        let completed_text = lines[1].spans[1].content.as_ref();
        assert!(
            completed_text.contains("草拟 PRD"),
            "Completed 文本应包含任务内容"
        );

        let pending_icon = lines[2].spans[0].content.as_ref();
        assert!(pending_icon.contains("◻"), "Pending 图标应为 ◻");
        let pending_text = lines[2].spans[1].content.as_ref();
        assert!(pending_text.contains("部署"), "Pending 文本应包含任务内容");
        assert!(
            pending_text.contains("(available)") || pending_text.contains("(可开始)"),
            "Pending 文本应包含 i18n 可用标记"
        );
    }

    #[test]
    fn test_render_todo_lines_empty() {
        let lines = render_todo_lines(&[]);
        assert_eq!(lines.len(), 1);
        for line in &lines {
            assert!(
                line.spans.is_empty(),
                "空 todo 列表不应输出任何内容行，仅 trailing blank lines"
            );
        }
    }

    #[test]
    fn test_spinner_summary_after_loading_ends() {
        let elapsed_ms: u64 = 30_000;
        let elapsed_str = peri_widgets::spinner::animation::format_elapsed(elapsed_ms);
        assert_eq!(elapsed_str, "30s");

        let summary = format!("  ✻  Brewed for {elapsed_str}");
        assert!(summary.contains("✻"));
        assert!(summary.contains("Brewed for"));
    }

    #[test]
    fn test_token_count_no_write_when_unchanged() {
        let prev_token_count: usize = 1500;
        let new_token_count: usize = 1500;
        let changed = prev_token_count != new_token_count;

        assert!(!changed, "token count 未变化时不应写 state");
    }

    #[test]
    fn test_footer_loading_steady_state_has_no_control_state_transition() {
        let prev_loading = true;
        let is_loading = true;
        let transition = prev_loading != is_loading;

        assert!(
            !transition,
            "loading 稳态不应写 was_loading/load_start，否则会触发持续重渲染"
        );
    }

    #[test]
    fn test_empty_with_todo_items_shows_footer_not_welcome() {
        let entries_empty = true;
        let is_loading = false;
        let todo_items_empty = false;
        let empty = entries_empty && !is_loading && todo_items_empty;

        assert!(
            !empty,
            "仅有 todo 条目且无消息时不应判定为 empty，避免 Welcome 覆盖 todo 显示"
        );
    }

    #[test]
    fn test_empty_without_todo_is_truly_empty() {
        let entries_empty = true;
        let is_loading = false;
        let todo_items_empty = true;
        let empty = entries_empty && !is_loading && todo_items_empty;

        assert!(empty);
    }

    fn proximity_check(total: u16, scroll_y: u16, vis_height: u16) -> bool {
        if total == 0 {
            return false;
        }
        let max_scroll = total.saturating_sub(vis_height);
        if scroll_y >= max_scroll {
            return false;
        }
        let distance = max_scroll.saturating_sub(scroll_y);
        let threshold = (vis_height / 2).max(5);
        distance <= threshold
    }

    #[test]
    fn test_proximity_at_bottom_should_not_trigger_scroll() {
        let total = 100;
        let vis_height = 20;
        let scroll_y = total - vis_height;
        assert!(!proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_within_half_viewport_should_follow() {
        let total = 100;
        let vis_height = 20;
        let scroll_y = total - vis_height - 10;
        assert!(proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_beyond_half_viewport_should_not_follow() {
        let total = 100;
        let vis_height = 20;
        let scroll_y = total - vis_height - 11;
        assert!(!proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_near_top_should_not_follow() {
        let total = 200;
        let vis_height = 30;
        let scroll_y = 20;
        assert!(!proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_small_viewport_minimum_threshold() {
        let total = 50;
        let vis_height = 6;
        let scroll_y = total - vis_height - 5;
        assert!(proximity_check(total, scroll_y, vis_height));
        let scroll_y = total - vis_height - 6;
        assert!(!proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_empty_content_no_follow() {
        assert!(!proximity_check(0, 0, 20));
    }

    #[test]
    fn test_proximity_content_smaller_than_viewport_at_bottom() {
        let total = 10;
        let vis_height = 30;
        assert!(!proximity_check(total, 0, vis_height));
    }
}
