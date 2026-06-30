//! AskUser multi-question form handler.
//!
//! Wraps a [`peri_acp_types::event_data::AskUser`] payload and implements
//! full key dispatch: Tab to cycle questions, j/k to navigate options,
//! Space to toggle multi-select, Enter to submit, Esc to dismiss.

use peri_acp_types::event_data::AskUser;

use super::super::state::{Handler, HandlerOutput};

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
}

impl Handler for AskUserHandler {
    fn render(&self, _area: (u16, u16)) {
        // P5: rendering uses legacy popup system
    }

    fn handle_key(&mut self, key: char) -> HandlerOutput {
        if self.form.questions.is_empty() {
            return HandlerOutput::Dismiss;
        }
        let q = &self.form.questions[self.question_idx];

        match key {
            '\t' => {
                // Tab: cycle to next question
                self.question_idx = (self.question_idx + 1) % self.form.questions.len();
                HandlerOutput::Nothing
            }
            '\n' | '\r' => {
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
            'j' | 'J' => {
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
            'k' | 'K' => {
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
            ' ' => {
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
            '\x1b' => HandlerOutput::Dismiss, // Esc
            _ => HandlerOutput::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
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
        let output = h.handle_key('\n');
        assert!(matches!(output, HandlerOutput::Submit(_)));
        if let HandlerOutput::Submit(payload) = output {
            let v: serde_json::Value = serde_json::from_str(&payload).expect("valid JSON");
            assert!(v["answers"].is_array());
        }
    }

    #[test]
    fn test_handle_key_enter_no_selection_submits_empty() {
        let mut h = AskUserHandler::new(make_form());
        let output = h.handle_key('\n');
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
        h.handle_key('j');
        // verify B was selected
        assert_eq!(h.selections[0], vec![1]);
        let output = h.handle_key('\n');
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
        h.handle_key('\t');
        assert_eq!(h.question_idx, 1);
        // Tab wraps around
        h.handle_key('\t');
        assert_eq!(h.question_idx, 0);
    }

    #[test]
    fn test_handle_key_tab_empty_questions_noop() {
        let form = AskUser { questions: vec![] };
        let mut h = AskUserHandler::new(form);
        // Empty questions: handle_key immediately dismisses
        let output = h.handle_key('a');
        assert_eq!(output, HandlerOutput::Dismiss);
    }

    #[test]
    fn test_handle_key_j_moves_down() {
        let mut h = AskUserHandler::new(make_form());
        // Default selections[0] is empty; unwrap_or(0) gives 0. j moves to 1.
        let _ = h.handle_key('j');
        assert_eq!(h.selections[0], vec![1]);
    }

    #[test]
    fn test_handle_key_j_clamps_at_end() {
        let mut h = AskUserHandler::new(make_form());
        // Move to last option (index 1)
        h.handle_key('j');
        assert_eq!(h.selections[0], vec![1]);
        // j again should clamp
        h.handle_key('j');
        assert_eq!(h.selections[0], vec![1]);
    }

    #[test]
    fn test_handle_key_k_moves_up() {
        let mut h = AskUserHandler::new(make_form());
        // Set selection to 1 first
        h.selections[0] = vec![1];
        h.handle_key('k');
        assert_eq!(h.selections[0], vec![0]);
    }

    #[test]
    fn test_handle_key_k_clamps_at_start() {
        let mut h = AskUserHandler::new(make_form());
        // Default is 0; k should saturate to 0
        h.handle_key('k');
        assert_eq!(h.selections[0], vec![0]);
    }

    #[test]
    fn test_handle_key_space_toggles_multi_select() {
        let mut form = make_form();
        form.questions[0].multi_select = true;
        let mut h = AskUserHandler::new(form);
        // Space toggles option 0 on
        h.handle_key(' ');
        assert_eq!(h.selections[0], vec![0]);
        // Space toggles option 0 off
        h.handle_key(' ');
        assert!(h.selections[0].is_empty());
    }

    #[test]
    fn test_handle_key_space_noop_on_single_select() {
        let mut h = AskUserHandler::new(make_form());
        // make_form creates a single-select question; space should be a no-op
        h.handle_key(' ');
        assert!(h.selections[0].is_empty());
    }

    #[test]
    fn test_handle_key_j_multi_select_toggles() {
        let mut form = make_form();
        form.questions[0].multi_select = true;
        let mut h = AskUserHandler::new(form);
        // j moves from 0 to 1 and toggles
        h.handle_key('j');
        assert_eq!(h.selections[0], vec![1]);
        // j again removes 1 (since 1 is already selected, next=1, toggle off)
        h.handle_key('j');
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
        h.handle_key('k');
        assert_eq!(h.selections[0], vec![1, 0]);
    }

    #[test]
    fn test_handle_key_esc_dismisses() {
        let mut h = AskUserHandler::new(make_form());
        assert_eq!(h.handle_key('\x1b'), HandlerOutput::Dismiss);
    }

    #[test]
    fn test_handle_key_unknown_returns_nothing() {
        let mut h = AskUserHandler::new(make_form());
        assert_eq!(h.handle_key('x'), HandlerOutput::Nothing);
    }
}
