//! HITL approval handler.
//!
//! Wraps a [`peri_acp_types::event_data::HitlPending`] payload and implements
//! interactive key dispatch for batch tool approvals:
//!   y/Enter = approve, n/Esc = dismiss, Tab = cycle between batch tools.
//!
//! Phase 1.4: implements `render` to draw a bordered popup listing the
//! pending tool calls (single or batch), with cursor highlight + input
//! preview + statistics summary + y/n hints. `desired_height` sizes the
//! popup to fit the batch items, capped to 60% of the screen.

use peri_acp_types::event_data::HitlPending;
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

/// Handler for a `"hitl-pending"` event. Holds the pending approval payload
/// and internal navigation state (selected batch index, approved flag).
#[derive(Debug)]
pub struct HitlHandler {
    /// The pending approval request received from the ACP layer.
    pub pending: HitlPending,
    /// Index of the currently selected tool in the batch (0 if no batch).
    selected: usize,
    /// Whether the user has confirmed approval.
    approved: bool,
}

impl HitlHandler {
    /// Create a new handler from a pending approval payload.
    pub fn new(pending: HitlPending) -> Self {
        Self {
            pending,
            selected: 0,
            approved: false,
        }
    }

    /// Total number of tools in the batch (1 for single, batch.len() for batch).
    fn total(&self) -> usize {
        self.pending
            .batch
            .as_ref()
            .map(|b| b.len())
            .unwrap_or(1)
            .max(1)
    }

    /// Render a single batch item line (cursor + tool_name + input preview).
    fn render_batch_item(
        &self,
        is_cursor: bool,
        tool_name: &str,
        input_preview: &str,
        max_width: usize,
    ) -> Line<'_> {
        let cursor_indicator = if is_cursor { "❯ " } else { "  " };
        let truncated_name = truncate_chars(tool_name, max_width.saturating_sub(8));
        let truncated_input = truncate_chars(input_preview, max_width.saturating_sub(8));
        Line::from(vec![
            Span::styled(
                cursor_indicator,
                if is_cursor {
                    Style::default()
                        .fg(theme::ACCENT)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            ),
            Span::styled(
                truncated_name,
                if is_cursor {
                    Style::default()
                        .fg(theme::TEXT)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::TEXT)
                },
            ),
            Span::styled(
                format!("  {}", truncated_input),
                Style::default().fg(theme::MUTED),
            ),
        ])
    }
}

/// Truncate a string to `max_len` characters, appending an ellipsis if truncated.
fn truncate_chars(s: &str, max_len: usize) -> String {
    if max_len <= 1 {
        return String::new();
    }
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max_len - 1).collect::<String>())
    }
}

/// Build a short "key=value" preview from a JSON tool input.
///
/// Picks the most informative key (command / file_path / pattern / path)
/// if available, falling back to the first key. Mirrors the v1 popup logic.
fn format_input_preview(input: &serde_json::Value, max_len: usize) -> String {
    let s = match input {
        serde_json::Value::Object(map) => {
            let key = ["command", "file_path", "pattern", "path"]
                .iter()
                .find(|k| map.contains_key(**k))
                .copied()
                .or_else(|| map.keys().next().map(|k| k.as_str()));

            if let Some(k) = key {
                if let Some(v) = map.get(k) {
                    let val = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    format!("{k}={val}")
                } else {
                    input.to_string()
                }
            } else {
                "{}".to_string()
            }
        }
        other => other.to_string(),
    };
    truncate_chars(&s, max_len)
}

impl Handler for HitlHandler {
    fn render(&self, frame: &mut ratatui::Frame, area: Rect) {
        let total = self.total();
        let title = if total == 1 {
            "Tool Approval"
        } else {
            "Batch Tool Approval"
        };

        let inner = BorderedPanel::new(Span::styled(
            title,
            Style::default()
                .fg(theme::WARNING)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme::WARNING))
        .render(frame, area);
        let max_width = inner.width as usize;

        let mut lines: Vec<Line> = Vec::new();

        // ── Single tool: show top-level tool_name + tool_input ────────
        // ── Batch: list each ToolApproval ─────────────────────────────
        if let Some(batch) = &self.pending.batch {
            for (i, item) in batch.iter().enumerate() {
                let is_cursor = i == self.selected;
                let preview = format_input_preview(
                    &serde_json::json!({ "input": item.input_summary }),
                    max_width.saturating_sub(6),
                );
                // For batch items, item.input_summary is already a string;
                // prefer it directly over the wrapped JSON.
                let display_preview = if item.input_summary.is_empty() {
                    preview
                } else {
                    truncate_chars(&item.input_summary, max_width.saturating_sub(6))
                };
                lines.push(self.render_batch_item(
                    is_cursor,
                    &item.tool_name,
                    &display_preview,
                    max_width,
                ));
            }
        } else {
            // Single tool: use top-level tool_name + tool_input preview.
            let preview = format_input_preview(&self.pending.tool_input, max_width);
            lines.push(self.render_batch_item(
                self.selected == 0,
                &self.pending.tool_name,
                &preview,
                max_width,
            ));
        }

        // ── Statistics summary for batch ──────────────────────────────
        if total > 1 {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("Batch: {} tools — Tab to cycle", total),
                Style::default().fg(theme::MUTED),
            )));
        }

        // ── Hint line ─────────────────────────────────────────────────
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(
                " y/Enter: ",
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("approve", Style::default().fg(theme::SAGE)),
            Span::styled(
                "   n/Esc: ",
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("reject", Style::default().fg(theme::ERROR)),
        ]));

        frame.render_widget(Paragraph::new(Text::from(lines)), inner);
    }

    fn desired_height(&self, screen_height: u16, _screen_width: u16) -> u16 {
        // Each item: 1 line. Batch summary: 2 lines (spacer + summary).
        // Hint: 2 lines (spacer + hint). Border: 2 lines.
        let items_h = self.total() as u16;
        let mut content = items_h;
        if self.total() > 1 {
            content += 2; // batch summary + spacer
        }
        content += 2; // hint + spacer
        content += 2; // border
        content.min(screen_height * 3 / 5).max(5)
    }

    fn handle_key(&mut self, key: KeyEvent) -> HandlerOutput {
        match key.code {
            KeyCode::Char('y' | 'Y') => {
                self.approved = true;
                HandlerOutput::Submit("approved".to_string())
            }
            KeyCode::Char('n' | 'N') => HandlerOutput::Dismiss,
            KeyCode::Tab => {
                let total = self.total();
                if total > 1 {
                    self.selected = (self.selected + 1) % total;
                }
                HandlerOutput::Nothing
            }
            KeyCode::Enter => {
                self.approved = true;
                HandlerOutput::Submit("approved".to_string())
            }
            _ => HandlerOutput::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::handler::{key, key_enter, key_tab};
    use super::*;
    use peri_acp_types::event_data::ToolApproval;

    fn make_pending() -> HitlPending {
        HitlPending {
            tool_name: "Edit".into(),
            tool_input: serde_json::json!({"path": "foo.rs"}),
            batch: None,
        }
    }

    fn make_batch_pending() -> HitlPending {
        HitlPending {
            tool_name: "Edit".into(),
            tool_input: serde_json::json!({"path": "foo.rs"}),
            batch: Some(vec![
                ToolApproval {
                    tool_id: "1".into(),
                    tool_name: "Edit".into(),
                    input_summary: "edit foo.rs".into(),
                },
                ToolApproval {
                    tool_id: "2".into(),
                    tool_name: "Write".into(),
                    input_summary: "write bar.rs".into(),
                },
            ]),
        }
    }

    #[test]
    fn test_handler_stores_payload() {
        let h = HitlHandler::new(make_pending());
        assert_eq!(h.pending.tool_name, "Edit");
    }

    #[test]
    fn test_handler_initial_state() {
        let h = HitlHandler::new(make_pending());
        assert_eq!(h.selected, 0);
        assert!(!h.approved);
        assert_eq!(h.total(), 1);
    }

    #[test]
    fn test_total_single_tool() {
        let h = HitlHandler::new(make_pending());
        assert_eq!(h.total(), 1);
    }

    #[test]
    fn test_total_batch() {
        let h = HitlHandler::new(make_batch_pending());
        assert_eq!(h.total(), 2);
    }

    #[test]
    fn test_handle_key_y_submits() {
        let mut h = HitlHandler::new(make_pending());
        assert_eq!(
            h.handle_key(key('y')),
            HandlerOutput::Submit("approved".to_string())
        );
        assert!(h.approved);
    }

    #[test]
    fn test_handle_key_uppercase_y_submits() {
        let mut h = HitlHandler::new(make_pending());
        assert_eq!(
            h.handle_key(key('Y')),
            HandlerOutput::Submit("approved".to_string())
        );
        assert!(h.approved);
    }

    #[test]
    fn test_handle_key_n_dismisses() {
        let mut h = HitlHandler::new(make_pending());
        assert_eq!(h.handle_key(key('n')), HandlerOutput::Dismiss);
    }

    #[test]
    fn test_handle_key_uppercase_n_dismisses() {
        let mut h = HitlHandler::new(make_pending());
        assert_eq!(h.handle_key(key('N')), HandlerOutput::Dismiss);
    }

    #[test]
    fn test_handle_key_enter_submits() {
        let mut h = HitlHandler::new(make_pending());
        assert_eq!(
            h.handle_key(key_enter()),
            HandlerOutput::Submit("approved".to_string())
        );
        assert!(h.approved);
    }

    #[test]
    fn test_handle_key_tab_single_tool_noop() {
        let mut h = HitlHandler::new(make_pending());
        assert_eq!(h.handle_key(key_tab()), HandlerOutput::Nothing);
        assert_eq!(h.selected, 0);
    }

    #[test]
    fn test_handle_key_tab_cycles_batch() {
        let mut h = HitlHandler::new(make_batch_pending());
        assert_eq!(h.handle_key(key_tab()), HandlerOutput::Nothing);
        assert_eq!(h.selected, 1);
        assert_eq!(h.handle_key(key_tab()), HandlerOutput::Nothing);
        assert_eq!(h.selected, 0);
        assert_eq!(h.handle_key(key_tab()), HandlerOutput::Nothing);
        assert_eq!(h.selected, 1);
    }

    #[test]
    fn test_handle_key_unknown_returns_nothing() {
        let mut h = HitlHandler::new(make_pending());
        assert_eq!(h.handle_key(key('x')), HandlerOutput::Nothing);
    }

    #[test]
    fn test_handle_key_does_not_approve_on_tab() {
        let mut h = HitlHandler::new(make_batch_pending());
        h.handle_key(key_tab());
        assert!(!h.approved);
    }

    // ── Phase 1.4: render / desired_height ────────────────────────────

    #[test]
    fn test_desired_height_single_tool_minimum() {
        // 单工具 HITL：至少 5 行（item + hint + spacer + border 等）。
        let h = HitlHandler::new(make_pending());
        let height = h.desired_height(50, 100);
        assert!(height >= 5, "single tool should have minimum height");
    }

    #[test]
    fn test_desired_height_batch_taller_than_single() {
        // 批量 HITL 应高于单工具（多 item 行 + 统计摘要）。
        let h_single = HitlHandler::new(make_pending());
        let h_batch = HitlHandler::new(make_batch_pending());
        let height_single = h_single.desired_height(50, 100);
        let height_batch = h_batch.desired_height(50, 100);
        assert!(
            height_batch > height_single,
            "batch ({height_batch}) should be taller than single ({height_single})"
        );
    }

    #[test]
    fn test_desired_height_capped_at_60_percent() {
        // 即使批量很大，也不超过屏幕 60%。
        let big_batch = HitlPending {
            tool_name: "Edit".into(),
            tool_input: serde_json::json!({}),
            batch: Some(
                (0..50)
                    .map(|i| ToolApproval {
                        tool_id: format!("t{i}"),
                        tool_name: "Edit".into(),
                        input_summary: format!("edit {i}"),
                    })
                    .collect(),
            ),
        };
        let h = HitlHandler::new(big_batch);
        let height = h.desired_height(40, 100);
        // 60% of 40 = 24
        assert!(
            height <= 24,
            "height {height} should be capped at 60% of screen (24)"
        );
    }

    #[test]
    fn test_render_single_tool_does_not_panic() {
        // 单工具 render 应在 TestBackend 上成功绘制。
        let h = HitlHandler::new(make_pending());
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                h.render(f, Rect::new(0, 0, 80, 24));
            })
            .expect("single-tool render should succeed");
    }

    #[test]
    fn test_render_batch_does_not_panic() {
        // 批量 render 应在 TestBackend 上成功绘制（多个 ToolApproval）。
        let h = HitlHandler::new(make_batch_pending());
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                h.render(f, Rect::new(0, 0, 80, 24));
            })
            .expect("batch render should succeed");
    }

    #[test]
    fn test_format_input_preview_picks_informative_key() {
        // format_input_preview 应优先选择 command/file_path/pattern/path。
        let preview = format_input_preview(&serde_json::json!({"path": "/tmp/foo.rs"}), 50);
        assert!(
            preview.contains("path=/tmp/foo.rs"),
            "preview should pick path key, got: {preview}"
        );

        let preview =
            format_input_preview(&serde_json::json!({"command": "ls -la", "cwd": "/tmp"}), 50);
        assert!(
            preview.contains("command=ls -la"),
            "preview should prefer command key, got: {preview}"
        );
    }

    #[test]
    fn test_truncate_chars_handles_cjk() {
        // 字符级截断应安全处理 CJK（按字符而非字节）。
        let s = "你好世界Hello";
        let truncated = truncate_chars(s, 5);
        assert_eq!(truncate_chars(s, 100), s, "no truncation when under limit");
        assert!(
            truncated.chars().count() <= 5,
            "truncated length should be at most 5 chars, got: {truncated}"
        );
        assert!(
            truncated.ends_with('…'),
            "truncated string should end with ellipsis, got: {truncated}"
        );
    }
}
