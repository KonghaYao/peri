//! AskUser multi-question form handler.
//!
//! Wraps a [`peri_acp_types::event_data::AskUser`] payload. The P2 stub
//! implements [`crate::state_machine::state::Handler`] with no real key
//! dispatch -- the actual question-form UI logic lands in P3.

use peri_acp_types::event_data::AskUser;

use super::super::state::{Handler, HandlerOutput};

/// Handler for an `"ask-user"` event. Holds the question form payload.
#[derive(Debug)]
pub struct AskUserHandler {
    /// The question form received from the ACP layer.
    pub form: AskUser,
}

impl AskUserHandler {
    /// Create a new handler from an ask-user payload.
    pub fn new(form: AskUser) -> Self {
        Self { form }
    }
}

impl Handler for AskUserHandler {
    fn render(&self, _area: (u16, u16)) {}

    fn handle_key(&mut self, _key: char) -> HandlerOutput {
        // P3 will dispatch Tab / arrow navigation + Enter submission.
        HandlerOutput::Nothing
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
                options: vec![QuestionOption {
                    label: "A".into(),
                    description: "first".into(),
                }],
                multi_select: false,
            }],
        }
    }

    #[test]
    fn test_handler_stores_payload() {
        let h = AskUserHandler::new(make_form());
        assert_eq!(h.form.questions.len(), 1);
    }

    #[test]
    fn test_handle_key_returns_nothing() {
        let mut h = AskUserHandler::new(make_form());
        assert_eq!(h.handle_key('a'), HandlerOutput::Nothing);
    }
}
