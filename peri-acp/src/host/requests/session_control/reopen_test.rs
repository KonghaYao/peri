use super::*;
use crate::host::requests::resource_owners;
use peri_acp_types::session_resources::work::PreparedWorkCommand;
use peri_acp_types::session_resources::work::{WorkAction, WorkCommand, WorkDecision, WorkQuery};
use peri_acp_types::session_resources::{ControlDecision, ControlResolution};

async fn work(
    cfg: &AcpServerConfig,
    id: &str,
) -> peri_acp_types::session_resources::work::WorkSnapshot {
    cfg.session_resources
        .load_session_work(&WorkQuery {
            session_id: id.into(),
            limit: 1,
        })
        .await
        .unwrap()
}

async fn bind_child(cfg: &AcpServerConfig, id: &str) -> String {
    let metadata = peri_agent::session::subagent::ChildResumeMetadata {
        version: 1,
        child_session_id: id.into(),
        recipient_lifecycle: 1,
        agent_name: "original-agent".into(),
        model_name: "original-model".into(),
        direct_initiator_session_id: "original-parent".into(),
        direct_initiator_lifecycle: 7,
        delegation_invocation_id: "original-invocation".into(),
        delegation_task_id: "original-task".into(),
        authorization_ref: "original-auth".into(),
        frozen_digest: "original-digest".into(),
        tool_ceiling: ["original-tool".into()].into(),
        tool_origins: Default::default(),
        skill_names: vec!["original-skill".into()],
        max_iterations: 13,
        persona: Some("original-persona".into()),
        system_prompt: "original-system".into(),
        claude_md: "original-instructions".into(),
        claude_local_md: None,
        skill_summary: "original-summary".into(),
        date: "2026-10-06".into(),
        language: Some("zh".into()),
        section_overrides: Default::default(),
        disabled_middlewares: Default::default(),
        built_in_subagents_enabled: false,
    };
    let metadata_json = serde_json::to_string(&metadata).unwrap();
    let snapshot = work(cfg, id).await;
    let receipt = cfg
        .session_resources
        .apply_work_mutation(
            &PreparedWorkCommand::try_new(WorkCommand {
                session_id: id.into(),
                recipient_lifecycle: 1,
                mutation_id: "original-child-metadata".into(),
                action: WorkAction::BindChildResumeMetadata {
                    expected_revision: snapshot.state.revision,
                    metadata_json: metadata_json.clone(),
                },
            })
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    metadata_json
}

/// [回归测试] Reopen must not reuse the closed manager or lose historical owner authorization.
#[tokio::test]
async fn reopen_copies_exact_owner_and_child_authority_and_replaces_lifecycle() {
    let tmp = tempfile::tempdir().unwrap();
    let (cfg, mut sessions, id) = fixture(&tmp).await;
    resource_owners::bind(&cfg, &id, &HashMap::new())
        .await
        .unwrap();
    let original_metadata = bind_child(&cfg, &id).await;
    let old_manager = cfg
        .session_manager
        .get_session(&id)
        .unwrap()
        .task_manager
        .clone();
    let old_inbox = cfg.session_manager.session_inbox_for(&id).unwrap();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let close = command(&cfg, &id, "close-original-life", ControlAction::Close).await;
    handle_request("session/control", &close, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    assert_eq!(work(&cfg, &id).await.control.status, ControlStatus::Closed);
    assert!(!sessions.contains_key(&id));
    let reopen = command(&cfg, &id, "reopen-new-life", ControlAction::Reopen).await;
    let receipt = handle_request("session/control", &reopen, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    let snapshot = work(&cfg, &id).await;
    assert_eq!(snapshot.control.lifecycle, 2);
    assert_eq!(
        cfg.session_manager
            .get_session(&id)
            .unwrap()
            .recipient_lifecycle,
        2
    );
    let mut expected_owner = snapshot.state.resource_owners[&1].clone();
    expected_owner.recipient_lifecycle = 2;
    assert_eq!(snapshot.state.resource_owners[&2], expected_owner);
    let mut expected_metadata: Value = serde_json::from_str(&original_metadata).unwrap();
    expected_metadata["recipientLifecycle"] = json!(2);
    assert_eq!(
        serde_json::from_str::<Value>(&snapshot.state.child_resume_metadata[&2]).unwrap(),
        expected_metadata
    );
    assert_eq!(
        serde_json::from_str::<Value>(&snapshot.state.child_resume_metadata[&1]).unwrap()
            ["recipientLifecycle"],
        1
    );
    assert!(!Arc::ptr_eq(
        &old_manager,
        &cfg.session_manager.get_session(&id).unwrap().task_manager
    ));
    assert!(!Arc::ptr_eq(
        &old_inbox,
        &cfg.session_manager.session_inbox_for(&id).unwrap()
    ));
    assert_eq!(sessions[&id].history.len(), 3);
    let manager = cfg
        .session_manager
        .get_session(&id)
        .unwrap()
        .task_manager
        .clone();
    assert_eq!(
        handle_request("session/control", &reopen, &cfg, &mut sessions, &transport)
            .await
            .unwrap(),
        receipt
    );
    assert!(Arc::ptr_eq(
        &manager,
        &cfg.session_manager.get_session(&id).unwrap().task_manager
    ));
}

/// [回归测试] Missing legacy owners permit history restoration only after durable quarantine.
#[tokio::test]
async fn legacy_restore_quarantines_canonical_ids_without_binding_an_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let (cfg, sessions, id) = fixture(&tmp).await;
    assert!(resource_owners::load_for_restore(&cfg, &id)
        .await
        .unwrap()
        .is_empty());
    let snapshot = work(&cfg, &id).await;
    assert!(snapshot.state.resource_owners.is_empty());
    assert!(snapshot.blocked);
    let evidence: Value =
        serde_json::from_str(&snapshot.state.legacy_unknown[&format!("ownerMissing:{id}:1")])
            .unwrap();
    assert_eq!(evidence["kind"], "unknownBuiltin");
    assert_eq!(
        evidence["canonicalMessageIds"],
        json!(sessions[&id]
            .history_payloads
            .iter()
            .map(|payload| payload.id().as_uuid().to_string())
            .collect::<Vec<_>>())
    );
    assert!(resource_owners::load(&cfg, &id).await.is_err());
    let revision = snapshot.state.revision;
    resource_owners::load_for_restore(&cfg, &id).await.unwrap();
    assert_eq!(work(&cfg, &id).await.state.revision, revision);
}

/// [回归测试] The new fork ID must carry a trusted owner declaration before publication.
#[tokio::test]
async fn fork_persists_trusted_owners_under_the_new_id() {
    let tmp = tempfile::tempdir().unwrap();
    let (cfg, mut sessions, id) = fixture(&tmp).await;
    sessions.get_mut(&id).unwrap().frozen = Some(
        resource_owners::load_frozen_for_environment(&cfg, &id)
            .await
            .unwrap(),
    );
    resource_owners::bind(&cfg, &id, &HashMap::new())
        .await
        .unwrap();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let response = handle_request(
        "session/fork",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let new_id = response["sessionId"].as_str().unwrap();
    assert_ne!(new_id, id);
    assert!(sessions.contains_key(new_id));
    assert_eq!(
        work(&cfg, new_id).await.state.resource_owners[&1].authorization_ref,
        format!("trusted-session-setup:{new_id}")
    );
    assert!(resource_owners::load(&cfg, new_id)
        .await
        .unwrap()
        .is_empty());
}

/// [回归测试] A closed legacy session cannot acquire current-root owners through Reopen.
#[tokio::test]
async fn reopen_without_previous_owner_is_durably_blocked_without_publication() {
    let tmp = tempfile::tempdir().unwrap();
    let (cfg, mut sessions, id) = fixture(&tmp).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let close = command(&cfg, &id, "close-legacy", ControlAction::Close).await;
    handle_request("session/control", &close, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    let reopen = command(&cfg, &id, "reopen-legacy", ControlAction::Reopen).await;
    let error = handle_request("session/control", &reopen, &cfg, &mut sessions, &transport)
        .await
        .unwrap_err();
    assert!(error
        .message
        .contains("previous persistent resource owners are missing"));
    let snapshot = work(&cfg, &id).await;
    assert!(snapshot.blocked);
    assert!(snapshot.state.resource_owners.is_empty());
    assert!(snapshot
        .state
        .legacy_unknown
        .contains_key(&format!("ownerMissing:{id}:2")));
    assert!(!sessions.contains_key(&id));
    assert!(cfg.session_manager.get_session(&id).is_none());
}

/// [回归测试] Cold builtin restoration must record unknown outcomes, never claim original ownership.
#[tokio::test]
async fn cold_builtin_owner_is_honestly_quarantined_instead_of_restored() {
    let tmp = tempfile::tempdir().unwrap();
    let (cfg, _sessions, id) = fixture(&tmp).await;
    resource_owners::bind(&cfg, &id, &HashMap::new())
        .await
        .unwrap();
    let error = match resource_owners::cold_environment(&cfg, &id).await {
        Err(error) => error,
        Ok(_) => panic!("unknown builtin owner must not be restored"),
    };
    assert!(error.message.contains("unknownBuiltin"));
    assert!(error.message.contains("Blocked"));
    let snapshot = work(&cfg, &id).await;
    assert!(snapshot.blocked);
    assert!(snapshot
        .state
        .legacy_unknown
        .contains_key(&format!("unknownBuiltin:{id}:1")));
}

/// [回归测试] Settled builtin owners allow a new lifecycle, not reuse of the closed environment.
#[tokio::test]
async fn settled_builtin_reopen_creates_a_fresh_environment_and_empty_queue() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut cfg, mut sessions, _fixture_id) = fixture(&tmp).await;
    let cwd = tmp.path().canonicalize().unwrap();
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: cwd.to_str().unwrap().into(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let created = handle_request(
        "session/new",
        &json!({"cwd":cwd.to_str().unwrap()}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let id = created["sessionId"].as_str().unwrap().to_owned();
    append_human_message(&cfg, &id, "persistent pre-close history").await;
    let old_environment = sessions[&id].environment.clone().unwrap();
    let old_manager = old_environment.task_manager();
    let old_queue = old_environment
        .cfg
        .session_manager
        .get_session(&id)
        .unwrap()
        .v2_message_queue
        .clone();
    old_queue.push(peri_acp_types::session::QueuedMessage::info(
        peri_acp_types::session::MessageSource::SystemInjected,
        peri_acp_types::messages::BaseMessage::ai("old-life transient notification"),
    ));
    let old_inbox = old_environment
        .cfg
        .session_manager
        .session_inbox_for(&id)
        .unwrap();
    let close = command(&cfg, &id, "close-builtin-life", ControlAction::Close).await;
    handle_request("session/control", &close, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    assert_eq!(work(&cfg, &id).await.control.status, ControlStatus::Closed);
    assert!(!sessions.contains_key(&id));
    let reopen = command(&cfg, &id, "reopen-builtin-life", ControlAction::Reopen).await;
    handle_request("session/control", &reopen, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    let new_environment = sessions[&id].environment.clone().unwrap();
    assert!(!Arc::ptr_eq(&old_environment, &new_environment));
    assert!(!Arc::ptr_eq(&old_manager, &new_environment.task_manager()));
    assert!(!Arc::ptr_eq(
        &old_inbox,
        &new_environment
            .cfg
            .session_manager
            .session_inbox_for(&id)
            .unwrap()
    ));
    let manager_session = new_environment
        .cfg
        .session_manager
        .get_session(&id)
        .unwrap();
    assert_eq!(manager_session.recipient_lifecycle, 2);
    assert!(!old_queue.is_empty());
    assert!(manager_session.v2_message_queue.is_empty());
    assert!(!manager_session.v2_message_queue.has_required());
    assert_eq!(sessions[&id].cwd, cwd.to_str().unwrap());
    assert_eq!(sessions[&id].history.len(), 1);
    let snapshot = work(&cfg, &id).await;
    assert!(!snapshot.blocked);
    assert_eq!(snapshot.state.resource_owners[&1].connections_json, "{}");
    assert_eq!(snapshot.state.resource_owners[&2].connections_json, "{}");
}

/// [回归测试] A cold-loaded Closed history view is not a live execution owner or a closing authority.
#[tokio::test]
async fn cold_load_closed_then_reopen_replaces_the_idle_view_environment() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut cfg, mut sessions, _fixture_id) = fixture(&tmp).await;
    let cwd = tmp.path().canonicalize().unwrap();
    let assembly = crate::host::assemble::WorkspaceAssembly {
        startup_cwd: cwd.to_str().unwrap().into(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    };
    cfg.workspace_assembly = Some(assembly);
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let created = handle_request(
        "session/new",
        &json!({"cwd":cwd.to_str().unwrap()}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let id = created["sessionId"].as_str().unwrap().to_owned();
    append_human_message(&cfg, &id, "history from stopped instance").await;
    let close = command(
        &cfg,
        &id,
        "close-before-instance-stops",
        ControlAction::Close,
    )
    .await;
    handle_request("session/control", &close, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    assert_eq!(work(&cfg, &id).await.control.status, ControlStatus::Closed);
    let configuration = cfg.peri_config.read().clone();
    let provider = cfg.provider.read().clone();
    drop(sessions);
    drop(cfg);
    let mut restarted = make_server_config(configuration, provider, &tmp).await;
    restarted.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: cwd.to_str().unwrap().into(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    let mut loaded_sessions = HashMap::new();
    handle_request(
        "session/load",
        &json!({"sessionId":id,"cwd":cwd.to_str().unwrap()}),
        &restarted,
        &mut loaded_sessions,
        &transport,
    )
    .await
    .unwrap();
    assert!(!loaded_sessions[&id].closing);
    assert!(loaded_sessions[&id].cancel_token.is_none());
    assert_eq!(
        work(&restarted, &id).await.control.status,
        ControlStatus::Closed
    );
    let idle_environment = loaded_sessions[&id].environment.clone().unwrap();
    let idle_manager = idle_environment.task_manager();
    let reopen = command(
        &restarted,
        &id,
        "reopen-after-cold-closed-load",
        ControlAction::Reopen,
    )
    .await;
    handle_request(
        "session/control",
        &reopen,
        &restarted,
        &mut loaded_sessions,
        &transport,
    )
    .await
    .unwrap();
    let fresh_environment = loaded_sessions[&id].environment.clone().unwrap();
    assert!(!Arc::ptr_eq(&idle_environment, &fresh_environment));
    assert!(!Arc::ptr_eq(
        &idle_manager,
        &fresh_environment.task_manager()
    ));
    assert!(idle_manager
        .as_any()
        .downcast_ref::<peri_agent::agent::async_tasks::TaskManager>()
        .unwrap()
        .session_close_settled());
    let fresh_session = fresh_environment
        .cfg
        .session_manager
        .get_session(&id)
        .unwrap();
    assert_eq!(fresh_session.recipient_lifecycle, 2);
    assert!(fresh_session
        .task_events_started
        .load(std::sync::atomic::Ordering::Acquire));
    assert!(fresh_session.v2_message_queue.is_empty());
    assert_eq!(loaded_sessions[&id].cwd, cwd.to_str().unwrap());
    assert_eq!(loaded_sessions[&id].history.len(), 1);
    assert_eq!(work(&restarted, &id).await.control.lifecycle, 2);
    drop(fresh_session);
    let pool = fresh_environment.cfg.mcp_pool.as_ref().unwrap();
    let (_, bound_manager) = pool
        .clone()
        .agent_session_binding_for_lifecycle(&id, 2)
        .await
        .unwrap()
        .unwrap();
    assert!(Arc::ptr_eq(
        &bound_manager,
        &fresh_environment.task_manager()
    ));
    assert!(
        matches!(pool.clone().agent_session_binding_for_lifecycle(&id, 1).await,
        Err(reason) if reason.contains("stale session lifecycle"))
    );
    let resolved = handle_request(
        "session/control/resolve",
        &reopen,
        &restarted,
        &mut loaded_sessions,
        &transport,
    )
    .await
    .unwrap();
    assert!(
        matches!(serde_json::from_value::<ControlResolution>(resolved).unwrap(),
        ControlResolution::Applied { receipt }
            if receipt.decision == ControlDecision::Accepted && receipt.state.lifecycle == 2)
    );
    assert!(Arc::ptr_eq(
        &fresh_environment,
        loaded_sessions[&id].environment.as_ref().unwrap()
    ));
}

/// [回归测试] Reopen rejects real futures and Independent children without cancelling another owner.
#[tokio::test]
async fn reopen_preserves_actual_owned_futures_and_independent_children() {
    for independent_child in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let (cfg, mut sessions, id) = fixture(&tmp).await;
        resource_owners::bind(&cfg, &id, &HashMap::new())
            .await
            .unwrap();
        let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
        let close = command(&cfg, &id, "close-before-view-load", ControlAction::Close).await;
        handle_request("session/control", &close, &cfg, &mut sessions, &transport)
            .await
            .unwrap();
        handle_request(
            "session/load",
            &json!({"sessionId":id,"cwd":tmp.path().to_str().unwrap()}),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap();
        let token = if independent_child {
            let child = peri_acp_types::session::AgentRuntime::new(
                "live-independent-child".into(),
                peri_acp_types::thread::CancelPolicy::Independent,
            );
            let token = child.cancel_token.clone();
            cfg.session_manager
                .get_session_mut(&id)
                .unwrap()
                .active_agents
                .insert("live-independent-child".into(), child);
            token
        } else {
            let token = CancellationToken::new();
            sessions.get_mut(&id).unwrap().cancel_token = Some(token.clone());
            token
        };
        let previous_manager = cfg
            .session_manager
            .get_session(&id)
            .unwrap()
            .task_manager
            .clone();
        let reopen = command(&cfg, &id, "reopen-must-not-steal", ControlAction::Reopen).await;
        let error = handle_request("session/control", &reopen, &cfg, &mut sessions, &transport)
            .await
            .unwrap_err();
        assert!(error
            .message
            .contains("previous live execution is not closed"));
        assert!(!token.is_cancelled());
        assert!(Arc::ptr_eq(
            &previous_manager,
            &cfg.session_manager.get_session(&id).unwrap().task_manager
        ));
        assert!(!previous_manager
            .as_any()
            .downcast_ref::<peri_agent::agent::async_tasks::TaskManager>()
            .unwrap()
            .session_close_settled());
        assert!(sessions.contains_key(&id));
    }
}
