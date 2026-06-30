use anyhow::Result;
use base64::Engine as _;
use ratatui::layout::Rect;

use crate::app::App;

/// Checks whether a mouse event falls within a given rectangle area.
pub fn mouse_in_rect(mouse: &ratatui::crossterm::event::MouseEvent, area: Rect) -> bool {
    mouse.row >= area.y
        && mouse.row < area.y + area.height
        && mouse.column >= area.x
        && mouse.column < area.x + area.width
}

/// Converts a terminal display column position to a character index within a line.
///
/// CJK and other full-width characters occupy 2 display columns. `mouse.column` is
/// a terminal column coordinate, but `CursorMove::Jump(row, col)` expects `col` as
/// a character index. This function accumulates `unicode_width` per character and
/// returns the largest character index whose display end does not exceed `display_col`.
pub fn display_col_to_char_idx(line: &str, display_col: usize) -> usize {
    let mut col = 0usize;
    for (char_idx, ch) in line.chars().enumerate() {
        if col >= display_col {
            return char_idx;
        }
        col += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
    }
    // Click past end of line → return index at end of line
    line.chars().count()
}

/// Converts a mouse coordinate within a textarea's rendered area to a
/// textarea (row, char_idx) cursor position.
///
/// Four offsets are accounted for:
/// 1. **Block border + padding**: textarea renders within `Block::inner(area)`;
///    mouse coordinates must subtract these offsets to obtain text-area coordinates.
/// 2. **Vertical scroll offset**: when the text has more lines than the visible area
///    the textarea scrolls vertically (`top_row`); visible row 0 maps to text row `top_row`.
/// 3. **Horizontal scroll offset**: when text overflows horizontally the textarea
///    scrolls horizontally (`top_col`); visible column 0 maps to text column `top_col`
///    (in display columns).
/// 4. **CJK character width**: `Jump(row, col)` expects `col` as a character index,
///    not a display column. Conversion uses `unicode_width` per character.
///
/// `top_row` and `top_col` are inferred from cursor position because
/// `tui_textarea`'s viewport is private.
pub fn textarea_mouse_to_cursor(
    textarea: &tui_textarea::TextArea<'_>,
    textarea_area: ratatui::layout::Rect,
    mouse: &ratatui::crossterm::event::MouseEvent,
) -> (usize, usize) {
    // 1. Compute inner area (stripping border + padding)
    let inner = textarea
        .block()
        .map(|b| b.inner(textarea_area))
        .unwrap_or(textarea_area);
    let inner_width = inner.width as usize;
    let inner_height = inner.height as usize;

    // Mouse coordinates relative to inner area (saturating to avoid u16 overflow
    // when clicking on borders)
    let visual_row = mouse.row.saturating_sub(inner.y) as usize;
    let visual_col = mouse.column.saturating_sub(inner.x) as usize;

    // 2. Infer vertical scroll offset (top_row)
    // tui_textarea uses next_scroll_top logic: cursor < top_row => top_row = cursor;
    // cursor >= top_row + height => top_row = cursor + 1 - height; else unchanged.
    // Since viewport is private, we infer from cursor position:
    // cursor is always within [top_row, top_row + height), so top_row <= cursor_row
    let (cursor_row, cursor_col) = textarea.cursor();
    let scroll_row = cursor_row.saturating_sub(inner_height.saturating_sub(1));

    // 3. Infer horizontal scroll offset (top_col, in display columns)
    let cursor_line = textarea
        .lines()
        .get(cursor_row)
        .map(|s| s.as_str())
        .unwrap_or("");
    let cursor_display_col: usize = cursor_line
        .chars()
        .take(cursor_col)
        .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0))
        .sum();
    let scroll_col = cursor_display_col.saturating_sub(inner_width.saturating_sub(1));

    // 4. Text row and display column
    let target_row = scroll_row + visual_row;
    let text_display_col = visual_col + scroll_col;

    // 5. Convert display column to character index
    let target_row = target_row.min(textarea.lines().len().saturating_sub(1));
    let target_line = textarea
        .lines()
        .get(target_row)
        .map(|s| s.as_str())
        .unwrap_or("");
    let char_idx = display_col_to_char_idx(target_line, text_display_col);

    (target_row, char_idx)
}

/// Encodes RGBA pixel data as PNG, returning a base64 string and the PNG byte count.
pub fn rgba_to_png_base64(width: u32, height: u32, rgba_bytes: &[u8]) -> Result<(String, usize)> {
    let mut png_bytes: Vec<u8> = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(rgba_bytes)?;
    }
    let size = png_bytes.len();
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
    Ok((b64, size))
}

/// Handle mouse events for message-area interactions NOT covered by the
/// state machine. The SM already handles:
/// - Scroll events → `Effect::Scroll`
/// - Textarea mouse click/drag/release → `Effect::MouseTextarea*`
/// - Panel mouse → `Modal::handle_mouse`
///
/// This function covers the remaining legacy behaviors:
/// - Message area text selection (start/drag/end + extract + clipboard copy)
/// - Message area scrollbar drag
/// - Scroll-to-top/bottom buttons
/// - AskUser popup scrollbar interaction
pub fn handle_mouse_event(app: &mut App, mouse: &ratatui::crossterm::event::MouseEvent) {
    use ratatui::crossterm::event::{MouseButton, MouseEventKind};

    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // ── AskUser 弹窗滚动条点击 ──────────────────────────────
            {
                if let Some(crate::app::InteractionPrompt::Questions(ref p)) =
                    app.session_mgr.current_mut().agent.interaction_prompt
                {
                    if let Some(metrics) = p.scrollbar_metrics {
                        if mouse.column >= metrics.bar_area.x
                            && mouse.column < metrics.bar_area.x + metrics.bar_area.width
                            && mouse.row >= metrics.bar_area.y
                            && mouse.row < metrics.bar_area.bottom()
                            && metrics.max_offset > 0
                        {
                            let bar_inner_height = metrics.bar_area.height.saturating_sub(2);
                            if bar_inner_height > 0 {
                                let rel_y = (mouse.row.saturating_sub(metrics.bar_area.y + 1))
                                    .min(bar_inner_height);
                                let new_offset = ((rel_y as f64 / bar_inner_height as f64)
                                    * metrics.max_offset as f64)
                                    as u16;
                                let new_offset = new_offset.min(metrics.max_offset);
                                if let Some(crate::app::InteractionPrompt::Questions(p)) = app
                                    .session_mgr
                                    .current_mut()
                                    .agent
                                    .interaction_prompt
                                    .as_mut()
                                {
                                    p.scroll_offset = new_offset;
                                }
                            }
                            return;
                        }
                    }
                }
            }

            if let Some(area) = app.session_mgr.current_mut().ui.messages_area {
                let scroll_offset = app.session_mgr.current_mut().ui.scroll_offset;
                let scroll_follow = app.session_mgr.current_mut().ui.scroll_follow;

                // Scroll-to-bottom button
                let btn_col_start = area.right().saturating_sub(2);
                let btn_row_start = area.bottom().saturating_sub(2);
                if !scroll_follow
                    && mouse.column >= btn_col_start
                    && mouse.column < area.right()
                    && mouse.row >= btn_row_start
                    && mouse.row < area.bottom()
                {
                    app.scroll_to_bottom();
                    return;
                }

                // Scroll-to-top button
                if scroll_offset > 0
                    && mouse.column >= btn_col_start
                    && mouse.column < area.right()
                    && mouse.row >= area.y
                    && mouse.row < area.y.saturating_add(2)
                {
                    app.scroll_to_top();
                    return;
                }

                // Scrollbar drag: click on rightmost column
                let scrollbar_col = area.right().saturating_sub(1);
                if mouse.column == scrollbar_col && mouse.row >= area.y && mouse.row < area.bottom()
                {
                    let track = area.height;
                    if track > 0 {
                        let max_scroll = app.session_mgr.current_mut().ui.scrollbar_max_offset;
                        let rel_y = mouse.row.saturating_sub(area.y);
                        let new_offset = if max_scroll > 0 {
                            ((rel_y as f64 / track as f64) * max_scroll as f64)
                                .clamp(0.0, max_scroll as f64) as u16
                        } else {
                            0
                        };
                        app.session_mgr.current_mut().ui.scroll_offset = new_offset;
                        app.session_mgr.current_mut().ui.scroll_follow = false;
                        app.session_mgr.current_mut().ui.scrollbar_dragging = true;
                        app.session_mgr.current_mut().ui.scrollbar_drag_start_y = mouse.row;
                        app.session_mgr.current_mut().ui.scrollbar_drag_start_offset = new_offset;
                    }
                    return;
                }

                // Message area: start text selection
                if mouse.row >= area.y
                    && mouse.row < area.y + area.height
                    && mouse.column >= area.x
                    && mouse.column < area.x + area.width
                {
                    let visual_row =
                        mouse.row - area.y + app.session_mgr.current_mut().ui.scroll_offset;
                    let visual_col = mouse.column - area.x;
                    app.session_mgr
                        .current_mut()
                        .ui
                        .text_selection
                        .start_drag(visual_row, visual_col);
                }
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            // Scrollbar drag
            if app.session_mgr.current_mut().ui.scrollbar_dragging {
                if let Some(area) = app.session_mgr.current_mut().ui.messages_area {
                    let track = area.height;
                    if track > 0 {
                        let max_scroll = app.session_mgr.current_mut().ui.scrollbar_max_offset;
                        let start_y = app.session_mgr.current_mut().ui.scrollbar_drag_start_y;
                        let start_offset =
                            app.session_mgr.current_mut().ui.scrollbar_drag_start_offset;
                        if max_scroll > 0 {
                            let delta_y = mouse.row as i32 - start_y as i32;
                            let delta_offset =
                                (delta_y as f64 * (max_scroll as f64 / track as f64)) as i32;
                            let new_offset = (start_offset as i32 + delta_offset)
                                .clamp(0, max_scroll as i32)
                                as u16;
                            app.session_mgr.current_mut().ui.scroll_offset = new_offset;
                            app.session_mgr.current_mut().ui.scroll_follow = false;
                        }
                    }
                }
            }
            // Text selection drag
            if app.session_mgr.current_mut().ui.text_selection.dragging {
                if let Some(area) = app.session_mgr.current_mut().ui.messages_area {
                    let visual_row = mouse
                        .row
                        .saturating_sub(area.y)
                        .saturating_add(app.session_mgr.current_mut().ui.scroll_offset);
                    let visual_col = mouse.column.saturating_sub(area.x);
                    app.session_mgr
                        .current_mut()
                        .ui
                        .text_selection
                        .update_drag(visual_row, visual_col);
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            app.session_mgr.current_mut().ui.scrollbar_dragging = false;
            if app.session_mgr.current_mut().ui.text_selection.dragging {
                app.session_mgr.current_mut().ui.text_selection.end_drag();
                let sel_start = app.session_mgr.current().ui.text_selection.start;
                let sel_end = app.session_mgr.current().ui.text_selection.end;
                if let (Some(start), Some(end)) = (sel_start, sel_end) {
                    let usable_width = app
                        .session_mgr
                        .current()
                        .ui
                        .messages_area
                        .map(|a| a.width.saturating_sub(1))
                        .unwrap_or(0);
                    let text =
                        if let Some(ref cache) = app.session_mgr.current().messages.message_cache {
                            crate::app::text_selection::extract_selected_text(
                                start,
                                end,
                                &cache.wrap_map,
                                usable_width,
                            )
                        } else {
                            None
                        };
                    app.session_mgr
                        .current_mut()
                        .ui
                        .text_selection
                        .set_selected_text(text);
                }
                copy_selection_to_clipboard(app);
            }
        }
        _ => {}
    }
}

/// Copies the current text selection to the system clipboard and updates UI
/// hints. Returns `true` if text was successfully copied.
pub fn copy_selection_to_clipboard(app: &mut App) -> bool {
    if let Some(text) = app
        .session_mgr
        .current_mut()
        .ui
        .text_selection
        .selected_text
        .take()
    {
        let char_count = text.chars().count();
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_text(&text);
        }
        app.session_mgr.current_mut().ui.copy_char_count = char_count;
        app.session_mgr.current_mut().ui.copy_message_until =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(2000));
        app.session_mgr.current_mut().ui.text_selection.clear();
        return true;
    }
    false
}

/// Copies the current panel selection to the system clipboard. Returns `true`
/// if text was successfully copied.
pub fn copy_panel_selection_to_clipboard(app: &mut App) -> bool {
    if let Some(text) = app
        .session_mgr
        .current_mut()
        .ui
        .panel_selection
        .selected_text
        .take()
    {
        let char_count = text.chars().count();
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_text(&text);
        }
        app.session_mgr.current_mut().ui.copy_char_count = char_count;
        app.session_mgr.current_mut().ui.copy_message_until =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(2000));
        app.session_mgr.current_mut().ui.panel_selection.clear();
        return true;
    }
    false
}
