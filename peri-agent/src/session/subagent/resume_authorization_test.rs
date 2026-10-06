use super::super::*;
use crate::session::test_resources::mock::admission::FixtureAdmission;
use crate::session::test_resources::mock::model::PreparedFixtureLlm;
use crate::tools::{BaseTool, EffectiveToolError, EffectiveToolErrorCode, ToolContext};
use peri_acp_types::session_resources::{work::*, SessionResources};
use sha2::{Digest, Sha256};

struct CeilingTool;

#[async_trait::async_trait]
impl BaseTool for CeilingTool {
    fn name(&self) -> &str {
        "SavedCeilingTool"
    }
    fn description(&self) -> &str {
        "saved capability fixture"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _context: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok("unused".into())
    }
}

async fn authorization_fixture(
    tools: Vec<Arc<dyn BaseTool>>,
) -> (Arc<MockSessionResources>, Arc<Session>, String) {
    let store = MockSessionResources::new();
    store.register_bound_session("authorization-parent", "/tmp/authorization-fixture");
    let parent = Session::new(
        Arc::from("/tmp/authorization-fixture"),
        FrozenContext::builder().build(),
        Some("authorization-parent".into()),
    );
    parent.set_subagent_host(SubagentHost {
        execution_admission_port: Some(Arc::new(FixtureAdmission(store.clone()))),
        session_resources: Some(store.clone()),
        ..Default::default()
    });
    let config = SubagentSpawnConfig {
        agent_name: "authorization-child".into(),
        prompt: "initial task".into(),
        parent_messages: Vec::new(),
        cancel_policy: SubagentCancelPolicy::Independent,
        max_iterations: 7,
        fork_directive_kind: None,
        run_mode: SubagentRunMode::Sync,
        skill_names: Vec::new(),
        llm: Box::new(EchoLLM),
        chain_assembler: Arc::new(EmptyChainAssembler),
        tools,
        tool_filter: Arc::new(|_| true),
        system_prompt: None,
        tool_invocation_resolver: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        session_resources: Some(store.clone()),
        event_handler: None,
        bg_event_sender: None,
        task_manager: None,
        on_bg_complete: None,
        langfuse_bridge: None,
        on_subagent_start: None,
        on_subagent_stop: None,
        register_runtime: None,
        deregister_runtime: None,
        parent_agent_id: None,
        parent_invocation_id: Some("original-spawn".into()),
        cancel_token: None,
        cwd: None,
        parent_thread_id: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_date: None,
    };
    let child = AdmittedSessionFactory::spawn_subagent(Some(&parent), config)
        .await
        .unwrap();
    (store, parent, child.child_thread_id)
}

async fn prepare_resume_invocation(
    store: &MockSessionResources,
    invocation_id: &str,
    authorization_ref: &str,
    scope: &str,
) {
    prepare_resume_invocation_for(
        store,
        "authorization-parent",
        invocation_id,
        authorization_ref,
        scope,
    )
    .await;
}

async fn prepare_resume_invocation_for(
    store: &MockSessionResources,
    initiator: &str,
    invocation_id: &str,
    authorization_ref: &str,
    scope: &str,
) {
    let parent_id = initiator.to_owned();
    let snapshot = store
        .inspect_work(&WorkQuery::new(parent_id.clone(), WorkSelector::Head))
        .await
        .unwrap();
    let arguments = "{}".to_owned();
    let arguments_ref = crate::agent::stages::prepare_work_evidence(
        store,
        &parent_id,
        arguments.as_bytes().to_vec(),
    )
    .await
    .unwrap();
    let digest = format!("{:x}", Sha256::digest(arguments.as_bytes()));
    let receipt = store
        .apply_work_mutation(&WorkCommand {
            session_id: parent_id,
            recipient_lifecycle: snapshot.control.lifecycle,
            mutation_id: format!("authorization-resume:{invocation_id}"),
            action: WorkAction::PrepareInvocation {
                expected_revision: snapshot.head.change_seq,
                intent: InvocationIntent {
                    invocation_id: invocation_id.into(),
                    tool_call_id: invocation_id.into(),
                    tool_name: "Agent".into(),
                    arguments: arguments_ref.clone(),
                    arguments_digest: digest.clone(),
                    effective_tool_name: "Agent".into(),
                    effective_arguments: arguments_ref,
                    effective_arguments_digest: digest,
                    owner_identity: "fixture-agent-owner".into(),
                    scope_id: scope.into(),
                    scope_epoch: None,
                    authorization_ref: authorization_ref.into(),
                    recovery_locator: format!("fixture-delegation:{invocation_id}"),
                },
            },
        })
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
}

fn current_resume_config(
    store: Arc<MockSessionResources>,
    child_id: String,
    invocation_id: &str,
    tools: Vec<Arc<dyn BaseTool>>,
) -> SubagentResumeConfig {
    let mut config = resume_config(store, child_id);
    config.parent_invocation_id = Some(invocation_id.into());
    config.llm = Box::new(PreparedFixtureLlm::new(Box::new(EchoLLM), tools.clone()));
    config.tools = tools;
    config
}

async fn saved_metadata(store: &MockSessionResources, child_id: &str) -> String {
    crate::session::work_access::descriptor(store, child_id, 1)
        .await
        .unwrap()
        .child_resume_metadata_json
        .unwrap()
}

async fn save_authorization_sibling(store: &MockSessionResources, child_id: &str) -> String {
    let source = store
        .load_session_snapshot(&child_id.to_owned())
        .await
        .unwrap();
    let peri_acp_types::session_resources::BindingState::Bound(binding) = source.binding else {
        panic!("fixture child must be bound");
    };
    let peri_acp_types::session_resources::FrozenState::Present(frozen) = source.frozen else {
        panic!("fixture child must have frozen evidence");
    };
    let sibling_id = uuid::Uuid::now_v7().to_string();
    store
        .save_child(&ChildSnapshot {
            target: NewSession {
                thread_id: sibling_id.clone(),
                created_at: peri_time::now_utc_rfc3339(),
                meta: NewSessionMeta {
                    title: None,
                    cwd: source.meta.cwd,
                    parent_thread_id: Some("authorization-parent".into()),
                    hidden: true,
                    cancel_policy: source.meta.cancel_policy,
                    snapshot_at_message_id: None,
                },
                binding,
                frozen,
            },
            parent_id: "authorization-parent".into(),
            root_id: "authorization-parent".into(),
            inherited: Default::default(),
        })
        .await
        .unwrap();
    store
        .update_thread_status(&sibling_id, "done")
        .await
        .unwrap();
    sibling_id
}

#[tokio::test]
async fn test_resume_cross_admission_preserves_saved_authorization_ceiling() {
    let (store, parent, child_id) = authorization_fixture(Vec::new()).await;
    let saved = saved_metadata(&store, &child_id).await;
    let metadata: ChildResumeMetadata = serde_json::from_str(&saved).unwrap();
    for (invocation_id, authorization_ref) in [
        ("resume-admission-one", "admission-one-authorization"),
        ("resume-admission-two", "admission-two-authorization"),
    ] {
        assert_ne!(metadata.authorization_ref, authorization_ref);
        prepare_resume_invocation(
            &store,
            invocation_id,
            authorization_ref,
            "authorization-parent",
        )
        .await;
        let config =
            current_resume_config(store.clone(), child_id.clone(), invocation_id, Vec::new());
        let resumed = SessionFactory::resume_subagent(Some(&parent), config)
            .await
            .expect("new admission must not equal the original authorization ref");
        assert_eq!(resumed.child_thread_id, child_id);
        assert_eq!(saved_metadata(&store, &child_id).await, saved);
        let effect = crate::session::work_access::effect(
            store.as_ref(),
            "authorization-parent",
            invocation_id,
        )
        .await
        .unwrap();
        let binding = effect.binding.unwrap();
        assert_eq!(binding.invocation_id, invocation_id);
        assert_eq!(binding.authorization_ref, authorization_ref);
        assert_eq!(binding.initiator_session_id, "authorization-parent");
        assert_eq!(
            store.load_meta(&child_id).await.unwrap().agent_status,
            AgentStatus::Done
        );
    }
}

#[tokio::test]
async fn test_resume_current_invocation_foreign_scope_is_rejected() {
    let (store, parent, child_id) = authorization_fixture(Vec::new()).await;
    prepare_resume_invocation(&store, "foreign-scope", "new-authorization", "other-parent").await;
    let config =
        current_resume_config(store.clone(), child_id.clone(), "foreign-scope", Vec::new());
    let error = SessionFactory::resume_subagent(Some(&parent), config)
        .await
        .err()
        .expect("foreign scope must be denied");
    assert_eq!(
        error.downcast_ref::<EffectiveToolError>().unwrap().code,
        EffectiveToolErrorCode::PermissionDenied
    );
    assert_eq!(
        store.load_meta(&child_id).await.unwrap().agent_status,
        AgentStatus::Done
    );
    let effect = crate::session::work_access::effect(
        store.as_ref(),
        "authorization-parent",
        "foreign-scope",
    )
    .await
    .unwrap();
    assert!(effect.binding.is_none());
}

#[tokio::test]
async fn test_resume_same_root_sibling_uses_its_own_current_authorization() {
    let (store, _parent, child_id) = authorization_fixture(Vec::new()).await;
    let saved = saved_metadata(&store, &child_id).await;
    let sibling_id = save_authorization_sibling(&store, &child_id).await;
    let caller = Session::new(
        Arc::from("/tmp/authorization-fixture"),
        FrozenContext::builder().build(),
        Some(sibling_id.clone()),
    );
    caller.set_subagent_host(SubagentHost {
        execution_admission_port: Some(Arc::new(FixtureAdmission(store.clone()))),
        session_resources: Some(store.clone()),
        ..Default::default()
    });
    prepare_resume_invocation_for(
        &store,
        &sibling_id,
        "sibling-resume",
        "sibling-admission",
        &sibling_id,
    )
    .await;
    let config = current_resume_config(
        store.clone(),
        child_id.clone(),
        "sibling-resume",
        Vec::new(),
    );
    let resumed = SessionFactory::resume_subagent(Some(&caller), config)
        .await
        .unwrap();
    assert_eq!(resumed.child_thread_id, child_id);
    assert_eq!(saved_metadata(&store, &child_id).await, saved);
    let effect = crate::session::work_access::effect(store.as_ref(), &sibling_id, "sibling-resume")
        .await
        .unwrap();
    let binding = effect.binding.unwrap();
    assert_eq!(binding.invocation_id, "sibling-resume");
    assert_eq!(binding.initiator_session_id, sibling_id);
    assert_eq!(binding.authorization_ref, "sibling-admission");
    assert_eq!(
        store.load_meta(&child_id).await.unwrap().agent_status,
        AgentStatus::Done
    );
}

#[tokio::test]
async fn test_resume_saved_ceiling_requires_original_authorization_evidence() {
    let (store, parent, original_child_id) = authorization_fixture(Vec::new()).await;
    let original_metadata: ChildResumeMetadata =
        serde_json::from_str(&saved_metadata(&store, &original_child_id).await).unwrap();
    prepare_resume_invocation(
        &store,
        "current-valid",
        "new-authorization",
        "authorization-parent",
    )
    .await;
    for corruption in ["authorization", "invocation", "lifecycle", "parent"] {
        let child_id = save_authorization_sibling(&store, &original_child_id).await;
        let mut metadata = original_metadata.clone();
        metadata.child_session_id = child_id.clone();
        match corruption {
            "authorization" => metadata.authorization_ref = "forged-ceiling".into(),
            "invocation" => metadata.delegation_invocation_id = "missing-original".into(),
            "lifecycle" => metadata.direct_initiator_lifecycle += 1,
            "parent" => metadata.direct_initiator_session_id = "other-parent".into(),
            _ => unreachable!(),
        }
        let work = store
            .inspect_work(&WorkQuery::new(child_id.clone(), WorkSelector::Head))
            .await
            .unwrap();
        let receipt = store
            .apply_work_mutation(&WorkCommand {
                session_id: child_id.clone(),
                recipient_lifecycle: work.control.lifecycle,
                mutation_id: format!("corrupt-metadata:{corruption}"),
                action: WorkAction::BindChildResumeMetadata {
                    expected_revision: work.head.change_seq,
                    metadata_json: serde_json::to_string(&metadata).unwrap(),
                },
            })
            .await
            .unwrap();
        assert_eq!(receipt.decision, WorkDecision::Accepted);
        let config =
            current_resume_config(store.clone(), child_id.clone(), "current-valid", Vec::new());
        let error = SessionFactory::resume_subagent(Some(&parent), config)
            .await
            .err()
            .unwrap();
        assert_eq!(
            error.downcast_ref::<EffectiveToolError>().unwrap().code,
            EffectiveToolErrorCode::PermissionDenied
        );
        assert_eq!(
            store.load_meta(&child_id).await.unwrap().agent_status,
            AgentStatus::Done
        );
    }
}

#[tokio::test]
async fn test_resume_other_root_parent_is_rejected_before_claim() {
    let (store, _parent, child_id) = authorization_fixture(Vec::new()).await;
    store.register_bound_session("other-root", "/tmp/authorization-fixture");
    let caller = Session::new(
        Arc::from("/tmp/authorization-fixture"),
        FrozenContext::builder().build(),
        Some("other-root".into()),
    );
    let config = current_resume_config(store.clone(), child_id.clone(), "unused", Vec::new());
    let before = store.statuses().len();
    let error = SessionFactory::resume_subagent(Some(&caller), config)
        .await
        .err()
        .unwrap();
    assert_eq!(
        error.downcast_ref::<EffectiveToolError>().unwrap().code,
        EffectiveToolErrorCode::PermissionDenied
    );
    assert_eq!(store.statuses().len(), before);
    assert_eq!(
        store.load_meta(&child_id).await.unwrap().agent_status,
        AgentStatus::Done
    );
}

#[tokio::test]
async fn test_resume_new_admission_cannot_bypass_saved_tool_ceiling() {
    let (store, parent, child_id) = authorization_fixture(vec![Arc::new(CeilingTool)]).await;
    prepare_resume_invocation(
        &store,
        "missing-capability",
        "new-authorization",
        "authorization-parent",
    )
    .await;
    let config = current_resume_config(
        store.clone(),
        child_id.clone(),
        "missing-capability",
        Vec::new(),
    );
    let error = SessionFactory::resume_subagent(Some(&parent), config)
        .await
        .err()
        .unwrap();
    assert!(error
        .to_string()
        .contains("saved child tool origin or ceiling unavailable"));
    assert_eq!(
        store.load_meta(&child_id).await.unwrap().agent_status,
        AgentStatus::Done
    );
}
