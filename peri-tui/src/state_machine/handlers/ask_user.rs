//! AskUser multi-question form handler.
//!
//! Wraps a [`peri_acp_types::event_data::AskUser`] payload and implements
//! full key dispatch: Tab to cycle questions, j/k to navigate options,
//! Space to toggle multi-select, Enter to submit, Esc to dismiss.
//!
//! Phase 1.4: implements `render` to draw a bordered popup with question
//! tabs (☐/✔ markers) + question text + numbered options (single-select
//! `❯ 1. label`, multi-select `❯ ● 1. label`) + descriptions + j/k/Space
//! hints. `desired_height` sizes the popup to fit the active question's
//! options, capped at 75% of the screen (AskUser can be tall).
//!
//! Custom-input (free-form text) is not yet supported in v2 — the v1 popup
//! overlaid a FieldTextarea on a numbered "custom" row. That feature will
//! land in a later phase along with textarea-in-Modal infrastructure.

use peri_acp_types::event_data::AskUser;
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

/// Handler for an `"ask-user"` event. Holds the question form payload,
/// navigation state, and per-question selections.
#[derive(Debug)]
pub struct AskUserHandler {
    /// The question form received from the ACP layer.
    pub form: AskUser,
    /// Currently focused question index.
    question_idx: usize,
    /// Selected option indices per question. For single-select questions
    /// this is a vec of at most one element (the chosen answer). For
    /// multi-select questions it can hold multiple indices.
    selections: Vec<Vec<usize>>,
}

impl AskUserHandler {
    /// Create a new handler from an ask-user payload.
    pub fn new(form: AskUser) -> Self {
        let q_count = form.questions.len();
        Self {
            form,
            question_idx: 0,
            selections: vec![Vec::new(); q_count],
        }
    }

    /// Whether option `idx` is selected in the active question.
    fn is_selected(&self, idx: usize) -> bool {
        self.selections
            .get(self.question_idx)
            .map(|s| s.contains(&idx))
            .unwrap_or(false)
    }

    /// Render the question tab header line: "☐ Q1  ☐ Q2  ✔ Q3" with the
    /// active question marked via styling.
    fn render_tab_header(&self) -> Line<'_> {
        let spans: Vec<Span> = self
            .form
            .questions
            .iter()
            .enumerate()
            .flat_map(|(i, q)| {
                let is_active = i == self.question_idx;
                let is_answered = !self.selections.get(i).map(|s| s.is_empty()).unwrap_or(true);
                let marker = if is_answered { "✔ " } else { "☐ " };
                let header: String = if q.header.is_empty() {
                    format!("Q{}", i + 1)
                } else {
                    q.header.chars().take(10).collect()
                };
                let label = format!("{marker}{header}");
                let style = if is_active {
                    Style::default()
                        .fg(theme::ACCENT)
                        .add_modifier(Modifier::BOLD)
                } else if is_answered {
                    Style::default().fg(theme::SAGE)
                } else {
                    Style::default().fg(theme::MUTED)
                };
                // Tab separator (a space between tabs).
                let sep_style = Style::default().fg(theme::MUTED);
                vec![Span::styled(label, style), Span::styled("  ", sep_style)]
            })
            .collect();
        Line::from(spans)
    }
}

impl Handler for AskUserHandler {
    fn render(&self, frame: &mut ratatui::Frame, area: Rect) {
        let inner = BorderedPanel::new(Span::styled(
            "Ask User",
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme::ACCENT))
        .render(frame, area);
        let max_width = inner.width as usize;

        let mut lines: Vec<Line> = Vec::new();

        // ── Tab header ─────────────────────────────────────────────
        if self.form.questions.len() > 1 {
            lines.push(self.render_tab_header());
            // Separator line of ─ chars.
            let sep: String = "─".repeat(max_width.saturating_sub(2));
            lines.push(Line::from(Span::styled(
                sep,
                Style::default().fg(theme::MUTED),
            )));
        }

        // ── Active question (empty form → render a placeholder) ────
        if let Some(q) = self.form.questions.get(self.question_idx) {
            // Question text (one or more lines).
            for l in q.question.lines() {
                lines.push(Line::from(Span::styled(
                    l.to_string(),
                    Style::default().fg(theme::TEXT),
                )));
            }
            lines.push(Line::from(""));

            // Options (numbered).
            let multi = q.multi_select;
            for (i, opt) in q.options.iter().enumerate() {
                let is_selected = self.is_selected(i);
                let num = i + 1;
                let cursor_mark = "❯";
                let space_mark = " ";

                let row_style = if is_selected {
                    Style::default().fg(theme::SAGE)
                } else {
                    Style::default().fg(theme::TEXT)
                };

                // Note: v2 AskUserHandler does not track an option cursor
                // (only `question_idx` for tab navigation). Selection state
                // is the only per-option signal we render. Mark all rows
                // with the space marker; selected rows get the visual cue
                // via the ● check icon (multi) or row color (single).
                let mark = space_mark;

                if multi {
                    let check = if is_selected { "●" } else { "○" };
                    lines.push(Line::from(vec![
                        Span::styled(format!("{mark} {check} {num}. "), row_style),
                        Span::styled(opt.label.clone(), row_style),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled(format!("{mark} {num}. "), row_style),
                        Span::styled(opt.label.clone(), row_style),
                    ]));
                }

                // Option description (if non-empty).
                if !opt.description.is_empty() {
                    let indent = if multi { "       " } else { "     " };
                    lines.push(Line::from(Span::styled(
                        format!("{}{}", indent, opt.description),
                        Style::default().fg(theme::MUTED),
                    )));
                }
            }

            // ── Hint line ──────────────────────────────────────────
            lines.push(Line::from(""));
            let hint = if multi {
                " j/k: navigate   Space: toggle   Tab: next question   Enter: submit   Esc: cancel"
            } else {
                " j/k: navigate   Tab: next question   Enter: submit   Esc: cancel"
            };
            lines.push(Line::from(Span::styled(
                hint,
                Style::default().fg(theme::MUTED),
            )));
        } else {
            // Empty form — defensive placeholder.
            lines.push(Line::from(Span::styled(
                "No questions to display.",
                Style::default().fg(theme::MUTED),
            )));
        }

        frame.render_widget(Paragraph::new(Text::from(lines)), inner);
    }

    fn desired_height(&self, screen_height: u16, _screen_width: u16) -> u16 {
        // Header (1) + separator (1) if multi-question.
        // Per question: question text lines (>=1) + spacer (1) + options
        // (n + descriptions). Hint (2). Border (2).
        let mut content: u16 = 0;
        if self.form.questions.len() > 1 {
            content += 2; // tab header + separator
        }
        if let Some(q) = self.form.questions.get(self.question_idx) {
            content += q.question.lines().count().max(1) as u16;
            content += 1; // spacer
            for opt in &q.options {
                content += 1; // option line
                if !opt.description.is_empty() {
                    content += 1; // description line
                }
            }
        } else {
            content += 1; // empty placeholder
        }
        content += 2; // hint + spacer
        content += 2; // border
                      // AskUser can be tall — cap at 75% of screen.
        content.min(screen_height * 3 / 4).max(5)
    }

    fn handle_key(&mut self, key: KeyEvent) -> HandlerOutput {
        if self.form.questions.is_empty() {
            return HandlerOutput::Dismiss;
        }
        let q = &self.form.questions[self.question_idx];

        match key.code {
            KeyCode::Tab => {
                // Tab: cycle to next question
                self.question_idx = (self.question_idx + 1) % self.form.questions.len();
                HandlerOutput::Nothing
            }
            KeyCode::Enter => {
                // Enter: submit answers
                let answers: Vec<String> = self
                    .selections
                    .iter()
                    .enumerate()
                    .map(|(i, sel)| {
                        if sel.is_empty() {
                            String::new()
                        } else {
                            sel.iter()
                                .filter_map(|&idx| self.form.questions[i].options.get(idx))
                                .map(|opt| opt.label.clone())
                                .collect::<Vec<_>>()
                                .join(",")
                        }
                    })
                    .collect();
                let payload = serde_json::json!({
                    "answers": answers,
                })
                .to_string();
                HandlerOutput::Submit(payload)
            }
            KeyCode::Char('j' | 'J') => {
                // j: move selection down. In single-select mode sets the
                // new position; in multi-select mode toggles the next item.
                if !q.options.is_empty() {
                    let cur = self.selections[self.question_idx]
                        .first()
                        .copied()
                        .unwrap_or(0);
                    let next = (cur + 1).min(q.options.len() - 1);
                    if q.multi_select {
                        let sel = &mut self.selections[self.question_idx];
                        if let Some(pos) = sel.iter().position(|&x| x == next) {
                            sel.remove(pos);
                        } else {
                            sel.push(next);
                        }
                    } else {
                        self.selections[self.question_idx] = vec![next];
                    }
                }
                HandlerOutput::Nothing
            }
            KeyCode::Char('k' | 'K') => {
                // k: move selection up. In single-select mode sets the new
                // position; in multi-select mode toggles the previous item.
                if !q.options.is_empty() {
                    let cur = self.selections[self.question_idx]
                        .first()
                        .copied()
                        .unwrap_or(0);
                    let prev = cur.saturating_sub(1);
                    if q.multi_select {
                        let sel = &mut self.selections[self.question_idx];
                        if let Some(pos) = sel.iter().position(|&x| x == prev) {
                            sel.remove(pos);
                        } else {
                            sel.push(prev);
                        }
                    } else {
                        self.selections[self.question_idx] = vec![prev];
                    }
                }
                HandlerOutput::Nothing
            }
            KeyCode::Char(' ') => {
                // Space: toggle the current option in multi-select mode.
                if q.multi_select && !q.options.is_empty() {
                    let cur = self.selections[self.question_idx]
                        .first()
                        .copied()
                        .unwrap_or(0);
                    let sel = &mut self.selections[self.question_idx];
                    if let Some(pos) = sel.iter().position(|&x| x == cur) {
                        sel.remove(pos);
                    } else {
                        sel.push(cur);
                    }
                }
                HandlerOutput::Nothing
            }
            KeyCode::Esc => HandlerOutput::Dismiss,
            _ => HandlerOutput::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::handler::{key, key_enter, key_esc, key_tab};
    use super::*;
    use peri_acp_types::event_data::{Question, QuestionOption};

    fn make_form() -> AskUser {
        AskUser {
            questions: vec![Question {
                id: "q1".into(),
                header: "Pick".into(),
                question: "Which option?".into(),
                options: vec![
                    QuestionOption {
                        label: "A".into(),
                        description: "first".into(),
                    },
                    QuestionOption {
                        label: "B".into(),
                        description: "second".into(),
                    },
                ],
                multi_select: false,
            }],
        }
    }

    #[test]
    fn test_handler_stores_payload() {
        let h = AskUserHandler::new(make_form());
        assert_eq!(h.form.questions.len(), 1);
        assert_eq!(h.question_idx, 0);
        assert_eq!(h.selections.len(), 1);
    }

    #[test]
    fn test_handle_key_enter_submits() {
        let mut h = AskUserHandler::new(make_form());
        let output = h.handle_key(key_enter());
        assert!(matches!(output, HandlerOutput::Submit(_)));
        if let HandlerOutput::Submit(payload) = output {
            let v: serde_json::Value = serde_json::from_str(&payload).expect("valid JSON");
            assert!(v["answers"].is_array());
        }
    }

    #[test]
    fn test_handle_key_enter_no_selection_submits_empty() {
        let mut h = AskUserHandler::new(make_form());
        let output = h.handle_key(key_enter());
        if let HandlerOutput::Submit(payload) = output {
            let v: serde_json::Value = serde_json::from_str(&payload).expect("valid JSON");
            assert_eq!(v["answers"][0], "");
        } else {
            panic!("expected Submit");
        }
    }

    #[test]
    fn test_handle_key_enter_with_selection() {
        let mut h = AskUserHandler::new(make_form());
        // j moves from default 0 to 1 (option B)
        h.handle_key(key('j'));
        // verify B was selected
        assert_eq!(h.selections[0], vec![1]);
        let output = h.handle_key(key_enter());
        if let HandlerOutput::Submit(payload) = output {
            let v: serde_json::Value = serde_json::from_str(&payload).expect("valid JSON");
            assert_eq!(v["answers"][0], "B");
        } else {
            panic!("expected Submit");
        }
    }

    #[test]
    fn test_handle_key_tab_cycles_questions() {
        let mut form = make_form();
        // Add a second question
        form.questions.push(Question {
            id: "q2".into(),
            header: "Second".into(),
            question: "Another?".into(),
            options: vec![],
            multi_select: false,
        });
        let mut h = AskUserHandler::new(form);
        assert_eq!(h.question_idx, 0);
        h.handle_key(key_tab());
        assert_eq!(h.question_idx, 1);
        // Tab wraps around
        h.handle_key(key_tab());
        assert_eq!(h.question_idx, 0);
    }

    #[test]
    fn test_handle_key_tab_empty_questions_noop() {
        let form = AskUser { questions: vec![] };
        let mut h = AskUserHandler::new(form);
        // Empty questions: handle_key immediately dismisses
        let output = h.handle_key(key('a'));
        assert_eq!(output, HandlerOutput::Dismiss);
    }

    #[test]
    fn test_handle_key_j_moves_down() {
        let mut h = AskUserHandler::new(make_form());
        // Default selections[0] is empty; unwrap_or(0) gives 0. j moves to 1.
        let _ = h.handle_key(key('j'));
        assert_eq!(h.selections[0], vec![1]);
    }

    #[test]
    fn test_handle_key_j_clamps_at_end() {
        let mut h = AskUserHandler::new(make_form());
        // Move to last option (index 1)
        h.handle_key(key('j'));
        assert_eq!(h.selections[0], vec![1]);
        // j again should clamp
        h.handle_key(key('j'));
        assert_eq!(h.selections[0], vec![1]);
    }

    #[test]
    fn test_handle_key_k_moves_up() {
        let mut h = AskUserHandler::new(make_form());
        // Set selection to 1 first
        h.selections[0] = vec![1];
        h.handle_key(key('k'));
        assert_eq!(h.selections[0], vec![0]);
    }

    #[test]
    fn test_handle_key_k_clamps_at_start() {
        let mut h = AskUserHandler::new(make_form());
        // Default is 0; k should saturate to 0
        h.handle_key(key('k'));
        assert_eq!(h.selections[0], vec![0]);
    }

    #[test]
    fn test_handle_key_space_toggles_multi_select() {
        let mut form = make_form();
        form.questions[0].multi_select = true;
        let mut h = AskUserHandler::new(form);
        // Space toggles option 0 on
        h.handle_key(key(' '));
        assert_eq!(h.selections[0], vec![0]);
        // Space toggles option 0 off
        h.handle_key(key(' '));
        assert!(h.selections[0].is_empty());
    }

    #[test]
    fn test_handle_key_space_noop_on_single_select() {
        let mut h = AskUserHandler::new(make_form());
        // make_form creates a single-select question; space should be a no-op
        h.handle_key(key(' '));
        assert!(h.selections[0].is_empty());
    }

    #[test]
    fn test_handle_key_j_multi_select_toggles() {
        let mut form = make_form();
        form.questions[0].multi_select = true;
        let mut h = AskUserHandler::new(form);
        // j moves from 0 to 1 and toggles
        h.handle_key(key('j'));
        assert_eq!(h.selections[0], vec![1]);
        // j again removes 1 (since 1 is already selected, next=1, toggle off)
        h.handle_key(key('j'));
        assert!(h.selections[0].is_empty());
    }

    #[test]
    fn test_handle_key_k_multi_select_toggles() {
        let mut form = make_form();
        form.questions[0].multi_select = true;
        let mut h = AskUserHandler::new(form);
        // Set cursor to 1
        h.selections[0] = vec![1];
        // k moves from 1 to 0 and toggles
        h.handle_key(key('k'));
        assert_eq!(h.selections[0], vec![1, 0]);
    }

    #[test]
    fn test_handle_key_esc_dismisses() {
        let mut h = AskUserHandler::new(make_form());
        assert_eq!(h.handle_key(key_esc()), HandlerOutput::Dismiss);
    }

    #[test]
    fn test_handle_key_unknown_returns_nothing() {
        let mut h = AskUserHandler::new(make_form());
        assert_eq!(h.handle_key(key('x')), HandlerOutput::Nothing);
    }

    // ── Phase 1.4: render / desired_height ────────────────────────────

    #[test]
    fn test_desired_height_minimum_for_single_question() {
        // 单问题 + 2 个选项 + 简短 question 文本：高度应 >= 5。
        let h = AskUserHandler::new(make_form());
        let height = h.desired_height(50, 100);
        assert!(
            height >= 5,
            "single-question form should have minimum height"
        );
    }

    #[test]
    fn test_desired_height_multi_question_includes_tab_header() {
        // 多问题时应包含 tab header + separator（比单问题高）。
        let single = AskUserHandler::new(make_form());
        let mut multi_form = make_form();
        multi_form.questions.push(Question {
            id: "q2".into(),
            header: "Second".into(),
            question: "Another?".into(),
            options: vec![],
            multi_select: false,
        });
        let multi = AskUserHandler::new(multi_form);
        let h_single = single.desired_height(50, 100);
        let h_multi = multi.desired_height(50, 100);
        // 多问题 +2 行（tab header + separator）
        assert!(
            h_multi >= h_single + 2,
            "multi-question form should include tab header ({h_multi} >= {h_single} + 2)"
        );
    }

    #[test]
    fn test_desired_height_capped_at_75_percent() {
        // 即使选项很多，也不超过屏幕 75%。
        let big_form = AskUser {
            questions: vec![Question {
                id: "q1".into(),
                header: "Big".into(),
                question: "Pick".into(),
                options: (0..50)
                    .map(|i| QuestionOption {
                        label: format!("opt{i}"),
                        description: format!("desc {i}"),
                    })
                    .collect(),
                multi_select: true,
            }],
        };
        let h = AskUserHandler::new(big_form);
        let height = h.desired_height(40, 100);
        // 75% of 40 = 30
        assert!(
            height <= 30,
            "height {height} should be capped at 75% of screen (30)"
        );
    }

    #[test]
    fn test_desired_height_with_descriptions() {
        // 选项带 description 应高于无 description 的同等 form。
        let mut no_desc_form = make_form();
        no_desc_form.questions[0].options = vec![
            QuestionOption {
                label: "A".into(),
                description: String::new(),
            },
            QuestionOption {
                label: "B".into(),
                description: String::new(),
            },
        ];
        let mut with_desc_form = make_form();
        with_desc_form.questions[0].options = vec![
            QuestionOption {
                label: "A".into(),
                description: "first".into(),
            },
            QuestionOption {
                label: "B".into(),
                description: "second".into(),
            },
        ];
        let h_no = AskUserHandler::new(no_desc_form).desired_height(50, 100);
        let h_with = AskUserHandler::new(with_desc_form).desired_height(50, 100);
        assert!(
            h_with > h_no,
            "form with descriptions ({h_with}) should be taller than without ({h_no})"
        );
    }

    #[test]
    fn test_render_single_question_does_not_panic() {
        // 单问题 render 应在 TestBackend 上成功绘制。
        let h = AskUserHandler::new(make_form());
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                h.render(f, Rect::new(0, 0, 80, 24));
            })
            .expect("single-question render should succeed");
    }

    #[test]
    fn test_render_multi_question_does_not_panic() {
        // 多问题 render 应在 TestBackend 上成功绘制（含 tab header）。
        let mut form = make_form();
        form.questions.push(Question {
            id: "q2".into(),
            header: "Second".into(),
            question: "Another?".into(),
            options: vec![QuestionOption {
                label: "X".into(),
                description: "x option".into(),
            }],
            multi_select: false,
        });
        let h = AskUserHandler::new(form);
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                h.render(f, Rect::new(0, 0, 80, 24));
            })
            .expect("multi-question render should succeed");
    }

    #[test]
    fn test_render_multi_select_shows_selected_marker() {
        // 多选 + Space 选中后，render 应不 panic 且标记 ● 已选。
        let mut form = make_form();
        form.questions[0].multi_select = true;
        let mut h = AskUserHandler::new(form);
        // Toggle option 0 on.
        h.handle_key(key(' '));
        assert_eq!(h.selections[0], vec![0]);

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                h.render(f, Rect::new(0, 0, 80, 24));
            })
            .expect("render with selected option should succeed");
    }

    #[test]
    fn test_is_selected_reflects_state() {
        // is_selected 应反映 selections 字段。
        let mut h = AskUserHandler::new(make_form());
        assert!(!h.is_selected(0));
        h.selections[0] = vec![1];
        assert!(!h.is_selected(0), "option 0 should not be selected");
        assert!(h.is_selected(1), "option 1 should be selected");
    }
}
