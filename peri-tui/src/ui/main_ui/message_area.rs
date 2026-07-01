use peri_acp::event::TodoStatusDto;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;

use super::sticky_header;
use crate::{
    app::{
        App,
        message_state::{MessageRenderCache, WrappedLineInfo},
    },
    ui::{theme, welcome},
};

/// 视口裁剪结果
struct ViewportClip {
    /// 裁剪后的可见行（含 spinner 和选区高亮）
    lines: Vec<Line<'static>>,
    /// 裁剪后的局部滚动偏移（相对于 lines[0] 的视觉行偏移）
    local_offset: u16,
}

/// V2 render cache builder: converts V2 ViewModels to Lines and builds the wrap map.
fn build_sync_render_cache_v2(
    view_models: &[peri_acp_types::view_model::ViewModel],
    diff_visible: bool,
    width: u16,
) -> MessageRenderCache {
    if width == 0 || view_models.is_empty() {
        return MessageRenderCache {
            lines: Vec::new(),
            wrap_map: Vec::new(),
            total_lines: 0,
            version: 0,
            width,
        };
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    for vm in view_models {
        let vm_lines = crate::kit::view_render::render_v2_vm(vm, width as usize, diff_visible);
        lines.extend(vm_lines);
        lines.push(Line::from(""));
    }

    let lines = dedup_blank_lines(lines);
    let (total_lines, wrap_map) = build_wrap_map(&lines, width);

    MessageRenderCache {
        lines,
        wrap_map,
        total_lines,
        version: 1,
        width,
    }
}

/// 去重连续空行并移除尾部多余空行。
fn dedup_blank_lines(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    let mut result: Vec<Line<'static>> = Vec::with_capacity(lines.len());
    let mut prev_empty = false;
    for line in lines {
        let is_empty =
            line.spans.is_empty() || (line.spans.len() == 1 && line.spans[0].content.is_empty());
        if is_empty && prev_empty {
            continue;
        }
        prev_empty = is_empty;
        result.push(line);
    }
    // 移除末尾多余空行
    while result.last().is_some_and(|l| {
        l.spans.is_empty() || (l.spans.len() == 1 && l.spans[0].content.is_empty())
    }) {
        result.pop();
    }
    result
}

/// Build wrap_map: maps each logical line to its visual row span.
fn build_wrap_map(lines: &[Line<'static>], width: u16) -> (usize, Vec<WrappedLineInfo>) {
    if width == 0 || lines.is_empty() {
        return (0, Vec::new());
    }
    let mut wrap_map = Vec::with_capacity(lines.len());
    let mut visual_row: u16 = 0;

    for (idx, line) in lines.iter().enumerate() {
        let plain_text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        let char_widths: Vec<u8> = plain_text
            .graphemes(true)
            .map(|g| unicode_width::UnicodeWidthStr::width(g) as u8)
            .collect();

        let visual_count = if plain_text.is_empty() {
            1
        } else {
            let text = ratatui::text::Text::from(line.clone());
            let count = Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .line_count(width);
            count.max(1) as u16
        };

        wrap_map.push(WrappedLineInfo {
            line_idx: idx,
            visual_row_start: visual_row,
            visual_row_end: visual_row + visual_count,
            plain_text,
            char_widths,
        });
        visual_row += visual_count;
    }
    (visual_row as usize, wrap_map)
}

pub(crate) fn render_messages(
    f: &mut Frame,
    app: &mut App,
    v2_view_models: Option<&[peri_acp_types::view_model::ViewModel]>,
    header_area: Rect,
    messages_area: Rect,
) {
    let inner = messages_area;
    app.session_mgr.current_mut().ui.messages_area = Some(inner);
    let visible_height = inner.height;
    let text_area_width = inner.width.saturating_sub(1);
    let diff_visible = app.session_mgr.current().ui.diff_visible;

    // V2 path: render from state machine V2 ViewModels when available.
    // Falls back to legacy MessageState path when v2 data is not set.
    //
    // [LEGACY FALLBACK — Phase 2 migration in progress]
    // Production: `runtime::apply_context::draw_now` always passes
    // `Some(v2_vms)` to `ui::main_ui::render`, so this `else` branch is
    // unreachable in production — draw_now always passes v2_view_models=Some.
    // Headless tests hit this path via `main_ui::render(f, &mut app, None)`;
    // they seed v2_test_views via the seed_v2_* helpers.
    let effective_v2: Vec<peri_acp_types::view_model::ViewModel> =
        if let Some(v2_vms) = v2_view_models {
            v2_vms.to_vec()
        } else {
            // Phase 2.6 step 7e.9: v2_test_views is the headless test seed source.
            let test_views = &app.session_mgr.current().messages.v2_test_views;
            if !test_views.is_empty() {
                test_views.clone()
            } else {
                Vec::new()
            }
        };

    if effective_v2.is_empty() {
        welcome::render_welcome(f, app, messages_area);
        return;
    }

    // V2 ViewModels change every frame during streaming; always rebuild.
    let cache = build_sync_render_cache_v2(&effective_v2, diff_visible, text_area_width);
    app.session_mgr.current_mut().messages.message_cache = Some(cache);

    // 计算 loading spinner 行
    let spinner_line: Option<Line<'static>> = if app.session_mgr.current().ui.loading {
        let session = app.session_mgr.current();
        let frame = peri_widgets::spinner::animation::tick_to_frame(session.spinner_state.tick());
        let verb = session.spinner_state.verb();
        let elapsed =
            peri_widgets::spinner::animation::format_elapsed(session.spinner_state.elapsed_ms());
        let tokens = session.spinner_state.displayed_tokens();

        let is_compact = verb.starts_with("压缩上下文");
        let accent = if is_compact {
            Style::default().fg(theme::THINKING)
        } else {
            Style::default().fg(theme::ACCENT)
        };
        let gray = Style::default().fg(theme::MUTED);
        let mut parts = vec![
            Span::styled(format!(" {} {}", frame, verb), accent),
            Span::styled(format!(" ({elapsed}"), gray),
        ];
        if tokens > 0 {
            let tokens_fmt = peri_widgets::spinner::animation::format_tokens(tokens);
            parts.push(Span::styled(format!(" · ↓ {tokens_fmt} tokens"), gray));
        }
        parts.push(Span::styled(")", gray));
        Some(Line::from(parts))
    } else if app
        .session_mgr
        .current()
        .spinner_state
        .last_summary_elapsed_ms()
        > 0
    {
        let elapsed = peri_widgets::spinner::animation::format_elapsed(
            app.session_mgr
                .current()
                .spinner_state
                .last_summary_elapsed_ms(),
        );
        Some(Line::from(Span::styled(
            format!("  ✻  Brewed for {elapsed}"),
            Style::default().fg(theme::MUTED),
        )))
    } else {
        None
    };

    // ── 从 message_cache 读取并计算滚动参数 ──────────────────────────────────
    let spinner_extra: u16 = if spinner_line.is_some() {
        spinner_extra_count(app)
    } else {
        0
    };
    let (max_scroll, offset) = {
        let cache = app.session_mgr.current().messages.message_cache.as_ref();
        let (total_lines, _version, render_width) = match cache {
            Some(c) => (c.total_lines, c.version, c.width),
            None => (0, 0u64, 0u16),
        };

        let visual_total = (total_lines as u16).saturating_add(spinner_extra);
        let max_scroll = visual_total.saturating_sub(visible_height);
        let scroll_follow = app.session_mgr.current().ui.scroll_follow;
        let scroll_offset = app.session_mgr.current().ui.scroll_offset;
        let (new_follow, off) = if scroll_follow {
            (true, max_scroll)
        } else {
            let off = scroll_offset.min(max_scroll);
            let new_follow = off >= max_scroll;
            (new_follow, off)
        };

        app.session_mgr.current_mut().ui.scroll_follow = new_follow;
        app.session_mgr.current_mut().ui.scroll_offset = off;
        app.session_mgr.current_mut().ui.scrollbar_max_offset = max_scroll;

        if render_width != text_area_width {
            // 宽度未匹配：用已计算好的 effective_v2 重建缓存。
            // Phase 2.6 step 7e.6-pre: 之前从 view_messages clone + vm_convert，
            // 但 effective_v2 已经在 line 150 计算完毕（生产路径来自 state.view_models()）。
            // 复用避免：(1) view_messages 在 Phase 2.6 后将被删除；(2) 终端 resize 时
            // view_messages 与 state.view 可能短暂不一致导致渲染闪烁。
            let diff = app.session_mgr.current().ui.diff_visible;
            let prev = app.session_mgr.current().messages.message_cache.as_ref();
            let new_cache = build_sync_render_cache_v2(&effective_v2, diff, text_area_width);
            // 保持版本递增（与 v1 路径行为一致）
            let version = prev.map_or(1, |c| c.version.wrapping_add(1));
            app.session_mgr.current_mut().messages.message_cache = Some(MessageRenderCache {
                lines: new_cache.lines,
                wrap_map: new_cache.wrap_map,
                total_lines: new_cache.total_lines,
                version,
                width: new_cache.width,
            });
        }

        (max_scroll, off)
    };

    // 仅在有滚动条时显示 sticky header
    if max_scroll > 0 {
        sticky_header::render_sticky_header(f, app, header_area);
    }

    // 文字区域（留出右侧 1 列给滚动条）
    let text_area = Rect {
        width: inner.width.saturating_sub(1),
        ..inner
    };

    // ── 视口裁剪 ──────────────────────────────────────────────────────────
    let clip = viewport_clip(app, offset, visible_height, &spinner_line);

    let paragraph = Paragraph::new(Text::from(clip.lines))
        .scroll((clip.local_offset, 0))
        .wrap(Wrap { trim: false });
    f.render_widget(paragraph, text_area);

    // 滚动条
    if max_scroll > 0 {
        let bar_area = Rect {
            x: inner.right().saturating_sub(1),
            y: inner.y,
            width: 1,
            height: inner.height,
        };
        peri_widgets::render_vertical_scrollbar(
            f,
            bar_area,
            offset,
            max_scroll,
            Style::default().fg(theme::MUTED),
            None,
            false,
        );

        if offset < max_scroll {
            let btn_area = Rect {
                x: inner.right().saturating_sub(1),
                y: inner.bottom().saturating_sub(1),
                width: 1,
                height: 1,
            };
            let arrow = Paragraph::new(Text::from(Span::styled(
                "▼",
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::BOLD),
            )));
            f.render_widget(arrow, btn_area);
        }

        if offset > 0 {
            let btn_area = Rect {
                x: inner.right().saturating_sub(1),
                y: inner.y,
                width: 1,
                height: 1,
            };
            let arrow = Paragraph::new(Text::from(Span::styled(
                "▲",
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::BOLD),
            )));
            f.render_widget(arrow, btn_area);
        }
    }
}

/// 基于视口裁剪提取可见行。
fn viewport_clip(
    app: &App,
    offset: u16,
    visible_height: u16,
    spinner_line: &Option<Line<'static>>,
) -> ViewportClip {
    // ── 阶段 1：从 message_cache 提取可见行 ──
    let (mut lines, local_offset, first_idx, total_lines) = {
        let cache = app.session_mgr.current().messages.message_cache.as_ref();
        let (cache_lines, wrap_map, cache_total_lines) = match cache {
            Some(c) => (&c.lines, &c.wrap_map, c.total_lines),
            None => {
                return ViewportClip {
                    lines: Vec::new(),
                    local_offset: 0,
                };
            }
        };

        let vis_start = offset as usize;
        let vis_end = (offset as usize + visible_height as usize + 1).min(cache_total_lines);

        let first_visible =
            wrap_map.partition_point(|info| info.visual_row_end as usize <= vis_start);
        let last_visible = wrap_map
            .partition_point(|info| (info.visual_row_start as usize) < vis_end)
            .saturating_sub(1);

        let total_lines = cache_total_lines;

        let (lines, local_offset, first_idx) =
            if first_visible < wrap_map.len() && first_visible <= last_visible {
                let first_visual = wrap_map[first_visible].visual_row_start as usize;
                let local_offset = vis_start.saturating_sub(first_visual) as u16;
                let lines = cache_lines[first_visible..=last_visible].to_vec();
                (lines, local_offset, first_visible)
            } else {
                (Vec::new(), 0u16, cache_lines.len())
            };

        (lines, local_offset, first_idx, total_lines)
    };

    // ── 阶段 2：Spinner 行追加 ──
    if let Some(line) = spinner_line {
        let spinner_visual_start = total_lines;
        let spinner_extra = spinner_extra_count(app);
        let spinner_visual_end = spinner_visual_start + spinner_extra as usize;
        let vis_start = offset as usize;
        let viewport_bottom = offset as usize + visible_height as usize;

        if vis_start < spinner_visual_end && viewport_bottom > spinner_visual_start {
            lines.push(Line::from(""));
            lines.push(line.clone());
            if app.session_mgr.current().ui.loading {
                let tip = crate::ui::tips::pick_tip(
                    app.session_mgr.current().spinner_state.raw_tick(),
                    &app.services.lc,
                );
                lines.push(Line::from(vec![
                    Span::styled("  ⎿  Tip: ", Style::default().fg(theme::MUTED)),
                    Span::styled(tip, Style::default().fg(theme::MUTED)),
                ]));
                lines.push(Line::from(""));
                for item in &app.session_mgr.current().todo_items {
                    let (icon, icon_style, text_style) = match item.status {
                        TodoStatusDto::InProgress => (
                            "  ◼  ",
                            Style::default()
                                .fg(theme::ACCENT)
                                .add_modifier(Modifier::BOLD),
                            Style::default().fg(theme::TEXT),
                        ),
                        TodoStatusDto::Completed => (
                            "  ✔  ",
                            Style::default().fg(theme::SAGE),
                            Style::default()
                                .fg(theme::MUTED)
                                .add_modifier(Modifier::CROSSED_OUT),
                        ),
                        TodoStatusDto::Pending => (
                            "  ◻  ",
                            Style::default().fg(theme::MUTED),
                            Style::default().fg(theme::MUTED),
                        ),
                    };
                    let hint = match item.status {
                        TodoStatusDto::Pending => Some("可开始"),
                        _ => None,
                    };
                    let mut spans = vec![
                        Span::styled(icon, icon_style),
                        Span::styled(item.content.clone(), text_style),
                    ];
                    if let Some(hint) = hint {
                        spans.push(Span::styled(
                            format!(" ({hint})"),
                            Style::default().fg(theme::MUTED),
                        ));
                    }
                    lines.push(Line::from(spans));
                }
                for _ in 0..3 {
                    lines.push(Line::from(""));
                }
            } else {
                for _ in 0..3 {
                    lines.push(Line::from(""));
                }
            }
        }
    }

    // ── 阶段 3：字符级选区高亮 ──
    if app.session_mgr.current().ui.text_selection.is_active() {
        let ts = &app.session_mgr.current().ui.text_selection;
        if let (Some(start), Some(end)) = (ts.start, ts.end) {
            let usable_width = app
                .session_mgr
                .current()
                .ui
                .messages_area
                .map(|a| a.width.saturating_sub(1))
                .unwrap_or(0);

            let empty_wrap: Vec<WrappedLineInfo> = Vec::new();
            let wrap_map = app
                .session_mgr
                .current()
                .messages
                .message_cache
                .as_ref()
                .map(|c| &c.wrap_map)
                .unwrap_or(&empty_wrap);

            let ((sr, sc), (er, ec)) = if start <= end {
                (start, end)
            } else {
                (end, start)
            };
            let logical_start =
                crate::app::text_selection::visual_to_logical(sr, sc, wrap_map, usable_width);
            let logical_end =
                crate::app::text_selection::visual_to_logical(er, ec, wrap_map, usable_width);

            if let (Some((start_line, start_char)), Some((end_line, end_char))) =
                (logical_start, logical_end)
            {
                let clip_start = start_line.max(first_idx);
                let clip_end = end_line.min(first_idx + lines.len().saturating_sub(1));

                for line_idx in clip_start..=clip_end {
                    let local_idx = line_idx - first_idx;
                    let (cs, ce) = if line_idx == start_line && line_idx == end_line {
                        (start_char, end_char)
                    } else if line_idx == start_line {
                        (start_char, usize::MAX)
                    } else if line_idx == end_line {
                        (0, end_char)
                    } else {
                        (0, usize::MAX)
                    };
                    let spans = std::mem::take(&mut lines[local_idx].spans);
                    lines[local_idx] = Line::from(highlight_line_spans(spans, cs, ce));
                }
            }
        }
    }

    ViewportClip {
        lines,
        local_offset,
    }
}

/// 计算 spinner 区域的额外逻辑行数
fn spinner_extra_count(app: &App) -> u16 {
    if app.session_mgr.current().ui.loading {
        let base = 7u16;
        base + app.session_mgr.current().todo_items.len() as u16
    } else {
        5
    }
}

/// 对一行的 spans 做字符级选区高亮。
pub(crate) fn highlight_line_spans<'a>(
    spans: Vec<Span<'a>>,
    char_start: usize,
    char_end: usize,
) -> Vec<Span<'a>> {
    let mut result = Vec::new();
    let mut cursor: usize = 0;
    for span in spans {
        let span_char_len = span.content.chars().count();
        let span_start = cursor;
        let span_end = cursor + span_char_len;

        if span_end <= char_start || span_start >= char_end {
            result.push(span);
        } else if span_start >= char_start && span_end <= char_end {
            result.push(Span::styled(
                span.content,
                Style {
                    fg: span.style.fg,
                    bg: Some(theme::SELECTION_BG),
                    underline_color: span.style.underline_color,
                    add_modifier: span.style.add_modifier,
                    sub_modifier: span.style.sub_modifier,
                },
            ));
        } else {
            if span_start < char_start {
                let skip = char_start - span_start;
                let byte_cut = span
                    .content
                    .char_indices()
                    .nth(skip)
                    .map(|(i, _)| i)
                    .unwrap_or(span.content.len());
                result.push(Span::styled(
                    span.content[..byte_cut].to_string(),
                    span.style,
                ));
            }
            let hl_char_start = span_start.max(char_start) - span_start;
            let hl_char_end = span_end.min(char_end) - span_start;
            let byte_start = span
                .content
                .char_indices()
                .nth(hl_char_start)
                .map(|(i, _)| i)
                .unwrap_or(0);
            let byte_end = span
                .content
                .char_indices()
                .nth(hl_char_end)
                .map(|(i, _)| i)
                .unwrap_or(span.content.len());
            result.push(Span::styled(
                span.content[byte_start..byte_end].to_string(),
                Style {
                    fg: span.style.fg,
                    bg: Some(theme::SELECTION_BG),
                    underline_color: span.style.underline_color,
                    add_modifier: span.style.add_modifier,
                    sub_modifier: span.style.sub_modifier,
                },
            ));
            if span_end > char_end {
                let skip = char_end - span_start;
                let byte_cut = span
                    .content
                    .char_indices()
                    .nth(skip)
                    .map(|(i, _)| i)
                    .unwrap_or(span.content.len());
                result.push(Span::styled(
                    span.content[byte_cut..].to_string(),
                    span.style,
                ));
            }
        }
        cursor = span_end;
    }
    result
}

#[cfg(test)]
#[path = "message_area_test.rs"]
mod message_area_test;
