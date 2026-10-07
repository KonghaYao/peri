use super::*;
use peri_acp_types::session_resources::work::*;
use sha2::{Digest, Sha256};

pub(crate) async fn seed_saved_fixture_runtime(
    resources: Arc<dyn SessionResources>,
    child_id: &str,
    frozen: FrozenSnapshotBytes,
    initiator: &str,
) {
    let work = resources
        .load_session_work(&WorkQuery {
            session_id: child_id.into(),
            limit: 1,
        })
        .await
        .unwrap();
    if work
        .state
        .child_resume_metadata
        .contains_key(&work.control.lifecycle)
    {
        return;
    }
    let saved = if let Some(previous) = work.state.child_resume_metadata.values().last() {
        let mut saved: crate::session::subagent::ChildResumeMetadata =
            serde_json::from_str(previous).unwrap();
        saved.recipient_lifecycle = work.control.lifecycle;
        saved
    } else {
        let intent =
            super::work::bind_fixture_task(resources.clone(), initiator, 1, child_id).await;
        crate::session::subagent::ChildResumeMetadata {
            version: 1,
            child_session_id: child_id.into(),
            recipient_lifecycle: work.control.lifecycle,
            agent_name: "fixture-child".into(),
            model_name: "fixture-scripted".into(),
            direct_initiator_session_id: initiator.into(),
            direct_initiator_lifecycle: 1,
            delegation_invocation_id: intent.invocation_id,
            delegation_task_id: child_id.into(),
            authorization_ref: intent.authorization_ref,
            frozen_digest: format!("{:x}", Sha256::digest(frozen.as_str().as_bytes())),
            tool_ceiling: Default::default(),
            tool_origins: Default::default(),
            skill_names: Vec::new(),
            max_iterations: 200,
            persona: None,
            system_prompt: String::new(),
            claude_md: "frozen-claude".into(),
            claude_local_md: None,
            skill_summary: "frozen-skills".into(),
            date: "2026-08-05".into(),
            language: None,
            section_overrides: Default::default(),
            disabled_middlewares: Default::default(),
            built_in_subagents_enabled: true,
        }
    };
    let work = resources
        .load_session_work(&WorkQuery {
            session_id: child_id.into(),
            limit: 1,
        })
        .await
        .unwrap();
    let receipt = resources
        .apply_work_mutation(
            &peri_acp_types::session_resources::work::PreparedWorkCommand::try_new(WorkCommand {
                session_id: child_id.into(),
                recipient_lifecycle: work.control.lifecycle,
                mutation_id: format!(
                    "fixture-saved-runtime:{child_id}:{}",
                    work.control.lifecycle
                ),
                action: WorkAction::BindChildResumeMetadata {
                    expected_revision: work.state.revision,
                    metadata_json: serde_json::to_string(&saved).unwrap(),
                },
            })
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
}

impl MockSessionResources {
    pub(crate) async fn initialize_historical_fixture(&self, child_id: &str) {
        self.durable_backend(child_id).await;
        let backend = self.work_backend.get().unwrap();
        let workspace = backend
            .resources
            .resolve_workspace(backend.repository.path())
            .await
            .unwrap();
        let bytes = FrozenSnapshotBytes::new(fixture_frozen());
        let current = backend
            .resources
            .load_session_snapshot(&child_id.to_owned())
            .await
            .unwrap();
        if !matches!(current.frozen, FrozenState::Present(_)) {
            backend
                .resources
                .adopt_legacy_session(&child_id.to_owned(), &current.meta.cwd, &workspace, &bytes)
                .await
                .unwrap();
        }
        self.with_region(&child_id.to_owned(), |region| {
            region.frozen = Some(bytes.as_str().into())
        });
        let initiator = self
            .region(&child_id.to_owned())
            .and_then(|region| region.meta)
            .and_then(|meta| meta.parent_thread_id)
            .unwrap_or_else(|| format!("fixture-original-parent:{child_id}"));
        self.durable_backend(&initiator).await;
        seed_saved_fixture_runtime(backend.resources.clone(), child_id, bytes, &initiator).await;
    }
}
