//! vm_to_lines + 各变体渲染函数 + 辅助函数。

use crate::i18n;
use crate::kit::tui_render_unit::{
    TuiAskUserBlock, TuiCollapsedGroup, TuiDivider, TuiHunkLineKind, TuiNoteLevel, TuiRenderUnit,
    TuiSubAgentGroup, TuiSystemNote, TuiToolCard,
};
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::style::{Modifier, Style};
use ratatui_kit::ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

// ── 工具卡片辅助函数（内联自 view_render/tool_card.rs）─────────────────

const COLLAPSED_BY_DEFAULT: &[&str] = &[
    "Bash",
    "Read",
    "Edit",
    "Write",
    "Glob",
    "Grep",
    "AskUserQuestion",
];
const AUTO_EXPAND: &[&str] = &["AgentResult", "ExecuteExtraTool", "SearchExtraTools"];
const FORCE_EXPAND_ON_COMPLETE: &[&str] = &[];

pub(super) fn compact_summary(text: &str, max_chars: usize) -> String {
    let joined = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" \u{b7} ");
    truncate_str(&joined, max_chars)
}

pub(super) fn compact_output_lines(text: &str, max_lines: usize, max_chars: usize) -> Vec<String> {
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

pub(super) fn format_running_duration(ms: u64) -> String {
    let secs = ms / 1000;
    let mins = secs / 60;
    let secs = secs % 60;
    if mins > 0 {
        format!("{}min {}s", mins, secs)
    } else {
        format!("{}s", secs)
    }
}

pub(super) fn diff_change_summary(
    diff: &crate::kit::tui_render_unit::TuiDiffBlock,
) -> Option<String> {
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

pub(super) fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}\u{2026}", truncated)
    }
}

// ── vm_to_lines：TuiRenderUnit → Vec<Line<'static>> ───────────────────────

/// 将单个 TuiRenderUnit 变体转换为渲染行。
/// 使用 kit::markdown 进行 markdown 解析（无缓存，每次全量）。
///
/// 用于不需要增量缓存的场景（如 SubAgentGroup 内部递归）。
/// 顶层 AssistantBubble / UserBubble 渲染应使用 [`vm_to_lines_cached`]。
pub(super) fn vm_to_lines(vm: &TuiRenderUnit, width: usize) -> Vec<Line<'static>> {
    vm_to_lines_cached(
        vm,
        width,
        &mut crate::kit::markdown::MarkdownRenderCache::default(),
    )
}

/// 与 [`vm_to_lines`] 同逻辑，但接受 markdown 渲染缓存以支持增量续跑。
///
/// [Phase 2] 流式期间 text 末尾追加 token 时，前缀 blocks（已闭合的 paragraph /
/// list item / code block）完全不变——cache 通过文本前缀比较复用上次处理到
/// stable_state 的累积状态，仅处理新增 block。
pub(super) fn vm_to_lines_cached(
    vm: &TuiRenderUnit,
    width: usize,
    md_cache: &mut crate::kit::markdown::MarkdownRenderCache,
) -> Vec<Line<'static>> {
    match vm {
        TuiRenderUnit::TuiAssistantBubble(data) => {
            let mut lines: Vec<Line<'static>> = Vec::new();

            // 推理块
            if let Some(ref reasoning) = data.reasoning {
                lines.extend(render_reasoning_block(reasoning, width));
            }

            // Markdown 文本
            if !data.text.is_empty() {
                let theme_guard = peri_theme::atoms::THEME_ATOM.state();
                let theme = theme_guard.read();
                let md_text_fg = theme.component.markdown.text;
                let palette_state = peri_theme::atoms::PALETTE_ATOM.state();
                let palette_guard = palette_state.read();
                let segments = crate::kit::markdown::parse_markdown_cached(
                    &data.text,
                    width,
                    *palette_guard,
                    md_text_fg,
                    md_cache,
                );
                for (seg_idx, seg) in segments.into_iter().enumerate() {
                    // segment 之间加空行（表格 ↔ 文本边界）
                    if seg_idx > 0 && !lines.last().is_some_and(|l| l.spans.is_empty()) {
                        lines.push(Line::default());
                    }
                    match seg {
                        crate::kit::markdown::MarkdownSegment::Text(seg_lines) => {
                            lines.extend(seg_lines);
                        }
                        crate::kit::markdown::MarkdownSegment::Table(data) => {
                            let mut table_theme =
                                ratatui_kit::components::TableTheme::from_palette(&palette_guard);
                            // 行文字使用终端默认色，而非纯白
                            table_theme.row_style = Style::default();
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
            let md_text_fg = component.markdown.text;
            let palette_state = peri_theme::atoms::PALETTE_ATOM.state();
            let palette_guard = palette_state.read();
            // 预留 2 列给 ❯ 前缀 / 续行缩进，确保折行后的文本 + 前缀总宽 ≤ vis_width，
            // 避免 build_wrap_map 的 Paragraph::line_count(vis_width) 把含前缀行多计一行。
            let user_text_width = width.saturating_sub(2).max(1);
            let segments = crate::kit::markdown::parse_markdown_cached(
                &data.text,
                user_text_width,
                *palette_guard,
                md_text_fg,
                md_cache,
            );

            let mut lines: Vec<Line<'static>> = Vec::new();
            lines.push(Line::from(""));

            for (seg_idx, seg) in segments.into_iter().enumerate() {
                // segment 之间加空行（带 bg）
                if seg_idx > 0 {
                    lines.push(Line::from(vec![Span::styled(
                        "  ",
                        Style::default().bg(user_bg),
                    )]));
                }
                match seg {
                    crate::kit::markdown::MarkdownSegment::Text(mut seg_lines) => {
                        for (i, line) in seg_lines.drain(..).enumerate() {
                            if i == 0 && seg_idx == 0 {
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
                        let mut table_theme =
                            ratatui_kit::components::TableTheme::from_palette(&palette_guard);
                        // 行文字使用终端默认色，而非纯白
                        table_theme.row_style = Style::default();
                        // 同样预留 2 列给续行缩进，与文本段对称
                        let table_lines = crate::kit::markdown::table_data_to_lines(
                            &data,
                            &table_theme,
                            user_text_width,
                        );
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
    width: usize,
) -> Vec<Line<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;
    let char_count = reasoning.text.chars().count();
    let mut lines = vec![Line::from("")];
    lines.push(Line::from(vec![Span::styled(
        i18n::tr_args(
            "render-thought-for",
            &[("count".to_string(), FluentValue::from(char_count as u64))],
        ),
        Style::default().fg(semantic.text.dim).italic(),
    )]));

    if !reasoning.collapsed {
        let style = Style::default().fg(semantic.text.dim).italic();
        let prefix = " \u{23bf} ";
        let max_text_width = width.saturating_sub(3); // prefix " ⏿ " 占 3 列
        // “…” 占 1 列（U+2026），当线条被截断时需要保留
        let ellipsis_width = 1usize;
        let tail_lines: Vec<&str> = reasoning.text.lines().rev().take(3).collect();

        for tail in tail_lines.into_iter().rev() {
            if tail.is_empty() {
                continue;
            }
            // [Fix] thinking 预览行不折行、不 pre-split——直接按 visual width 截断。
            // 每条 thinking 行强制占 1 个 visual row，流式期间 block 高度完全稳定，
            // 不会把下方响应文本"推出视野又拉回"。
            let truncated = if tail.width() <= max_text_width {
                tail.to_string()
            } else {
                truncate_to_width(tail, max_text_width.saturating_sub(ellipsis_width)) + "\u{2026}"
            };
            lines.push(Line::from(vec![
                Span::styled(prefix.to_string(), style),
                Span::styled(truncated, style),
            ]));
        }
    }
    lines.push(Line::from(""));

    lines
}

/// 按 visual width 截断文本，返回 ≤max_width 列的 prefix-free 字符串。
/// CJK 安全：使用 UnicodeWidthStr::width() 而非字节/字符计数。
fn truncate_to_width(text: &str, max_width: usize) -> String {
    let mut w = 0usize;
    for (i, c) in text.char_indices() {
        let cw = c.width().unwrap_or(1);
        if w + cw > max_width {
            return text[..i].to_string();
        }
        w += cw;
    }
    text.to_string()
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
    let display_name = crate::kit::tool_display::format_tool_name(&data.tool_name);

    // 指示器 + 颜色
    let (indicator, indicator_color) = if data.is_error {
        ("\u{25cf}", semantic.status.error)
    } else if data.is_running {
        ("\u{25cf}", semantic.status.running)
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

    // Read/Edit/Write/Glob/Grep 完成后在头行显示摘要后缀，不另起输出行
    let mut has_header_suffix = false;
    if !data.is_running && !data.is_error && !data.output_summary.is_empty() {
        let suffix = match data.tool_name.as_str() {
            "Read" => {
                let total_lines = data
                    .output_summary
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .count();
                format!(" \u{2014} {} lines", total_lines)
            }
            "Glob" | "Grep" => {
                let total = data
                    .output_summary
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .count();
                format!(" \u{2014} {} matches", total)
            }
            "Edit" | "Write" => {
                let trimmed = data.output_summary.trim();
                let lines = trimmed.lines().count();
                let base = if lines <= 3 {
                    truncate_str(trimmed, 200)
                } else {
                    format!("{} lines changed", lines)
                };
                let diff_suffix = data
                    .diff
                    .as_ref()
                    .and_then(diff_change_summary)
                    .map(|s| format!(" · {}", s))
                    .unwrap_or_default();
                format!(" \u{2014} {}{}", base, diff_suffix)
            }
            _ => String::new(),
        };
        if !suffix.is_empty() {
            header_spans.push(Span::styled(suffix, Style::default().fg(semantic.text.dim)));
            has_header_suffix = true;
        }
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
    } else if data.is_running {
        false
    } else if AUTO_EXPAND.contains(&data.tool_name.as_str()) {
        false
    } else if FORCE_EXPAND_ON_COMPLETE.contains(&data.tool_name.as_str()) {
        data.is_running
    } else {
        COLLAPSED_BY_DEFAULT.contains(&data.tool_name.as_str())
    };

    if collapsed {
        // 头行已显示摘要后缀的工具无需额外输出行
        if !has_header_suffix && !data.output_summary.is_empty() {
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
    let content_color = match data.level {
        TuiNoteLevel::Info => semantic.text.muted,
        TuiNoteLevel::Warning => semantic.status.warning,
        TuiNoteLevel::Error => semantic.status.error,
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    for line_text in data.text.lines() {
        let prefix_str = if line_text.starts_with('\u{273b}') {
            "\u{273b} "
        } else if line_text.starts_with("\u{23bf}") {
            "\u{23bf} "
        } else if line_text.starts_with("  \u{23bf}") {
            "  \u{23bf} "
        } else {
            ""
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
                Style::default().fg(content_color),
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

pub(super) fn trim_trailing_blank_lines(lines: &mut Vec<Line<'static>>) {
    while lines
        .last()
        .is_some_and(|line| line.spans.iter().all(|span| span.content.is_empty()))
    {
        lines.pop();
    }
}

pub(super) fn with_message_spacing(mut lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    trim_trailing_blank_lines(&mut lines);
    let mut spaced = Vec::with_capacity(lines.len() + 1);
    spaced.push(Line::from(""));
    spaced.extend(lines);
    spaced
}

#[cfg(test)]
#[path = "render_test.rs"]
mod tests;
