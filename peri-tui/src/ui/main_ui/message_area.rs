use peri_acp::event::TodoStatusDto;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Wrap},
    Frame,
};

use super::sticky_header;
use crate::{
    app::App,
    ui::{render_thread::RenderEvent, theme, welcome},
};

/// 视口裁剪结果
struct ViewportClip {
    /// 裁剪后的可见行（含 spinner 和选区高亮）
    lines: Vec<Line<'static>>,
    /// 裁剪后的局部滚动偏移（相对于 lines[0] 的视觉行偏移）
    local_offset: u16,
}

pub(crate) fn render_messages(
    f: &mut Frame,
    app: &mut App,
    header_area: Rect,
    messages_area: Rect,
) {
    // Welcome Card 或消息列表
    if app.session_mgr.current().messages.view_messages.is_empty() {
        welcome::render_welcome(f, app, messages_area);
        return;
    }

    let inner = messages_area;
    app.session_mgr.current_mut().ui.messages_area = Some(inner);
    let visible_height = inner.height;

    // 计算 loading spinner 行（Claude Code 风格：✻ verb (Xm Xs · ↓ X.Xk tokens)）
    // compact 时紫色，其余橙色；loading 结束后显示总结行：✻ Brewed for Xm Xs
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

    // 渲染驱动宽度同步：用 last_resize_width 去抖——宽度未变时跳过重复发送，
    // 避免每秒 N 次 resize 事件导致渲染线程队列积压和 CPU 暴涨
    // （参见 spec/issues/2026-05-14-streaming-resize-cpu-spike）。
    {
        let text_area_width = inner.width.saturating_sub(1);
        let cache_width = app.session_mgr.current().messages.render_cache.read().width;
        let last_resize = app.session_mgr.current().messages.last_resize_width;
        if last_resize != Some(text_area_width)
            && cache_width != text_area_width
            && text_area_width > 0
        {
            app.session_mgr.current_mut().messages.last_resize_width = Some(text_area_width);
            let _ = app
                .session_mgr
                .current_mut()
                .messages
                .render_tx
                .try_send(RenderEvent::Resize(text_area_width));
        }
    }

    // ── 从 RenderCache 读取并计算滚动参数 ──────────────────────────────────
    let spinner_extra: u16 = if spinner_line.is_some() {
        spinner_extra_count(app)
    } else {
        0
    };
    let (max_scroll, offset) = {
        let cache = app.session_mgr.current().messages.render_cache.read();

        let total_lines = cache.total_lines;
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

        let version = cache.version;
        drop(cache);

        app.session_mgr.current_mut().messages.last_render_version = version;
        app.session_mgr.current_mut().ui.scroll_follow = new_follow;
        app.session_mgr.current_mut().ui.scroll_offset = off;
        app.session_mgr.current_mut().ui.scrollbar_max_offset = max_scroll;

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
    // 利用 wrap_map 定位可见的逻辑行范围，只传递视口内的行给 Paragraph，
    // 避免 ratatui Paragraph::render 内部 O(offset) 的 WordWrapper 遍历导致 CPU 暴涨。
    // （ratatui 即使设了 scroll(offset)，仍会对 offset 之前的所有行做 grapheme 分割 + wrap 计算）
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
            None,  // max_thumb_len
            false, // show_arrows: 箭头由下面手动渲染
        );

        // 滚动到底按钮（当用户滚离底部时显示）
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

        // 滚动到顶按钮（当用户滚离顶部时显示）
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
///
/// 策略：
/// 1. 从 RenderCache 的 wrap_map 用二分查找定位 [vis_start, vis_end) 视觉行范围
///    对应的逻辑行 [first_visible, last_visible]
/// 2. 只克隆这些逻辑行（O(visible) 而非 O(total)）
/// 3. 计算局部滚动偏移（local_offset = vis_start - first_visible.visual_row_start）
/// 4. 检查 spinner 行是否在视口范围内（spinner 在底部，visual 行号 > cache.total_lines）
/// 5. 如果有文本选区，在裁剪后的行上做高亮
fn viewport_clip(
    app: &App,
    offset: u16,
    visible_height: u16,
    spinner_line: &Option<Line<'static>>,
) -> ViewportClip {
    // ── 阶段 1：从 cache 提取可见行（cache guard 在 block 结束时 drop） ──
    let (mut lines, local_offset, first_idx, total_lines) = {
        let cache = app.session_mgr.current().messages.render_cache.read();

        let vis_start = offset as usize;
        // +1 给 wrap 行留余量
        let vis_end = (offset as usize + visible_height as usize + 1).min(cache.total_lines);

        // wrap_map 按 visual_row_start 升序排列
        // 二分找第一个 visual_row_end > vis_start 的行（即首个与视口相交的行）
        let first_visible = cache
            .wrap_map
            .partition_point(|info| info.visual_row_end as usize <= vis_start);
        // 二分找最后一个 visual_row_start < vis_end 的行
        let last_visible = cache
            .wrap_map
            .partition_point(|info| (info.visual_row_start as usize) < vis_end)
            .saturating_sub(1);

        let total_lines = cache.total_lines;

        let (lines, local_offset, first_idx) =
            if first_visible < cache.wrap_map.len() && first_visible <= last_visible {
                let first_visual = cache.wrap_map[first_visible].visual_row_start as usize;
                let local_offset = vis_start.saturating_sub(first_visual) as u16;
                let lines = cache.lines[first_visible..=last_visible].to_vec();
                (lines, local_offset, first_visible)
            } else {
                // 空内容或视口超出范围
                (Vec::new(), 0u16, cache.lines.len())
            };

        (lines, local_offset, first_idx, total_lines)
    }; // cache guard dropped here

    // ── 阶段 2：Spinner 行追加（无需 cache） ──
    // spinner 行在视觉上排在 cache.total_lines 之后。
    // 检查视口是否覆盖到 spinner 区域
    if let Some(line) = spinner_line {
        let spinner_visual_start = total_lines;
        let spinner_extra = spinner_extra_count(app);
        let spinner_visual_end = spinner_visual_start + spinner_extra as usize;
        let vis_start = offset as usize;
        // 视口底边不截断到 total_lines——spinner 在 total_lines 之外，
        // 必须用完整的视口范围才能正确检测交集
        let viewport_bottom = offset as usize + visible_height as usize;

        // 视口与 spinner 区域有交集
        if vis_start < spinner_visual_end && viewport_bottom > spinner_visual_start {
            // 追加 spinner 分隔空行 + spinner line
            lines.push(Line::from(""));
            lines.push(line.clone());
            // Tip + TODO
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

    // ── 阶段 3：字符级选区高亮（需要再次读 cache 获取 wrap_map） ──
    // 只在裁剪后的可见行上做高亮（减少工作量）
    if app.session_mgr.current().ui.text_selection.is_active() {
        let ts = &app.session_mgr.current().ui.text_selection;
        if let (Some(start), Some(end)) = (ts.start, ts.end) {
            let cache = app.session_mgr.current().messages.render_cache.read();
            let wrap_map = &cache.wrap_map;
            let usable_width = app
                .session_mgr
                .current()
                .ui
                .messages_area
                .map(|a| a.width.saturating_sub(1))
                .unwrap_or(0);

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
                // lines 中的第 i 行对应 cache.lines 中的 first_idx + i
                // 只处理 [start_line, end_line] ∩ [first_idx, first_idx + lines.len()) 的行
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
        // 空行(1) + spinner(1) + tip(1) + 空行(1) + todo_items(N) + trailing(3) = 7 + N
        let base = 7u16;
        base + app.session_mgr.current().todo_items.len() as u16
    } else {
        // 空行(1) + spinner(1) + trailing(3) = 5
        5
    }
}

/// 对一行的 spans 做字符级选区高亮。
/// `char_start` / `char_end` 是该行 plain_text 的字符偏移（非 byte 索引）。
/// 将 spans 中对应范围的字符的 style 追加淡蓝色背景（深色主题选区色），范围外的 span 保持原样。
/// 使用 char_indices() 保证 unicode 安全切割。
pub(crate) fn highlight_line_spans<'a>(
    spans: Vec<Span<'a>>,
    char_start: usize,
    char_end: usize,
) -> Vec<Span<'a>> {
    let mut result = Vec::new();
    let mut cursor: usize = 0; // 当前在 plain_text 中的字符位置
    for span in spans {
        let span_char_len = span.content.chars().count();
        let span_start = cursor;
        let span_end = cursor + span_char_len;

        if span_end <= char_start || span_start >= char_end {
            // 完全在选区外 → 保持原样
            result.push(span);
        } else if span_start >= char_start && span_end <= char_end {
            // 完全在选区内 → 淡蓝色背景（强制覆盖原有 bg）
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
            // 部分重叠 → 拆分为 2~3 个子 span
            // 左段（选区外）
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
            // 中段（选区内，淡蓝色背景）
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
            // 右段（选区外）
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
