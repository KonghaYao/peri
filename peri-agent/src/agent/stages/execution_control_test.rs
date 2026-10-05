use super::*;
use crate::agent::react::{ReactLLM, Reasoning, StreamingContext};
use crate::messages::BaseMessage;
use crate::session::test_resources::mock::MockSessionResources;
use crate::session::{FrozenContext, MessageSource, MessageTranscript, QueuedMessage, Session};
use crate::tools::BaseTool;
use peri_acp_types::session_resources::SessionResources;
use std::sync::Arc;

struct ControlledModel {
    resources: Arc<MockSessionResources>,
    session_id: String,
    resume: bool,
}

#[async_trait::async_trait]
impl ReactLLM for ControlledModel {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        for (command_id, action) in [
            ("pause-model", ControlAction::Pause),
            ("resume-model", ControlAction::Resume),
        ] {
            if command_id == "resume-model" && !self.resume {
                break;
            }
            let state = self
                .resources
                .load_session_control(&self.session_id)
                .await
                .map_err(anyhow::Error::new)?;
            self.resources
                .apply_session_control(&ControlCommand {
                    session_id: self.session_id.clone(),
                    command_id: command_id.into(),
                    expected_lifecycle: state.lifecycle,
                    expected_revision: state.revision,
                    expected_control_generation: state.control_generation,
                    action,
                })
                .await
                .map_err(anyhow::Error::new)?;
        }
        Ok(Reasoning::with_answer("", "STALE_RESPONSE_MUST_NOT_COMMIT"))
    }
    fn provider_capabilities(&self) -> crate::agent::compact_v2::projection::ProviderCapabilities {
        Default::default()
    }
}

#[tokio::test]
async fn pause_or_later_resume_cannot_commit_the_old_models_response() {
    for resume in [false, true] {
        let resources = MockSessionResources::new();
        let session_id = "controlled-session".to_owned();
        resources.register_bound_session(&session_id, "/tmp/controlled-session");
        let session = Session::new(
            Arc::from("/tmp/controlled-session"),
            FrozenContext::builder().build(),
            None,
        );
        let mut context = StageContext::builder(
            session.start_turn(),
            session.transcript(),
            session.queue().clone(),
        )
        .build();
        *context.session.transcript.write() =
            MessageTranscript::new().with_persistence(resources.clone(), session_id.clone());
        context.session.queue.push(QueuedMessage::prompt(
            MessageSource::UserInput,
            BaseMessage::human("work"),
        ));
        context.runtime.llm = Arc::new(ControlledModel {
            resources: resources.clone(),
            session_id: session_id.clone(),
            resume,
        });
        assert!(matches!(
            run_react_loop(context.clone(), 4).await,
            LoopResult::Interrupted
        ));
        assert_eq!(context.session.transcript.read().entries().len(), 1);
        let state = resources.load_session_control(&session_id).await.unwrap();
        assert!(state.attempt.is_none());
        assert_eq!(
            state.status,
            if resume {
                ControlStatus::Active
            } else {
                ControlStatus::Paused
            }
        );
    }
}
