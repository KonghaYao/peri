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
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::Line,
        widgets::Paragraph,
    },
};

use crate::kit::atoms::REWIND_ACTION_TX;
use crate::kit::popup_overlay::close_popup;
use crate::kit::rewind_action::RewindAction;
use crate::ui::theme;

/// 视图切换——messages ↔ files。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RewindView {
    Messages,
    Files,
}

#[component]
pub fn RewindPopup(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let preview_store = hooks.use_store(*crate::kit::atoms::REWIND_PREVIEW.get().unwrap());
    let preview = preview_store.read().clone();
    let _ = preview_store;

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

    hooks.use_local_events(move |event: Event| {
        if let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
        {
            match (key.modifiers, key.code) {
                // ── 视图切换 ──
                (KeyModifiers::NONE, KeyCode::Tab)
                | (KeyModifiers::SHIFT, KeyCode::BackTab)
                | (KeyModifiers::NONE, KeyCode::BackTab) => {
                    let cur = *view.read();
                    *view.write() = match cur {
                        RewindView::Messages => RewindView::Files,
                        RewindView::Files => RewindView::Messages,
                    };
                }

                // ── 上下导航（按当前视图分派） ──
                (KeyModifiers::NONE, KeyCode::Up) => {
                    let cur = *view.read();
                    match cur {
                        RewindView::Messages => {
                            let mut s = msg_sel.write();
                            *s = s.saturating_sub(1);
                        }
                        RewindView::Files => {
                            let mut s = file_sel.write();
                            *s = s.saturating_sub(1);
                        }
                    }
                }
                (KeyModifiers::NONE, KeyCode::Down) => {
                    let cur = *view.read();
                    match cur {
                        RewindView::Messages => {
                            let mut s = msg_sel.write();
                            if msg_count > 0 {
                                *s = (*s).saturating_add(1).min(msg_count - 1);
                            }
                        }
                        RewindView::Files => {
                            let mut s = file_sel.write();
                            if file_count > 0 {
                                *s = (*s).saturating_add(1).min(file_count - 1);
                            }
                        }
                    }
                }

                // ── Enter：确认回退 ──
                // target_message_id 从 messages 选中条目取（无 messages 时占位空串——不发 RPC）
                (KeyModifiers::NONE, KeyCode::Enter) => {
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
                }

                // ── Esc：仅关闭 popup，由全局 event_handlers 处理 ──
                // 但作为兜底，本地也响应一次（防止事件被消费链卡住）
                (KeyModifiers::NONE, KeyCode::Esc) => {
                    if let Some(tx) = REWIND_ACTION_TX.get() {
                        let _ = tx.send(RewindAction::Cancel);
                    }
                    close_popup();
                }

                _ => {}
            }
        }
    });

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
                    .fg(theme::MUTED)
                    .italic(),
            );
            lines.push(Line::from(""));
            lines.push(Line::from("  Rewind 通常由 Agent 在工具调用前触发；").fg(theme::DIM));
            lines.push(Line::from("  或由历史面板右键选择消息后回退。").fg(theme::DIM));
            lines.push(Line::from(""));
            lines.push(Line::from("  Esc: close").fg(theme::DIM));
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
                    .fg(theme::TEXT)
                    .bold(),
            );

            if p.messages.is_empty() {
                lines.push(Line::from("    (no messages to rewind)").fg(theme::DIM));
            } else {
                for (i, msg) in p.messages.iter().enumerate().take(8) {
                    let is_selected = cur_view == RewindView::Messages && i == cur_msg_sel;
                    let prefix = if is_selected { "  > " } else { "    " };
                    let preview_text = truncate_str(&msg.preview, 40);
                    let role_label = role_display(&msg.role);
                    let line_text = format!("{}[{}] {}", prefix, role_label, preview_text);
                    if is_selected {
                        lines.push(Line::from(line_text).fg(theme::THINKING).bold());
                    } else {
                        lines.push(Line::from(line_text).fg(theme::TEXT));
                    }
                }
                if p.messages.len() > 8 {
                    lines.push(
                        Line::from(format!("    ... and {} more", p.messages.len() - 8))
                            .fg(theme::DIM),
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
                    .fg(theme::TEXT)
                    .bold(),
            );

            if p.files.is_empty() {
                lines.push(Line::from("    (no file changes)").fg(theme::DIM));
            } else {
                for (i, fc) in p.files.iter().enumerate().take(6) {
                    let is_selected = cur_view == RewindView::Files && i == cur_file_sel;
                    let prefix = if is_selected { "  > " } else { "    " };
                    let status_color = match fc.change_type.as_str() {
                        "added" => theme::SAGE,
                        "deleted" => theme::ERROR,
                        _ => theme::WARNING,
                    };
                    let path_text = truncate_str(&fc.path, 42);
                    let line_text = format!("{}{} ({})", prefix, path_text, fc.change_type);
                    if is_selected {
                        lines.push(Line::from(line_text).fg(theme::THINKING).bold());
                    } else {
                        lines.push(Line::from(line_text).fg(status_color));
                    }
                }
                if p.files.len() > 6 {
                    lines.push(
                        Line::from(format!("    ... and {} more", p.files.len() - 6))
                            .fg(theme::DIM),
                    );
                }
            }

            lines.push(Line::from(""));
            lines.push(
                Line::from(
                    "  Tab: switch view  |  ↑↓: select  |  Enter: rewind to selected  |  Esc: cancel",
                )
                .fg(theme::DIM),
            );
        }
    }

    let text_render = Paragraph::new(ratatui::text::Text::from(lines));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Rewind Changes ").fg(theme::WARNING).bold().centered(),
            width: Constraint::Length(60),
            height: Constraint::Length(22),
        ) {
            Text(text: text_render)
        }
    )
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
mod tests {
    use super::*;

    #[test]
    fn test_truncate_str_short() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_str_exact() {
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_str_long() {
        assert_eq!(truncate_str("hello world", 5), "hello…");
    }

    #[test]
    fn test_truncate_str_cjk() {
        // 中文字符 1 char = 3 bytes；chars().take 计 char 数不 panic
        assert_eq!(truncate_str("你好世界朋友", 4), "你好世界…");
    }

    #[test]
    fn test_role_display_known() {
        assert_eq!(role_display("user"), "U");
        assert_eq!(role_display("assistant"), "A");
        assert_eq!(role_display("system"), "S");
        assert_eq!(role_display("tool"), "T");
    }

    #[test]
    fn test_role_display_unknown() {
        assert_eq!(role_display("custom"), "?");
        assert_eq!(role_display(""), "?");
    }

    #[test]
    fn test_rewind_view_toggle() {
        assert_ne!(RewindView::Messages, RewindView::Files);
        let v = RewindView::Messages;
        match v {
            RewindView::Messages => {}
            RewindView::Files => panic!("expected Messages"),
        }
    }
}
