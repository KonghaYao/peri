//! Real SQLite spawn/compact/reopen/resume regression for read-only inherited history.
use super::*;
use crate::agent::compact_v2::{
    micro_compact, run_compact, CompactConfig, CompactOutcome, ContextPressure,
};
use crate::messages::MessageId;
use peri_acp_types::projection::{
    MessageProjectionDirective, ProjectionAction, ProjectionActionEntry, ProjectionTarget,
};
use peri_acp_types::session_resources::{SessionResources, SessionStoreShutdownPort};
use peri_acp_types::store::PersistedPayload;

struct SummaryModel;
#[async_trait::async_trait]
impl peri_model::Model for SummaryModel {
    fn capabilities(&self) -> peri_model::ModelCapabilities {
        peri_model::ModelCapabilities {
            supports_tools: false,
            supports_reasoning: false,
            supports_vision: false,
            supports_streaming: false,
        }
    }
    async fn stream(
        &self,
        _: peri_model::ModelRequest,
        _: CancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelStream> {
        Err(peri_model::ModelError::cancelled())
    }
    async fn complete(
        &self,
        _: peri_model::ModelRequest,
        _: CancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelResponse> {
        peri_model::ModelResponse::new(
            peri_model::ModelMessage::assistant_text("<summary>child durable summary</summary>"),
            peri_model::StopReason::EndTurn,
            None,
            None,
        )
    }
}

fn directive(id: MessageId) -> MessageProjectionDirective {
    MessageProjectionDirective {
        policy_version: crate::agent::compact_v2::PROJECTION_POLICY_VERSION,
        entries: vec![ProjectionActionEntry {
            message_id: id,
            target: ProjectionTarget::Message,
            action: ProjectionAction::CompactToolResult {
                keep_head: 50,
                keep_tail: 20,
                preserve_recovery_handle: false,
            },
        }],
    }
}

fn spawn_config(
    store: Arc<dyn SessionResources>,
    messages: Vec<BaseMessage>,
    cwd: &str,
) -> SubagentSpawnConfig {
    SubagentSpawnConfig {
        agent_name: "snapshot-child".into(),
        prompt: "child prompt".into(),
        parent_messages: messages,
        cancel_policy: SubagentCancelPolicy::Independent,
        max_iterations: 10,
        fork_directive_kind: Some(ForkDirectiveKind::Fork),
        run_mode: SubagentRunMode::Sync,
        skill_names: vec![],
        llm: Box::new(EchoLLM),
        chain_assembler: Arc::new(EmptyChainAssembler),
        tools: vec![],
        tool_filter: Arc::new(|_| true),
        system_prompt: None,
        tool_invocation_resolver: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        session_resources: Some(store),
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
        cancel_token: None,
        cwd: Some(cwd.into()),
        parent_thread_id: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_date: None,
    }
}

async fn compact(session: &Arc<Session>, cwd: &str) {
    let pressure = ContextPressure {
        estimated_tokens: 96_000,
        context_window: 100_000,
        output_reserve: 4_000,
        predicted_tool_growth: 0,
        safety_buffer: 5_000,
        cache_hit_rate: 0.0,
    };
    let transcript = session.transcript();
    let mut owned = std::mem::take(&mut *transcript.write());
    let outcome = run_compact(
        &mut owned,
        Some(&SummaryModel),
        &CompactConfig::default(),
        &pressure,
        true,
        &mut 0,
        cwd,
    )
    .await;
    *transcript.write() = owned;
    assert_eq!(outcome.outcome, CompactOutcome::FullApplied);
}

async fn flush_session(session: &Arc<Session>) {
    let arc = session.transcript();
    let transcript = std::mem::take(&mut *arc.write());
    transcript.flush_persistence().await.unwrap();
    *arc.write() = transcript;
}

/// [回归测试] 原 parent ID 不得 append 成 child own；父 Full 之后冷恢复仍使用 spawn 时的父投影。
#[tokio::test]
async fn test_sqlite_subagent_spawn_full_micro_cold_resume_preserves_provenance() {
    let repo = crate::session::test_resources::git_repository();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("provenance.db");
    // 真门面（真 SQLite）：绑定、frozen 原字节与继承区都来自真实实现，
    // 冷重开是同一库文件的第二个句柄——不是另一个空替身。
    let resources = peri_resources::Resources::open_with(Some(db_path.clone()))
        .await
        .unwrap();
    let (store, shutdown) = resources.into_parts();
    let workspace = store.resolve_workspace(repo.path()).await.unwrap();
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    let parent_id = create_bound_root(&store, &workspace, None).await;
    let parent = Session::new(
        Arc::from(cwd.as_str()),
        FrozenContext::builder().build(),
        Some(parent_id.clone()),
    );
    let parent_hidden = BaseMessage::human("old parent excluded");
    let parent_tool = BaseMessage::tool_result("parent-bash", "parent-output-".repeat(1_000));
    let parent_messages = vec![
        parent_hidden.clone(),
        BaseMessage::human("parent question"),
        BaseMessage::ai_with_tool_calls(
            "parent tool",
            vec![ToolCallRequest::new(
                "parent-bash",
                "Bash",
                serde_json::json!({"command":"fixture"}),
            )],
        ),
        parent_tool.clone(),
    ];
    {
        let arc = parent.transcript();
        let mut transcript = crate::session::transcript::MessageTranscript::new()
            .with_persistence(store.clone(), parent_id.clone());
        for message in &parent_messages {
            transcript.append(message.clone());
        }
        transcript.set_excluded(parent_hidden.id(), true);
        transcript.set_flags_projection(parent_tool.id(), directive(parent_tool.id()));
        transcript.flush_persistence().await.unwrap();
        *arc.write() = transcript;
    }
    let config = spawn_config(store.clone(), parent_messages.clone(), &cwd);
    let spawned = SessionFactory::spawn_subagent(Some(&parent), config)
        .await
        .unwrap();
    let child_id = spawned.child_thread_id.clone();
    assert_eq!(
        spawned.session.transcript().read().ancestor_len(),
        parent_messages.len()
    );
    compact(&spawned.session, &cwd).await;
    let own_tool = BaseMessage::tool_result("child-bash", "child-output-".repeat(1_000));
    {
        let arc = spawned.session.transcript();
        let mut transcript = std::mem::take(&mut *arc.write());
        transcript.append(BaseMessage::human("child inspection"));
        transcript.append(BaseMessage::ai_with_tool_calls(
            "child tool",
            vec![ToolCallRequest::new(
                "child-bash",
                "Bash",
                serde_json::json!({"command":"fixture"}),
            )],
        ));
        transcript.append(own_tool.clone());
        assert!(
            micro_compact(
                &mut transcript,
                &CompactConfig {
                    micro_compact_stale_steps: 0,
                    ..Default::default()
                }
            ) > 0
        );
        assert!(transcript.flags(own_tool.id()).truncated);
        transcript.flush_persistence().await.unwrap();
        transcript.shutdown_persistence();
        *arc.write() = transcript;
    }
    let child_flags = store.load_session_snapshot(&child_id).await.unwrap().flags;
    assert!(child_flags.values().any(|flags| flags.excluded));
    assert!(child_flags[&own_tool.id()].truncated);
    assert!(parent_messages
        .iter()
        .all(|message| !child_flags.contains_key(&message.id())));
    let own_ids = store
        .load_session_snapshot(&child_id)
        .await
        .unwrap()
        .payloads
        .iter()
        .map(PersistedPayload::id)
        .collect::<Vec<_>>();
    assert!(parent_messages
        .iter()
        .all(|message| !own_ids.contains(&message.id())));
    let parent_flags_before = store.load_session_snapshot(&parent_id).await.unwrap().flags;
    assert_eq!(
        parent_flags_before[&parent_tool.id()].projection,
        Some(directive(parent_tool.id()))
    );
    compact(&parent, &cwd).await;
    assert!(
        store.load_session_snapshot(&parent_id).await.unwrap().flags[&parent_tool.id()].excluded
    );
    parent.transcript().read().shutdown_persistence();
    drop(spawned);
    // 关闭走部署关闭权（业务句柄没有全局关闭；这里与部署装配同形）。
    shutdown.shutdown().await.unwrap();
    let reopened_resources = peri_resources::Resources::open_with(Some(db_path.clone()))
        .await
        .unwrap();
    let (reopened, reopened_shutdown) = reopened_resources.into_parts();
    let recording = RecordingLLM::new();
    let received = recording.received.clone();
    let config = resume_config_with(
        Arc::clone(&reopened),
        child_id.clone(),
        Box::new(recording),
        SubagentRunMode::Sync,
        None,
        None,
    );
    let resumed = SessionFactory::resume_subagent(Some(&parent), config)
        .await
        .unwrap();
    {
        let arc = resumed.session.transcript();
        let transcript = arc.read();
        assert_eq!(transcript.ancestor_len(), parent_messages.len());
        assert!(
            !transcript.flags(parent_tool.id()).excluded,
            "父 Full 不能污染 child 冻结 snapshot"
        );
        assert_eq!(
            transcript.flags(parent_tool.id()).projection,
            Some(directive(parent_tool.id()))
        );
        for (id, flags) in child_flags {
            assert_eq!(transcript.flags(id), flags, "当前 child own flags 必须恢复");
        }
    }
    {
        let requests = received.read();
        let request = &requests[0];
        assert!(request
            .iter()
            .any(|message| message.content().contains("child durable summary")));
        assert!(request
            .iter()
            .all(|message| message.id() != parent_hidden.id()));
        let parent_view = request
            .iter()
            .find(|message| message.id() == parent_tool.id())
            .unwrap();
        let own_view = request
            .iter()
            .find(|message| message.id() == own_tool.id())
            .unwrap();
        assert!(
            parent_view.content().len() < 500,
            "冻结的 ancestor projection 必须仍渲染"
        );
        assert!(own_view.content().len() < own_tool.content().len());
    }
    flush_session(&resumed.session).await;
    resumed.session.transcript().read().shutdown_persistence();
    drop(resumed);
    reopened_shutdown.shutdown().await.unwrap();
}
