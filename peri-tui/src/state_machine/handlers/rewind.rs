//! Rewind preview handler.
//!
//! Wraps a [`peri_acp_types::event_data::RewindPreview`] payload and
//! dispatches confirm/cancel keys (Enter/y = submit, Esc/n/q = dismiss).
//!
//! Phase 1.4: implements `render` to draw a bordered popup listing the
//! pending file changes + messages that will be undone, plus a confirm/cancel
//! hint line. `desired_height` sizes the popup to fit the content (files +
//! messages + header + hints + border), capped to 60% of the screen.

use peri_acp_types::event_data::RewindPreview;
use peri_widgets::BorderedPanel;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
};

use super::super::state::{Handler, HandlerOutput};
use crate::ui::theme;

/// Handler for a `"rewind-preview"` event. Holds the change preview.
#[derive(Debug)]
pub struct RewindHandler {
    /// The file/message change preview received from the ACP layer.
    pub preview: RewindPreview,
}

impl RewindHandler {
    /// Create a new handler from a rewind-preview payload.
    pub fn new(preview: RewindPreview) -> Self {
        Self { preview }
    }
}

impl Handler for RewindHandler {
    fn render(&self, frame: &mut ratatui::Frame, area: Rect) {
        let inner = BorderedPanel::new(Span::styled(
            "Rewind Preview",
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme::ACCENT))
        .render(frame, area);
        let max_width = inner.width as usize;

        let mut lines: Vec<Line> = Vec::new();

        // ── Messages section ───────────────────────────────────────────
        if !self.preview.messages.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("Messages ({}):", self.preview.messages.len()),
                Style::default()
                    .fg(theme::WARNING)
                    .add_modifier(Modifier::BOLD),
            )));
            let max_preview = max_width.saturating_sub(6);
            for msg in &self.preview.messages {
                let preview: String = if msg.preview.chars().count() > max_preview {
                    format!(
                        "{}…",
                        msg.preview.chars().take(max_preview).collect::<String>()
                    )
                } else {
                    msg.preview.clone()
                };
                lines.push(Line::from(vec![
                    Span::styled("  • ", Style::default().fg(theme::MUTED)),
                    Span::styled(
                        format!("[{}] ", msg.role),
                        Style::default().fg(theme::MUTED),
                    ),
                    Span::styled(preview, Style::default().fg(theme::TEXT)),
                ]));
            }
        }

        // ── Files section ──────────────────────────────────────────────
        if !self.preview.files.is_empty() {
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }
            lines.push(Line::from(Span::styled(
                format!("Files ({}):", self.preview.files.len()),
                Style::default()
                    .fg(theme::WARNING)
                    .add_modifier(Modifier::BOLD),
            )));
            let max_path = max_width.saturating_sub(8);
            for fc in &self.preview.files {
                let path: String = fc.path.chars().take(max_path).collect();
                lines.push(Line::from(vec![
                    Span::styled("  • ", Style::default().fg(theme::MUTED)),
                    Span::styled(path, Style::default().fg(theme::TEXT)),
                    Span::styled(
                        format!(" ({})", fc.change_type),
                        Style::default().fg(theme::MUTED),
                    ),
                ]));
            }
        }

        // ── Empty state ────────────────────────────────────────────────
        if self.preview.messages.is_empty() && self.preview.files.is_empty() {
            lines.push(Line::from(Span::styled(
                "No changes to preview.",
                Style::default().fg(theme::MUTED),
            )));
        }

        // ── Hint line ──────────────────────────────────────────────────
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(
                " Enter/y: ",
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("confirm rewind", Style::default().fg(theme::WARNING)),
            Span::styled(
                "   Esc/n/q: ",
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("cancel", Style::default().fg(theme::TEXT)),
        ]));

        frame.render_widget(Paragraph::new(Text::from(lines)), inner);
    }

    fn desired_height(&self, screen_height: u16, _screen_width: u16) -> u16 {
        // header (1) + messages (n) + spacer (1 if messages) + files header (1)
        // + files (m) + spacer (1) + hint (2) + border (2)
        let messages_h = self.preview.messages.len() as u16;
        let files_h = self.preview.files.len() as u16;
        let mut content = 1u16; // hint spacer
        if messages_h > 0 {
            content += 1 + messages_h; // header + items
        }
        if files_h > 0 {
            if messages_h > 0 {
                content += 1; // separator
            }
            content += 1 + files_h; // header + items
        }
        if messages_h == 0 && files_h == 0 {
            content += 1; // empty state line
        }
        content += 2; // hint line + spacer
        content += 2; // border
        // Cap at 60% of screen to leave room for input + status bar.
        content.min(screen_height * 3 / 5).max(5)
    }

    fn handle_key(&mut self, key: KeyEvent) -> HandlerOutput {
        match key.code {
            // Enter, y, Y → confirm rewind
            KeyCode::Enter => HandlerOutput::Submit("confirmed".to_string()),
            KeyCode::Char('y' | 'Y') => HandlerOutput::Submit("confirmed".to_string()),
            // Esc, n, N, q, Q → dismiss
            KeyCode::Esc => HandlerOutput::Dismiss,
            KeyCode::Char('n' | 'N' | 'q' | 'Q') => HandlerOutput::Dismiss,
            _ => HandlerOutput::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::handler::{key, key_enter, key_esc};
    use super::*;
    use peri_acp_types::event_data::{FileChange, RewindMessage};

    fn make_preview() -> RewindPreview {
        RewindPreview {
            files: vec![],
            messages: vec![],
        }
    }

    fn make_populated_preview() -> RewindPreview {
        RewindPreview {
            files: vec![
                FileChange {
                    path: "src/foo.rs".into(),
                    change_type: "Edit".into(),
                    diff: None,
                },
                FileChange {
                    path: "src/bar.rs".into(),
                    change_type: "Write".into(),
                    diff: None,
                },
            ],
            messages: vec![RewindMessage {
                id: "m1".into(),
                role: "user".into(),
                preview: "hello world".into(),
            }],
        }
    }

    #[test]
    fn test_handler_stores_payload() {
        let h = RewindHandler::new(make_preview());
        assert!(h.preview.files.is_empty());
    }

    #[test]
    fn test_handle_key_enter_confirms() {
        let mut h = RewindHandler::new(make_preview());
        assert_eq!(
            h.handle_key(key_enter()),
            HandlerOutput::Submit("confirmed".to_string())
        );
    }

    #[test]
    fn test_handle_key_y_confirms() {
        let mut h = RewindHandler::new(make_preview());
        assert_eq!(
            h.handle_key(key('y')),
            HandlerOutput::Submit("confirmed".to_string())
        );
    }

    #[test]
    fn test_handle_key_esc_dismisses() {
        let mut h = RewindHandler::new(make_preview());
        assert_eq!(h.handle_key(key_esc()), HandlerOutput::Dismiss);
    }

    #[test]
    fn test_handle_key_n_dismisses() {
        let mut h = RewindHandler::new(make_preview());
        assert_eq!(h.handle_key(key('n')), HandlerOutput::Dismiss);
    }

    #[test]
    fn test_handle_key_other_is_nothing() {
        let mut h = RewindHandler::new(make_preview());
        assert_eq!(h.handle_key(key('x')), HandlerOutput::Nothing);
    }

    // ── Phase 1.4: render / desired_height ────────────────────────────

    #[test]
    fn test_desired_height_empty_preview_minimum() {
        // 空预览：应至少有最小高度（empty state + hint + border）。
        let h = RewindHandler::new(make_preview());
        let height = h.desired_height(50, 100);
        assert!(height >= 5, "empty preview should have minimum height");
    }

    #[test]
    fn test_desired_height_scales_with_content() {
        // 有 files + messages 时高度应大于空预览。
        let h_empty = RewindHandler::new(make_preview());
        let h_full = RewindHandler::new(make_populated_preview());
        let height_empty = h_empty.desired_height(50, 100);
        let height_full = h_full.desired_height(50, 100);
        assert!(
            height_full > height_empty,
            "populated preview should be taller than empty"
        );
    }

    #[test]
    fn test_desired_height_capped_at_60_percent() {
        // 即使内容很多，也不应超过屏幕的 60%。
        let big_preview = RewindPreview {
            files: (0..50)
                .map(|i| FileChange {
                    path: format!("file_{i}.rs"),
                    change_type: "Edit".into(),
                    diff: None,
                })
                .collect(),
            messages: (0..50)
                .map(|i| RewindMessage {
                    id: format!("m{i}"),
                    role: "user".into(),
                    preview: format!("msg {i}"),
                })
                .collect(),
        };
        let h = RewindHandler::new(big_preview);
        let height = h.desired_height(40, 100);
        // 60% of 40 = 24
        assert!(
            height <= 24,
            "height {height} should be capped at 60% of screen (24)"
        );
    }

    #[test]
    fn test_render_does_not_panic() {
        // render 应在不 panic 的情况下绘制（验证 BorderedPanel + Paragraph 组合）。
        let h = RewindHandler::new(make_populated_preview());
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                h.render(f, Rect::new(0, 0, 80, 24));
            })
            .expect("render should succeed");
    }
}
