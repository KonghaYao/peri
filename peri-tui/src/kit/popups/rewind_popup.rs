//! ratatui-kit RewindPopup component.
//!
//! 回退变更弹窗：从 REWIND_PREVIEW atom 读取真实数据（files + messages），
//! 支持列表选择、Tab 切换视图、Enter 确认（默认回退到选中 message + revert_files=true）、
//! Esc 取消。
//!
//! ## 数据源
//!
//! 由 `kit/acp_events.rs::dispatch_and_notify` 在收到 `RewindPreview` 事件时
//! 写入 `REWIND_PREVIEW` atom。用户双击 Esc 触发 popup 时若 atom 为 None，
//! 显示"无可回退"占位（不发 RPC）。
//!
//! ## 用户路径
//!
//! - **Up/Down**：在当前视图（messages 或 files）中选择条目
//! - **Tab/BackTab**：在 messages ↔ files 间切换
//! - **Enter**：发送 `RewindAction::Confirm { target_message_id, revert_files: true }`
//!   到 REWIND_ACTION_TX，然后 close_popup
//! - **Esc**：由 event_handlers 全局链关闭 popup（不发 Cancel RPC——避免冗余）

#![allow(clippy::needless_update)]

use ratatui_kit::{
    crossterm::event::{Event, KeyEventKind},
    prelude::*,
    ratatui::{style::Stylize, text::Line},
};

use crate::i18n;
use crate::kit::atoms::{LANG_VERSION, REWIND_ACTION_TX};
use crate::kit::list_nav::{ListNavAction, classify_list_nav, next_selection, previous_selection};
use crate::kit::popup_overlay::close_popup;
use crate::kit::rewind_action::RewindAction;
use peri_theme::atoms::THEME_ATOM;

/// 视图切换——messages ↔ files。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RewindView {
    Messages,
    Files,
}

#[component]
pub fn RewindPopup(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let preview_store = hooks.use_atom(&crate::kit::atoms::REWIND_PREVIEW);
    let preview = preview_store.read().clone();
    let _ = preview_store;

    hooks.use_atom(&LANG_VERSION);

    // 当前视图（messages 默认；用户可 Tab 切到 files）
    let view = hooks.use_state(|| RewindView::Messages);
    // messages 视图选中索引（默认最新一条 = 回退一步）
    let msg_sel = hooks.use_state(|| 0usize);
    // files 视图选中索引
    let file_sel = hooks.use_state(|| 0usize);

    let msg_count = preview.as_ref().map(|p| p.messages.len()).unwrap_or(0);
    let file_count = preview.as_ref().map(|p| p.files.len()).unwrap_or(0);

    // 闭包另持一份 preview 副本（避免与渲染端争用 move）
    let preview_for_closure = preview.clone();

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
        if let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
        {
            match classify_list_nav(&key) {
                Some(ListNavAction::CycleForward | ListNavAction::CycleBackward) => {
                    let cur = *view.read();
                    *view.write() = match cur {
                        RewindView::Messages => RewindView::Files,
                        RewindView::Files => RewindView::Messages,
                    };
                    return EventResult::Consumed;
                }
                Some(ListNavAction::MoveUp) => {
                    let cur = *view.read();
                    match cur {
                        RewindView::Messages => {
                            let mut s = msg_sel.write();
                            *s = previous_selection(*s);
                        }
                        RewindView::Files => {
                            let mut s = file_sel.write();
                            *s = previous_selection(*s);
                        }
                    }
                    return EventResult::Consumed;
                }
                Some(ListNavAction::MoveDown) => {
                    let cur = *view.read();
                    match cur {
                        RewindView::Messages => {
                            let mut s = msg_sel.write();
                            *s = next_selection(*s, msg_count);
                        }
                        RewindView::Files => {
                            let mut s = file_sel.write();
                            *s = next_selection(*s, file_count);
                        }
                    }
                    return EventResult::Consumed;
                }
                Some(ListNavAction::Confirm) => {
                    let target_id = match &preview_for_closure {
                        Some(p) => p
                            .messages
                            .get(*msg_sel.read())
                            .map(|m| m.id.clone())
                            .unwrap_or_default(),
                        None => String::new(),
                    };
                    if !target_id.is_empty()
                        && let Some(tx) = REWIND_ACTION_TX.get()
                    {
                        let _ = tx.send(RewindAction::Confirm {
                            target_message_id: target_id,
                            revert_files: true,
                        });
                    }
                    close_popup();
                    return EventResult::Consumed;
                }
                Some(ListNavAction::Cancel) => {
                    close_popup();
                    return EventResult::Consumed;
                }
                None => {}
            }
        }
        EventResult::Ignored
    });

    let popup_tokens = &theme_def.read().component.popup;
    let guard = theme_def.read();
    let semantic = &guard.semantic;
    let cur_view = *view.read();
    let cur_msg_sel = *msg_sel.read();
    let cur_file_sel = *file_sel.read();

    let mut lines: Vec<Line<'_>> = Vec::new();

    match &preview {
        None => {
            // 无预览数据——双击 Esc 触发的空 popup
            lines.push(Line::from(""));
            lines.push(
                Line::from("  No rewind preview available.")
                    .fg(semantic.text.muted)
                    .italic(),
            );
            lines.push(Line::from(""));
            lines
                .push(Line::from("  Rewind 通常由 Agent 在工具调用前触发；").fg(semantic.text.dim));
            lines.push(Line::from("  或由历史面板右键选择消息后回退。").fg(semantic.text.dim));
            lines.push(Line::from(""));
            lines.push(Line::from(i18n::tr("common-esc-close")).fg(semantic.text.dim));
        }
        Some(p) => {
            lines.push(Line::from(""));

            // ── Messages 视图 ──
            let msg_marker = if cur_view == RewindView::Messages {
                "▶"
            } else {
                " "
            };
            lines.push(
                Line::from(format!("{} Messages ({})", msg_marker, p.messages.len()))
                    .fg(semantic.text.primary)
                    .bold(),
            );

            if p.messages.is_empty() {
                lines.push(Line::from("    (no messages to rewind)").fg(semantic.text.dim));
            } else {
                for (i, msg) in p.messages.iter().enumerate().take(8) {
                    let is_selected = cur_view == RewindView::Messages && i == cur_msg_sel;
                    let prefix = if is_selected { "  > " } else { "    " };
                    let preview_text = truncate_str(&msg.preview, 40);
                    let role_label = role_display(&msg.role);
                    let line_text = format!("{}[{}] {}", prefix, role_label, preview_text);
                    if is_selected {
                        lines.push(Line::from(line_text).fg(popup_tokens.selected_fg).bold());
                    } else {
                        lines.push(Line::from(line_text).fg(semantic.text.primary));
                    }
                }
                if p.messages.len() > 8 {
                    lines.push(
                        Line::from(format!("    ... and {} more", p.messages.len() - 8))
                            .fg(semantic.text.dim),
                    );
                }
            }

            lines.push(Line::from(""));

            // ── Files 视图 ──
            let file_marker = if cur_view == RewindView::Files {
                "▶"
            } else {
                " "
            };
            lines.push(
                Line::from(format!("{} Files ({})", file_marker, p.files.len()))
                    .fg(semantic.text.primary)
                    .bold(),
            );

            if p.files.is_empty() {
                lines.push(Line::from("    (no file changes)").fg(semantic.text.dim));
            } else {
                for (i, fc) in p.files.iter().enumerate().take(6) {
                    let is_selected = cur_view == RewindView::Files && i == cur_file_sel;
                    let prefix = if is_selected { "  > " } else { "    " };
                    let status_color = match fc.change_type.as_str() {
                        "added" => semantic.status.success,
                        "deleted" => semantic.status.error,
                        _ => semantic.status.warning,
                    };
                    let path_text = truncate_str(&fc.path, 42);
                    let line_text = format!("{}{} ({})", prefix, path_text, fc.change_type);
                    if is_selected {
                        lines.push(Line::from(line_text).fg(popup_tokens.selected_fg).bold());
                    } else {
                        lines.push(Line::from(line_text).fg(status_color));
                    }
                }
                if p.files.len() > 6 {
                    lines.push(
                        Line::from(format!("    ... and {} more", p.files.len() - 6))
                            .fg(semantic.text.dim),
                    );
                }
            }

            lines.push(Line::from(""));
            lines.push(Line::from(i18n::tr("rewind-confirm-hint")).fg(semantic.text.dim));
        }
    }

    popup_text_shell!(i18n::tr("rewind-title"), semantic.status.warning, lines)
}

// ── 辅助函数 ─────────────────────────────────────────────────────────────

/// 截断字符串到 max chars（CJK 安全）。
fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{}…", truncated)
    }
}

/// role 字符串映射为显示标签。
fn role_display(role: &str) -> &str {
    match role {
        "user" => "U",
        "assistant" => "A",
        "system" => "S",
        "tool" => "T",
        _ => "?",
    }
}

#[cfg(test)]
#[path = "rewind_popup_test.rs"]
mod tests;
