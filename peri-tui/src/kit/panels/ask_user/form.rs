//! One owner for question focus, selections, and the active text editor.
//! Decisions return effects; channel publication and popup ownership stay in the panel.

use crate::kit::acp_types::PendingInteraction;
use peri_tui_components::list_nav::ListNavAction;
use peri_tui_components::textarea::TextAreaState;

use ratatui_kit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;

#[derive(Default)]
pub(super) struct FormState {
    pub focused: usize,
    pub answers: Vec<Vec<usize>>,
    pub focused_option: usize,
    pub is_typing: bool,
    pub typing_state: TextAreaState,
    pub custom_answers: Vec<Option<String>>,
    session_fingerprint: Vec<String>,
}

#[derive(Debug, PartialEq)]
pub(super) enum FormOutcome {
    Ignored,
    Consumed,
    Submit(serde_json::Value),
    RequestCancel,
}

impl FormState {
    pub fn reset_for_owner_change(
        &mut self,
        interaction: Option<&PendingInteraction<AskUser>>,
    ) -> bool {
        let fingerprint = interaction_fingerprint(interaction);
        if self.session_fingerprint == fingerprint {
            return false;
        }
        let count = interaction.map(|p| p.payload.questions.len()).unwrap_or(0);
        *self = Self {
            answers: vec![vec![]; count],
            custom_answers: vec![None; count],
            session_fingerprint: fingerprint,
            ..Self::default()
        };
        true
    }

    pub fn toggle_option(&mut self, question: usize, option: usize, multi_select: bool) {
        if question >= self.answers.len() {
            self.answers.resize(question + 1, vec![]);
        }
        let selected = &mut self.answers[question];
        if multi_select {
            if let Some(pos) = selected.iter().position(|&x| x == option) {
                selected.remove(pos);
            } else {
                selected.push(option);
            }
        } else {
            *selected = if selected.first() == Some(&option) {
                vec![]
            } else {
                vec![option]
            };
        }
    }

    pub fn begin_custom_input(&mut self, question: usize) {
        let existing = self
            .custom_answers
            .get(question)
            .cloned()
            .flatten()
            .unwrap_or_default();
        self.typing_state.replace_all_no_undo(existing);
        self.typing_state.clear_undo_history();
        self.is_typing = true;
    }

    /// Avoid taking a notifying write guard for unrelated navigation keys.
    pub fn accepts_key(&self, key: &KeyEvent, pending: Option<&AskUser>) -> bool {
        self.is_typing
            || (key.modifiers, key.code) == (KeyModifiers::NONE, KeyCode::Char(' '))
            || match peri_tui_components::list_nav::classify_list_nav(key) {
                Some(ListNavAction::CycleForward | ListNavAction::CycleBackward) => {
                    pending.is_some_and(|p| !p.questions.is_empty())
                }
                Some(_) => true,
                None => false,
            }
    }

    pub fn handle_key(
        &mut self,
        key: &KeyEvent,
        pending: Option<&AskUser>,
        wrap_width: usize,
    ) -> FormOutcome {
        if self.is_typing && self.handle_typing_key(key, pending, wrap_width) {
            return FormOutcome::Consumed;
        }
        if (key.modifiers, key.code) == (KeyModifiers::NONE, KeyCode::Char(' ')) {
            if let Some(q) = pending.and_then(|p| p.questions.get(self.focused)) {
                if self.focused_option == q.options.len() && q.allows_custom_text() {
                    self.begin_custom_input(self.focused);
                } else if self.focused_option < q.options.len() {
                    self.toggle_option(self.focused, self.focused_option, q.multi_select);
                }
            }
            return FormOutcome::Consumed;
        }
        self.navigate(
            peri_tui_components::list_nav::classify_list_nav(key),
            pending,
        )
    }

    fn navigate(
        &mut self,
        action: Option<ListNavAction>,
        pending: Option<&AskUser>,
    ) -> FormOutcome {
        let count = pending.map(|p| p.questions.len()).unwrap_or(0);
        match action {
            Some(ListNavAction::MoveUp) => {
                if !self.is_typing {
                    self.focused_option =
                        peri_tui_components::list_nav::previous_selection(self.focused_option);
                }
            }
            Some(ListNavAction::MoveDown) => {
                if !self.is_typing {
                    let limit = pending
                        .and_then(|p| p.questions.get(self.focused))
                        .map(|q| q.options.len() + usize::from(q.allows_custom_text()))
                        .unwrap_or(0);
                    if limit > 0 {
                        self.focused_option = peri_tui_components::list_nav::next_selection(
                            self.focused_option,
                            limit,
                        );
                        if self.focused_option == limit.saturating_sub(1)
                            && pending
                                .and_then(|p| p.questions.get(self.focused))
                                .is_some_and(Question::allows_custom_text)
                        {
                            self.begin_custom_input(self.focused);
                        }
                    }
                }
            }
            Some(ListNavAction::CycleForward | ListNavAction::CycleBackward) if count > 0 => {
                if !self.is_typing {
                    self.focused = if action == Some(ListNavAction::CycleForward) {
                        peri_tui_components::list_nav::cycle_next(self.focused, count)
                    } else {
                        peri_tui_components::list_nav::cycle_previous(self.focused, count)
                    };
                    self.focused_option = self
                        .answers
                        .get(self.focused)
                        .and_then(|v| v.first().copied())
                        .unwrap_or(0);
                }
            }
            Some(ListNavAction::Confirm) => return self.confirm(pending, count),
            Some(ListNavAction::Cancel) => return FormOutcome::RequestCancel,
            _ => return FormOutcome::Ignored,
        }
        FormOutcome::Consumed
    }

    fn confirm(&mut self, pending: Option<&AskUser>, count: usize) -> FormOutcome {
        let all_answered = (0..count).all(|i| {
            let answer = self.answers.get(i).cloned().unwrap_or_default();
            let custom = self.custom_answers.get(i).cloned().flatten();
            let Some(q) = pending.and_then(|p| p.questions.get(i)) else {
                return true;
            };
            if q.field_type.is_some() {
                // MCP 字段只按 schema 的 required 判定；布尔/枚举不要求自定义文本。
                !q.required
                    || !answer.is_empty()
                    || custom.is_some_and(|text| !text.trim().is_empty())
            } else {
                !answer.is_empty() || custom.is_some() || q.options.is_empty()
            }
        });
        if all_answered {
            return FormOutcome::Submit(build_answers_map(
                pending,
                &self.answers,
                &self.custom_answers,
            ));
        }
        if count == 0 {
            return FormOutcome::Submit(build_answers_map(
                pending,
                &self.answers,
                &self.custom_answers,
            ));
        }
        let mut next = (self.focused + 1) % count;
        loop {
            let is_answered = self.answers.get(next).is_some_and(|v| !v.is_empty())
                || self
                    .custom_answers
                    .get(next)
                    .and_then(Clone::clone)
                    .is_some_and(|text| !text.trim().is_empty());
            let needs_answer = pending
                .and_then(|p| p.questions.get(next))
                .map(|q| {
                    if q.field_type.is_some() {
                        q.required
                    } else {
                        !q.options.is_empty()
                    }
                })
                .unwrap_or(false);
            if !is_answered && needs_answer {
                break;
            }
            next = (next + 1) % count;
            if next == self.focused {
                break;
            }
        }
        self.focused = next;
        self.focused_option = 0;
        FormOutcome::Consumed
    }
}

fn interaction_fingerprint(interaction: Option<&PendingInteraction<AskUser>>) -> Vec<String> {
    interaction
        .map(|interaction| {
            let owner = &interaction.owner;
            let mut fingerprint = vec![
                owner.client_instance_id.to_string(),
                owner.generation.to_string(),
                owner.prompt_epoch.to_string(),
                owner.token.to_string(),
            ];
            fingerprint.extend(interaction.payload.questions.iter().map(|q| q.id.clone()));
            fingerprint
        })
        .unwrap_or_default()
}

pub(super) fn build_answers_map(
    pending: Option<&AskUser>,
    answers: &[Vec<usize>],
    custom_answers: &[Option<String>],
) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    let Some(au) = pending else {
        return serde_json::Value::Object(map);
    };
    for (i, q) in au.questions.iter().enumerate() {
        let custom = custom_answers.get(i).cloned().flatten();
        let selected: Vec<usize> = answers.get(i).cloned().unwrap_or_default();
        if let Some(value) = typed_question_value(q, &selected, custom.as_deref()) {
            map.insert(q.id.clone(), value);
        }
    }
    serde_json::Value::Object(map)
}

/// MCP 字段按 schema 类型回填；内置 AskUser 保持「label 即答案」的旧行为。
fn typed_question_value(
    q: &Question,
    selected: &[usize],
    custom: Option<&str>,
) -> Option<serde_json::Value> {
    let Some(field_type) = q.field_type.as_deref() else {
        // 内置 AskUser 保持旧语义：空字符串也是一次明确的自定义作答。
        return Some(legacy_question_value(q, selected, custom));
    };
    let custom = custom.filter(|text| !text.trim().is_empty());
    let selected_values: Vec<serde_json::Value> = selected
        .iter()
        .filter_map(|index| q.options.get(*index))
        .map(|option| {
            option
                .value
                .clone()
                .unwrap_or_else(|| serde_json::Value::String(option.label.clone()))
        })
        .collect();
    match field_type {
        "url" => None,
        "boolean" => selected_values.first().cloned(),
        "array" => Some(serde_json::Value::Array(selected_values)),
        "integer" => custom
            .and_then(|text| text.trim().parse::<i64>().ok())
            .map(|value| serde_json::Value::Number(value.into()))
            .or_else(|| selected_values.first().cloned()),
        "number" => custom
            .and_then(|text| text.trim().parse::<f64>().ok())
            .and_then(serde_json::Number::from_f64)
            .map(serde_json::Value::Number)
            .or_else(|| selected_values.first().cloned()),
        _ => custom
            .map(|text| serde_json::Value::String(text.to_string()))
            .or_else(|| selected_values.first().cloned()),
    }
}

fn legacy_question_value(
    q: &Question,
    selected: &[usize],
    custom: Option<&str>,
) -> serde_json::Value {
    if q.multi_select {
        let mut labels: Vec<serde_json::Value> = selected
            .iter()
            .filter_map(|index| q.options.get(*index).map(|option| json!(option.label)))
            .collect();
        if let Some(custom_text) = custom {
            labels.push(json!(custom_text));
        }
        json!(labels)
    } else if let Some(custom_text) = custom {
        json!(custom_text)
    } else {
        selected
            .first()
            .and_then(|index| q.options.get(*index).map(|option| json!(option.label)))
            .unwrap_or(json!(""))
    }
}

use peri_acp_types::event_data::{AskUser, Question};
