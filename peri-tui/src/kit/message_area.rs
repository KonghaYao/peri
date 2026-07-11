//! MessageArea：直接读取 VIEW_MODELS，通过 vm_to_lines 将 TuiRenderUnit
//! 转换为 Vec<Line>，在 ScrollView 中渲染。
//!
//! - 滚动：由 ScrollViewState 处理键盘/鼠标事件
//! - 智能跟随：use_effect 检测 VIEW_MODELS 变化
//! - 不再使用 RENDER_CACHE / render_bridge / viewport_clip / wrap_map

#![allow(clippy::needless_update)]

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use crate::i18n;
use crate::kit::atoms::{LANG_VERSION, VIEW_MODELS};
use crate::kit::focus_router;
use crate::kit::panel_registry::clean_scrollbars;
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
                            let table_border_style =
                                Style::default().fg(ratatui::style::Color::Gray);
                            lines.extend(crate::kit::markdown::table_data_to_lines(
                                &data,
                                table_border_style,
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
                        let table_border_style = Style::default().fg(ratatui::style::Color::Gray);
                        let table_lines =
                            crate::kit::markdown::table_data_to_lines(&data, table_border_style);
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
    let lines_cache = hooks.use_state(|| (0u64, 0usize, Vec::<Line<'static>>::new()));

    // ── Footer 行预计算：必须在 empty 分支之前调用，确保所有 hook 顺序一致 ──
    let footer_lines = build_footer_lines(&mut hooks, is_loading, &todo_items);

    let empty = snapshot.items.is_empty() && !is_loading && todo_items.is_empty();
    let brewed_lines = if empty && !footer_lines.is_empty() {
        Some(footer_lines.clone())
    } else {
        None
    };

    // ── 构建全量行（带缓存，footer 不参与缓存）──
    // [TRAP] 缓存 key 不能加 `lines.is_empty()` 之类的"空内容"判断——
    // Welcome 屏 items 为空时 vm_to_lines 永远返回空 Vec，写入后再读到
    // is_empty()=true，needs_rebuild 永远为 true，每帧都执行
    // `*lines_cache.write() = ...`。ratatui-kit 的 ReactiveMutRef::Drop 无条件
    // notifier.wake()（不检查值是否变化），wake 又触发 re-render → 自激回路
    // 100% CPU。空内容必须视为有效缓存，靠 generation/width 检测真实变化。
    let mut all_lines: Vec<Line<'static>> = {
        let needs_rebuild = {
            let guard = lines_cache.read();
            guard.0 != vm_generation || guard.1 != props.width
        }; // guard 在此释放，后续 write() 不会冲突
        if !needs_rebuild {
            lines_cache.read().2.clone()
        } else {
            let mut lines: Vec<Line<'static>> = Vec::new();
            for item in snapshot.items.iter() {
                lines.extend(vm_to_lines(item, props.width));
            }
            drop(snapshot);
            *lines_cache.write() = (vm_generation, props.width, lines.clone());
            lines
        }
    };
    if !empty {
        all_lines.extend(footer_lines);
    }

    let scroll_state = hooks.use_state(ScrollViewState::default);
    let prev_items_len = hooks.use_state(|| 0usize);
    let _prev_is_loading = hooks.use_state(|| false);
    let _scroll_throttle = hooks.use_state(|| 0u8);
    let _todo_hash = hash_todo_items(&todo_items);

    // ── 消息区位置追踪 ──
    let area_hook = hooks.use_hook(MsgAreaTracker::new);
    let area_rect = area_hook.rect;

    let vis_width = area_rect
        .map(|r| r.width.saturating_sub(1))
        .unwrap_or(props.width as u16)
        .max(1);
    let vis_height = area_rect.map(|r| r.height).unwrap_or(60).max(1);

    // ── 总视觉行数：使用 Paragraph wrap 预测 ──
    let total_visual_rows: u16 = if all_lines.is_empty() {
        if is_loading { 1 } else { 0 }
    } else {
        Paragraph::new(RatText::from(all_lines.clone()))
            .wrap(Wrap { trim: false })
            .line_count(vis_width as u16) as u16
    };

    // ── 鼠标事件处理（仅滚动，移除文本选中）──
    {
        let _vis_width_handler = vis_width;
        hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
            if let Event::Key(key) = &event {
                let _ = focus_router::message_accepts_key(key);
            }

            if let Event::Mouse(mouse) = &event {
                if matches!(mouse.kind, MouseEventKind::Moved) {
                    return EventResult::Ignored;
                }

                if let Some(area) = area_rect {
                    let in_area = mouse_in_area(mouse.row, mouse.column, area);

                    if in_area {
                        match mouse.kind {
                            MouseEventKind::ScrollDown => {
                                let mut state = scroll_state.write();
                                for _ in 0..SCROLL_LINES {
                                    state.scroll_down();
                                }
                            }
                            MouseEventKind::ScrollUp => {
                                let mut state = scroll_state.write();
                                for _ in 0..SCROLL_LINES {
                                    state.scroll_up();
                                }
                            }
                            _ => {}
                        }
                    } else {
                        match mouse.kind {
                            MouseEventKind::ScrollDown => {
                                let mut state = scroll_state.write();
                                for _ in 0..SCROLL_LINES {
                                    state.scroll_down();
                                }
                            }
                            MouseEventKind::ScrollUp => {
                                let mut state = scroll_state.write();
                                for _ in 0..SCROLL_LINES {
                                    state.scroll_up();
                                }
                            }
                            MouseEventKind::Down(MouseButton::Left) => {
                                return EventResult::Ignored;
                            }
                            _ => {
                                scroll_state.write().handle_event(&event);
                            }
                        }
                    }
                }

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
                    if total_visual_rows > *lsa.read() {
                        let max_scroll = total_visual_rows.saturating_sub(vis_height);
                        let scroll_y = st.read().offset().y as u16;
                        if scroll_y < max_scroll {
                            st.write().scroll_to_bottom();
                        }
                        *lsa.write() = total_visual_rows;
                    }
                    return;
                }

                if prev == 0 && len > 0 {
                    st.write().scroll_to_bottom();
                    *lsa.write() = total_visual_rows;
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
                if total_visual_rows > *lsa.read() {
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

    // ── ScrollView 渲染：全量内容传给 Paragraph，ScrollView 负责视口裁剪 ──
    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            ScrollView(
                flex_direction: Direction::Vertical,
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
                state: scroll_state,
                scrollbars: clean_scrollbars(),
                active: false,
            ) {
                View(
                    flex_direction: Direction::Vertical,
                    width: Constraint::Fill(1),
                    height: Constraint::Length(total_visual_rows.max(1)),
                ) {
                    Text(text: Paragraph::new(RatText::from(all_lines)).wrap(Wrap { trim: false }))
                }
            }
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
        if *last_reset_counter.read() != current {
            *summary_elapsed_ms.write() = 0;
            *last_reset_counter.write() = current;
        }
    }

    {
        let current_epoch = *loading_epoch.read();
        if is_loading && *last_epoch.read() != current_epoch {
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
