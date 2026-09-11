//! One owner for question focus, selections, and the active text editor.
//! Decisions return effects; channel publication and popup ownership stay in the panel.

use crate::components::textarea::TextAreaState;
use crate::kit::acp_types::PendingInteraction;
use crate::kit::list_nav::{
    ListNavAction, classify_list_nav, cycle_next, cycle_previous, next_selection,
    previous_selection,
};
use peri_acp_types::event_data::AskUser;
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
            || match classify_list_nav(key) {
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
                if self.focused_option == q.options.len() {
                    self.begin_custom_input(self.focused);
                } else if self.focused_option < q.options.len() {
                    self.toggle_option(self.focused, self.focused_option, q.multi_select);
                }
            }
            return FormOutcome::Consumed;
        }
        self.navigate(classify_list_nav(key), pending)
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
                    self.focused_option = previous_selection(self.focused_option);
                }
            }
            Some(ListNavAction::MoveDown) => {
                if !self.is_typing {
                    let limit = pending
                        .and_then(|p| p.questions.get(self.focused))
                        .map(|q| q.options.len() + 1)
                        .unwrap_or(0);
                    if limit > 0 {
                        self.focused_option = next_selection(self.focused_option, limit);
                        if self.focused_option == limit.saturating_sub(1) {
                            self.begin_custom_input(self.focused);
                        }
                    }
                }
            }
            Some(ListNavAction::CycleForward | ListNavAction::CycleBackward) if count > 0 => {
                if !self.is_typing {
                    self.focused = if action == Some(ListNavAction::CycleForward) {
                        cycle_next(self.focused, count)
                    } else {
                        cycle_previous(self.focused, count)
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
        let all_answered = self.answers.iter().enumerate().all(|(i, answer)| {
            !answer.is_empty()
                || self.custom_answers.get(i).is_some_and(Option::is_some)
                || pending
                    .and_then(|p| p.questions.get(i))
                    .map(|q| q.options.is_empty())
                    .unwrap_or(true)
        });
        if all_answered {
            return FormOutcome::Submit(build_answers_map(
                pending,
                &self.answers,
                &self.custom_answers,
            ));
        }
        let mut next = (self.focused + 1) % count;
        loop {
            let is_answered = self.answers.get(next).is_some_and(|v| !v.is_empty())
                || self.custom_answers.get(next).is_some_and(Option::is_some);
            let has_no_options = pending
                .and_then(|p| p.questions.get(next))
                .map(|q| q.options.is_empty())
                .unwrap_or(true);
            if !is_answered && !has_no_options {
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
    if let Some(au) = pending {
        for (i, q) in au.questions.iter().enumerate() {
            let custom = custom_answers.get(i).cloned().flatten();
            let selected: Vec<usize> = answers.get(i).cloned().unwrap_or_default();
            let val = if q.multi_select {
                // 多选：合并预设选项 labels + 自定义文本
                let mut labels: Vec<serde_json::Value> = selected
                    .iter()
                    .filter_map(|idx| q.options.get(*idx).map(|opt| json!(opt.label)))
                    .collect();
                if let Some(custom_text) = custom
                    && !custom_text.is_empty()
                {
                    labels.push(json!(custom_text));
                }
                if labels.is_empty() {
                    json!([])
                } else {
                    json!(labels)
                }
            } else if let Some(custom_text) = custom {
                // 单选：自定义文本优先
                json!(custom_text)
            } else {
                // 单选：仅预设选项
                selected
                    .first()
                    .and_then(|idx| q.options.get(*idx).map(|opt| json!(opt.label)))
                    .unwrap_or(json!(""))
            };
            map.insert(q.id.clone(), val);
        }
    }
    serde_json::Value::Object(map)
}
