use super::*;

/// [回归测试] production 首个 Reason 必须看到同一 middleware chain 在
/// before_agent 后生成的 ToolSearch dynamic prompt contribution。
#[cfg(not(windows))]
#[tokio::test]
async fn test_production_first_reason_sees_after_before_agent_dynamic_contributions() {
    use peri_acp_types::session::{MessageKind, MessageSource, QueuedMessage};
    use peri_agent::agent::stages::{run_react_loop, LoopResult};

    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = Arc::new(CapturePromptModel {
        requests: Arc::clone(&requests),
    }) as Arc<dyn Model>;
    let mut ctx = make_session_context("dynamic-first-reason").await;
    ctx.primary_llm_factory = Some(Arc::new(move || Arc::clone(&model)));
    let frozen = frozen_with_dynamic_prompt_policy("DYNAMIC_BASE_SENTINEL", &[]);
    let (out, _) = make_stage_build(&ctx)(make_stage_request(frozen, None)).unwrap();
    let parent = Arc::clone(&out.session);
    out.context.session.queue.push(QueuedMessage::new(
        MessageKind::Prompt,
        MessageSource::UserInput,
        BaseMessage::human("finish"),
    ));

    let loop_result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run_react_loop(out.context, 1),
    )
    .await
    .expect("真实 loop 不得挂起");

    assert!(matches!(loop_result, LoopResult::Completed));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let system = system_text(&requests[0]);
    let tool_search_offset = system
        .find("## Deferred Tools")
        .expect("首个 Reason 缺少 ToolSearch contribution");
    assert_eq!(system.matches("DYNAMIC_BASE_SENTINEL").count(), 1);
    assert_eq!(system.matches("## Deferred Tools").count(), 1);
    let tool_search = &system[tool_search_offset..];
    assert_eq!(
        tool_search
            .matches("Discover deferred tools by name or keyword")
            .count(),
        1
    );
    assert_eq!(
        tool_search
            .matches("Invoke a registered deferred tool by name")
            .count(),
        1
    );
    assert_eq!(
        &*parent.store().frozen.system_prompt,
        "DYNAMIC_BASE_SENTINEL"
    );
    assert!(!parent
        .store()
        .frozen
        .system_prompt
        .contains("## Deferred Tools"));
}

#[derive(Clone)]
struct RecordingSubagentAssembler {
    context: Arc<Mutex<Option<peri_agent::session::subagent::SubagentChainContext>>>,
}

impl peri_agent::session::subagent::SubagentChainAssembler for RecordingSubagentAssembler {
    fn assemble(
        &self,
        ctx: &peri_agent::session::subagent::SubagentChainContext,
    ) -> peri_agent::middleware::MiddlewareChain {
        *self.context.lock().unwrap() = Some(ctx.clone());
        peri_agent::middleware::MiddlewareChain::new()
    }
}

struct FixedAnswerModel;

#[async_trait]
impl peri_model::Model for FixedAnswerModel {
    fn capabilities(&self) -> peri_model::ModelCapabilities {
        peri_model::ModelCapabilities {
            supports_streaming: true,
            ..Default::default()
        }
    }

    async fn stream(
        &self,
        _: peri_model::ModelRequest,
        cancellation: AgentCancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelStream> {
        let response = peri_model::ModelResponse::new(
            peri_model::ModelMessage::assistant_text("done"),
            peri_model::StopReason::EndTurn,
            None,
            None,
        )?;
        Ok(peri_model::ModelStream::with_parent_cancellation(
            futures::stream::iter(vec![Ok(peri_model::ModelStreamEvent::Completed(response))]),
            cancellation,
        ))
    }
}

fn prepared_child_model() -> Box<dyn peri_agent::agent::react::ReactLLM + Send + Sync> {
    Box::new(peri_agent::agent::model_bridge::AgentModelBridge::new(
        execution_fixture::wrap_model(Arc::new(FixedAnswerModel)),
    ))
}

struct ChildFixtureSdk {
    template: SessionContext,
    sessions: Mutex<
        std::collections::BTreeMap<
            String,
            Arc<dyn peri_acp_types::execution_admission::ExecutionAdmissionPort>,
        >,
    >,
}

impl ChildFixtureSdk {
    fn port(
        &self,
        session_id: &str,
    ) -> Arc<dyn peri_acp_types::execution_admission::ExecutionAdmissionPort> {
        let mut sessions = self.sessions.lock().unwrap();
        sessions
            .entry(session_id.into())
            .or_insert_with(|| {
                let mut context = self.template.clone();
                context.session_id = session_id.into();
                context.thread_id = Some(session_id.into());
                context.session_access = None;
                execution_fixture::bind_execution(&mut context, None);
                context.execution_admission_port.unwrap()
            })
            .clone()
    }
}

#[async_trait]
impl peri_acp_types::execution_admission::ExecutionAdmissionPort for ChildFixtureSdk {
    async fn admit(
        &self,
        request: peri_acp_types::execution_admission::AdmissionRequest,
    ) -> Result<
        peri_acp_types::execution_admission::AdmissionOutcome,
        peri_acp_types::execution_admission::ExecutionAdmissionError,
    > {
        self.port(&request.snapshot.session_id).admit(request).await
    }

    async fn entered(
        &self,
        request: peri_acp_types::execution_admission::EntryRequest,
    ) -> Result<
        peri_acp_types::execution_admission::EntryOutcome,
        peri_acp_types::execution_admission::ExecutionAdmissionError,
    > {
        self.port(&request.admission.session_id)
            .entered(request)
            .await
    }

    async fn settle(
        &self,
        request: peri_acp_types::execution_admission::SettlementRequest,
    ) -> Result<
        peri_acp_types::execution_admission::SettlementOutcome,
        peri_acp_types::execution_admission::ExecutionAdmissionError,
    > {
        self.port(&request.admission.session_id)
            .settle(request)
            .await
    }
}

async fn bind_child_fixture_resources(
    context: &mut SessionContext,
    cwd: &std::path::Path,
    frozen: &FrozenSessionData,
) -> tempfile::TempDir {
    use peri_acp_types::session_resources::{
        FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources,
    };
    let directory = tempfile::tempdir().unwrap();
    let resources: Arc<dyn SessionResources> = Arc::new(
        peri_resources::sessions::SessionResourcesImpl::open(
            directory.path().join("child-execution.db"),
        )
        .await
        .unwrap(),
    );
    let workspace = resources.resolve_workspace(cwd).await.unwrap();
    resources
        .create_session(&NewSession {
            thread_id: context.session_id.clone(),
            created_at: peri_time::now_utc_rfc3339(),
            meta: NewSessionMeta {
                title: None,
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: peri_acp_types::workspace::SessionBinding::from_workspace(&workspace),
            frozen: FrozenSnapshotBytes::new(
                crate::session::frozen_snapshot::encode_frozen_snapshot(frozen).unwrap(),
            ),
        })
        .await
        .unwrap();
    context.cwd = workspace.cwd.to_string_lossy().into_owned();
    context.session_resources = Some(resources);
    context.thread_id = Some(context.session_id.clone());
    context.session_access = None;
    execution_fixture::bind_execution(context, None);
    let root = context.execution_admission_port.as_ref().unwrap().clone();
    context.execution_admission_port = Some(Arc::new(ChildFixtureSdk {
        template: context.clone(),
        sessions: Mutex::new(std::collections::BTreeMap::from([(
            context.session_id.clone(),
            root,
        )])),
    }));
    directory
}

async fn prepare_child_fixture_intent(context: &SessionContext) -> String {
    use peri_acp_types::session_resources::work::*;
    use sha2::{Digest, Sha256};
    let resources = context.session_resources.as_ref().unwrap();
    let query = WorkQuery::new(&context.session_id, WorkSelector::Head);
    let snapshot = resources.inspect_work(&query).await.unwrap();
    let authorization_ref = "explicit-dynamic-fixture:no-external-tools".to_owned();
    let receipt = resources
        .apply_work_mutation(&WorkCommand {
            session_id: context.session_id.clone(),
            recipient_lifecycle: snapshot.control.lifecycle,
            mutation_id: uuid::Uuid::now_v7().to_string(),
            action: WorkAction::BindResourceOwners {
                expected_revision: snapshot.head.change_seq,
                connections_json: "[]".into(),
                authorization_ref: authorization_ref.clone(),
            },
        })
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    let invocation_id = uuid::Uuid::now_v7().to_string();
    let arguments_json = serde_json::json!({"prompt":"finish"}).to_string();
    let arguments_digest = format!("{:x}", Sha256::digest(arguments_json.as_bytes()));
    let arguments = resources
        .prepare_evidence(&EvidenceWrite {
            session_id: context.session_id.clone(),
            storage_scope: context.session_id.clone(),
            payload_id: format!("dynamic-child-arguments:{invocation_id}"),
            encoding: 1,
            bytes: arguments_json.into_bytes(),
        })
        .await
        .unwrap();
    let intent = InvocationIntent {
        invocation_id: invocation_id.clone(),
        tool_call_id: uuid::Uuid::now_v7().to_string(),
        tool_name: "Subagent".into(),
        arguments: arguments.clone(),
        arguments_digest: arguments_digest.clone(),
        effective_tool_name: "Subagent".into(),
        effective_arguments: arguments,
        effective_arguments_digest: arguments_digest,
        owner_identity: "explicit-dynamic-fixture-local-owner".into(),
        scope_id: context.session_id.clone(),
        scope_epoch: None,
        authorization_ref,
        recovery_locator: format!("dynamic-child:{invocation_id}"),
    };
    prepare_dynamic_dispatch(context, intent).await;
    invocation_id
}

async fn dynamic_fixture_action(
    resources: &dyn peri_acp_types::session_resources::SessionResources,
    session_id: &str,
    action: peri_acp_types::session_resources::work::WorkAction,
) {
    use peri_acp_types::session_resources::work::*;
    let head = resources
        .inspect_work(&WorkQuery::new(session_id, WorkSelector::Head))
        .await
        .unwrap();
    let receipt = resources
        .apply_work_mutation(&WorkCommand {
            session_id: session_id.into(),
            recipient_lifecycle: head.control.lifecycle,
            mutation_id: uuid::Uuid::now_v7().to_string(),
            action,
        })
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted, "{receipt:?}");
}

async fn prepare_dynamic_dispatch(
    context: &SessionContext,
    intent: peri_acp_types::session_resources::work::InvocationIntent,
) {
    use peri_acp_types::{execution_admission::*, session_resources::work::*};
    let resources = context.session_resources.as_ref().unwrap().as_ref();
    let session_id = context.session_id.as_str();
    let delivery_id = uuid::Uuid::now_v7().to_string();
    let content = crate::host::work_query::test_payload(
        resources,
        session_id,
        &peri_acp_types::store::PersistedPayload::Message(BaseMessage::human("finish")),
    )
    .await;
    dynamic_fixture_action(
        resources,
        session_id,
        WorkAction::PublishDelivery {
            delivery: PublishDelivery {
                delivery_id: delivery_id.clone(),
                event: WorkEvent {
                    producer_namespace: "dynamic-fixture".into(),
                    event_id: delivery_id,
                    event_kind: "userInput".into(),
                    causation_id: None,
                    content,
                },
                purpose: DeliveryPurpose::UserInput,
                policy: peri_acp_types::session::MessagePolicy::ensure_processing(),
            },
        },
    )
    .await;
    let available = resources
        .inspect_work(&WorkQuery::new(session_id, WorkSelector::Availability))
        .await
        .unwrap();
    let candidate = &crate::host::work_query::availability(&available)
        .unwrap()
        .candidates[0];
    let sdk = context.execution_admission_port.as_ref().unwrap();
    let AdmissionOutcome::Admitted { admission } = sdk
        .admit(AdmissionRequest {
            request_id: uuid::Uuid::now_v7().to_string(),
            snapshot: (&available).into(),
            existing_admission: None,
        })
        .await
        .unwrap()
    else {
        panic!("dynamic fixture requires SDK admission");
    };
    let delivery_ids = candidate.delivery_ids.clone();
    let registered = WorkCommand {
        session_id: session_id.into(),
        recipient_lifecycle: admission.lifecycle,
        mutation_id: uuid::Uuid::now_v7().to_string(),
        action: WorkAction::RegisterAdmission {
            admission: admission.clone(),
        },
    };
    assert_eq!(
        resources
            .apply_work_mutation(&registered)
            .await
            .unwrap()
            .decision,
        WorkDecision::Accepted
    );
    assert!(matches!(sdk.entered(EntryRequest {
        admission: admission.clone(), entry_evidence_id: registered.mutation_id,
    }).await.unwrap(), EntryOutcome::Applied { receipt } if receipt.admission == admission));
    let snapshot = resources
        .inspect_work(&WorkQuery::new(session_id, WorkSelector::Head))
        .await
        .unwrap();
    dynamic_fixture_action(
        resources,
        session_id,
        WorkAction::ClaimBatch {
            guard: WorkGuard {
                expected_revision: snapshot.head.change_seq,
                expected_control_generation: admission.control_generation,
                execution: admission.execution.clone(),
            },
            batch_id: admission.work_id.clone(),
            delivery_ids,
        },
    )
    .await;
    let snapshot = resources
        .inspect_work(&WorkQuery::new(session_id, WorkSelector::Head))
        .await
        .unwrap();
    let processing =
        crate::host::work_query::test_processing(resources, session_id, &admission.work_id).await;
    let request_id = uuid::Uuid::now_v7().to_string();
    dynamic_fixture_action(
        resources,
        session_id,
        WorkAction::BeginReason {
            guard: WorkGuard {
                expected_revision: snapshot.head.change_seq,
                expected_control_generation: admission.control_generation,
                execution: admission.execution.clone(),
            },
            target: WorkTarget {
                work_id: admission.work_id.clone(),
                expected_work_revision: processing.revision,
            },
            request_id: request_id.clone(),
            request: ReasonRequest {
                payload: intent.arguments.clone(),
                request_digest: intent.arguments_digest.clone(),
                model_ref: "dynamic-fixture-model".into(),
                authorization_ref: intent.authorization_ref.clone(),
            },
        },
    )
    .await;
    let snapshot = resources
        .inspect_work(&WorkQuery::new(session_id, WorkSelector::Head))
        .await
        .unwrap();
    let processing =
        crate::host::work_query::test_processing(resources, session_id, &admission.work_id).await;
    let response = crate::host::work_query::test_payload(
        resources,
        session_id,
        &peri_acp_types::store::PersistedPayload::Message(BaseMessage::ai_with_tool_calls(
            "finish",
            vec![peri_acp_types::messages::ToolCallRequest::new(
                &intent.tool_call_id,
                &intent.tool_name,
                serde_json::json!({"prompt":"finish"}),
            )],
        )),
    )
    .await;
    dynamic_fixture_action(
        resources,
        session_id,
        WorkAction::CommitReasonResponseAndDispatchIntent {
            guard: WorkGuard {
                expected_revision: snapshot.head.change_seq,
                expected_control_generation: admission.control_generation,
                execution: admission.execution.clone(),
            },
            target: WorkTarget {
                work_id: admission.work_id.clone(),
                expected_work_revision: processing.revision,
            },
            request_id,
            response,
            dispatch_intents: vec![intent.clone()],
            next_work_id: None,
        },
    )
    .await;
    let snapshot = resources
        .inspect_work(&WorkQuery::new(session_id, WorkSelector::Head))
        .await
        .unwrap();
    let processing =
        crate::host::work_query::test_processing(resources, session_id, &admission.work_id).await;
    let effects = resources
        .inspect_work(&WorkQuery::new(
            session_id,
            WorkSelector::Effect {
                invocation_id: intent.invocation_id.clone(),
            },
        ))
        .await
        .unwrap();
    let WorkPage::Effects(effects) = effects.page else {
        panic!("dynamic fixture requires its actual effect");
    };
    assert_eq!(effects[0].status, InvocationStatus::Prepared);
    dynamic_fixture_action(
        resources,
        session_id,
        WorkAction::BeginDispatch {
            guard: WorkGuard {
                expected_revision: snapshot.head.change_seq,
                expected_control_generation: admission.control_generation,
                execution: admission.execution,
            },
            target: WorkTarget {
                work_id: admission.work_id,
                expected_work_revision: processing.revision,
            },
            invocation_id: intent.invocation_id,
            expected_effect_revision: effects[0].revision,
        },
    )
    .await;
}

/// [回归测试] production stage 创建的主 Session 必须保存 session/new 的完整
/// snapshot；一级 child 与其链装配 context 继续复用同一冻结事实和 disabled set。
#[tokio::test]
async fn test_production_stage_propagates_frozen_snapshot_to_main_and_child() {
    use peri_agent::session::subagent::{
        SessionFactory, SubagentCancelPolicy, SubagentRunMode, SubagentSpawnConfig,
    };

    let mut ctx = make_session_context("frozen-production-parent").await;
    ctx.language = Some("en-US".into());
    let sentinel = make_sentinel_frozen();
    let workspace = tempfile::tempdir().unwrap();
    let _resources = bind_child_fixture_resources(&mut ctx, workspace.path(), &sentinel).await;
    let invocation_id = prepare_child_fixture_intent(&ctx).await;
    let stage_build = make_stage_build(&ctx);
    let (out, _) = stage_build(make_stage_request(sentinel.clone(), None)).unwrap();
    let parent_frozen = &out.session.store().frozen;
    assert_eq!(&*parent_frozen.system_prompt, sentinel.system_prompt());
    assert_eq!(&*parent_frozen.claude_md, "FROZEN_CLAUDE_SENTINEL");
    assert_eq!(&*parent_frozen.skill_summary, "FROZEN_SKILLS_SENTINEL");
    assert_eq!(&*parent_frozen.date, "1999-12-31");
    assert_eq!(parent_frozen.language.as_deref(), Some("zh-CN"));
    assert_eq!(parent_frozen.meta_harness, *sentinel.meta_harness());
    let local = out
        .session
        .subagent_host()
        .and_then(|host| host.frozen_claude_local_md.as_ref().map(|v| (**v).clone()));
    assert_eq!(local.as_deref(), Some("FROZEN_LOCAL_SENTINEL"));

    let recorded = Arc::new(Mutex::new(None));
    let child = SessionFactory::spawn_subagent(
        Some(&out.session),
        SubagentSpawnConfig {
            agent_name: "frozen-child".into(),
            prompt: "finish".into(),
            parent_messages: vec![],
            cancel_policy: SubagentCancelPolicy::Cascade,
            max_iterations: 1,
            fork_directive_kind: None,
            run_mode: SubagentRunMode::Sync,
            skill_names: vec![],
            llm: prepared_child_model(),
            chain_assembler: Arc::new(RecordingSubagentAssembler {
                context: Arc::clone(&recorded),
            }),
            tools: vec![],
            tool_filter: peri_agent::session::tool_catalog::ToolFilterPolicy::canonical(
                Some(vec![]),
                vec![],
            ),
            system_prompt: None,
            tool_invocation_resolver: None,
            compact_config: None,
            context_budget: None,
            compact_llm: None,
            session_resources: ctx.session_resources.clone(),
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
            parent_invocation_id: Some(invocation_id),
            cancel_token: None,
            cwd: None,
            parent_thread_id: Some(ctx.session_id.clone()),
            frozen_claude_md: None,
            frozen_claude_local_md: local,
            frozen_skill_summary: None,
            frozen_date: None,
        },
    )
    .await
    .expect("child spawn 必须成功");
    let child_frozen = &child.session.store().frozen;
    assert_eq!(&*child_frozen.system_prompt, "BASE_FROZEN_SYSTEM_SENTINEL");
    assert_eq!(&*child_frozen.claude_md, "FROZEN_CLAUDE_SENTINEL");
    assert_eq!(&*child_frozen.skill_summary, "FROZEN_SKILLS_SENTINEL");
    assert_eq!(&*child_frozen.date, "1999-12-31");
    assert_eq!(child_frozen.language.as_deref(), Some("zh-CN"));
    assert_eq!(child_frozen.meta_harness, *sentinel.meta_harness());
    let child_ctx = recorded
        .lock()
        .unwrap()
        .clone()
        .expect("child chain context");
    assert_eq!(
        child_ctx.frozen_claude_md.as_deref(),
        Some("FROZEN_CLAUDE_SENTINEL")
    );
    assert_eq!(
        child_ctx.frozen_claude_local_md.as_deref(),
        Some("FROZEN_LOCAL_SENTINEL")
    );
    assert_eq!(
        child_ctx.frozen_skill_summary.as_deref(),
        Some("FROZEN_SKILLS_SENTINEL")
    );
    assert_eq!(
        child_ctx.meta_harness_disabled,
        sentinel.meta_harness().disabled_middlewares
    );
}

/// [回归测试] override 只生成 model-facing effective prompt；其语言、段落覆盖
/// 与 disabled policy 必须来自 frozen snapshot，不能被当轮 SessionContext 覆盖。
#[cfg(not(windows))]
#[tokio::test]
async fn test_production_stage_uses_frozen_language_and_keeps_override_out_of_base_prompt() {
    use peri_acp_types::agents::AgentOverrides;

    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = Arc::new(CapturePromptModel {
        requests: Arc::clone(&requests),
    }) as Arc<dyn Model>;
    let mut ctx = make_session_context("frozen-language-override").await;
    ctx.language = Some("en-US".into());
    ctx.primary_llm_factory = Some(Arc::new(move || Arc::clone(&model)));
    let sentinel = make_sentinel_frozen();
    let stage_build = make_stage_build(&ctx);
    let (out, _) = stage_build(make_stage_request(
        sentinel.clone(),
        Some(AgentOverrides {
            persona: Some("OVERRIDE_PERSONA_MARKER".into()),
            ..Default::default()
        }),
    ))
    .unwrap();

    out.context
        .runtime
        .llm
        .generate_reasoning(&[], &[], None)
        .await
        .expect("capturing model 必须返回固定完成响应");

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let effective = requests[0].messages[0]
        .text_content()
        .expect("首条必须是 system message");
    assert!(effective.contains("OVERRIDE_PERSONA_MARKER"), "{effective}");
    assert!(
        effective.contains("FROZEN_SECTION_OVERRIDE_MARKER"),
        "{effective}"
    );
    assert!(
        effective.contains("Always respond in Simplified Chinese"),
        "{effective}"
    );
    assert!(
        !effective.contains("Always respond in en-US"),
        "{effective}"
    );
    assert!(
        effective.contains("Today's date: 1999-12-31"),
        "{effective}"
    );
    assert_eq!(effective.matches("Today's date:").count(), 1, "{effective}");
    assert_eq!(
        &*out.session.store().frozen.system_prompt,
        "BASE_FROZEN_SYSTEM_SENTINEL"
    );
    assert!(!out
        .session
        .store()
        .frozen
        .system_prompt
        .contains("OVERRIDE_PERSONA_MARKER"));
    assert_eq!(
        out.session.store().frozen.meta_harness.disabled_middlewares,
        sentinel.meta_harness().disabled_middlewares
    );
}

/// [回归测试] 空 CLAUDE/skills 是冻结的“缺席”而非 legacy None。即使文件在
/// snapshot 后出现，真实 before_agent lifecycle 也不得把 marker 注入贡献。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn test_production_stage_keeps_empty_frozen_prompt_inputs_after_late_files_appear() {
    use peri_acp_types::session::{MessageKind, MessageSource, QueuedMessage};
    use peri_agent::agent::stages::{run_react_loop, LoopResult};
    use peri_agent::session::FrozenContext;

    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(tmp.path());
    let cwd = tmp.path().join("project");
    std::fs::create_dir_all(cwd.join(".claude/skills/late-skill")).unwrap();
    let frozen = FrozenSessionData::from_frozen_parts(
        FrozenContext::builder()
            .system_prompt("EMPTY_FROZEN_BASE")
            .claude_md("")
            .skill_summary("")
            .date("2026-08-25")
            .build(),
        None,
    );
    std::fs::write(cwd.join("CLAUDE.md"), "LATE_CLAUDE_MARKER").unwrap();
    std::fs::write(
        cwd.join(".claude/skills/late-skill/SKILL.md"),
        "---\nname: late-skill\ndescription: LATE_SKILL_MARKER\n---\nlate\n",
    )
    .unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = Arc::new(CapturePromptModel {
        requests: Arc::clone(&requests),
    }) as Arc<dyn Model>;
    let mut ctx = make_session_context("late-frozen-files").await;
    let _resources = bind_child_fixture_resources(&mut ctx, &cwd, &frozen).await;
    ctx.primary_llm_factory = Some(Arc::new(move || Arc::clone(&model)));
    let stage_build = make_stage_build(&ctx);
    let (out, _) = stage_build(make_stage_request(frozen, None)).unwrap();
    let parent = Arc::clone(&out.session);
    let chain = Arc::clone(&out.context.runtime.middleware_chain);
    out.context.session.queue.push(QueuedMessage::new(
        MessageKind::Prompt,
        MessageSource::UserInput,
        BaseMessage::human("finish"),
    ));

    let loop_result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run_react_loop(out.context, 1),
    )
    .await
    .expect("真实 loop 不得挂起");

    assert!(
        matches!(loop_result, LoopResult::Completed),
        "{loop_result:?}"
    );
    use peri_acp_types::{execution_admission::*, session_resources::work::*};
    let resources = ctx.session_resources.as_ref().unwrap();
    let current = resources
        .inspect_work(&WorkQuery::new(
            &ctx.session_id,
            WorkSelector::CurrentAdmission,
        ))
        .await
        .unwrap();
    let WorkPage::Admissions(admissions) = current.page else {
        panic!("parent fixture requires its original admission");
    };
    let admission = admissions[0].admission.clone();
    let evidence_id = crate::host::execution::finish_admission(resources.as_ref(), &admission)
        .await
        .unwrap();
    assert!(
        matches!(ctx.execution_admission_port.as_ref().unwrap().settle(SettlementRequest {
        admission: admission.clone(),
        proof: AttemptStoppedProof::AttemptStopped {
            instance_id: admission.instance_id.clone(), generation_id: admission.generation_id.clone(),
            execution: admission.execution.clone(), evidence_id,
        },
    }).await.unwrap(), SettlementOutcome::Applied { receipt } if receipt.admission == admission)
    );
    let invocation_id = prepare_child_fixture_intent(&ctx).await;
    let contributions = chain.collect_prompt_contributions();
    assert!(
        !contributions.contains("LATE_CLAUDE_MARKER"),
        "{contributions}"
    );
    assert!(
        !contributions.contains("LATE_SKILL_MARKER"),
        "{contributions}"
    );
    assert_eq!(&*parent.store().frozen.claude_md, "");
    assert_eq!(&*parent.store().frozen.skill_summary, "");

    let recorded = Arc::new(Mutex::new(None));
    let child = peri_agent::session::subagent::SessionFactory::spawn_subagent(
        Some(&parent),
        peri_agent::session::subagent::SubagentSpawnConfig {
            agent_name: "empty-frozen-child".into(),
            prompt: "finish".into(),
            parent_messages: vec![],
            cancel_policy: peri_agent::session::subagent::SubagentCancelPolicy::Cascade,
            max_iterations: 1,
            fork_directive_kind: None,
            run_mode: peri_agent::session::subagent::SubagentRunMode::Sync,
            skill_names: vec![],
            llm: prepared_child_model(),
            chain_assembler: Arc::new(RecordingSubagentAssembler {
                context: Arc::clone(&recorded),
            }),
            tools: vec![],
            tool_filter: peri_agent::session::tool_catalog::ToolFilterPolicy::canonical(
                Some(vec![]),
                vec![],
            ),
            system_prompt: None,
            tool_invocation_resolver: None,
            compact_config: None,
            context_budget: None,
            compact_llm: None,
            session_resources: ctx.session_resources.clone(),
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
            parent_invocation_id: Some(invocation_id),
            cancel_token: None,
            cwd: None,
            parent_thread_id: Some(ctx.session_id.clone()),
            frozen_claude_md: None,
            frozen_claude_local_md: parent
                .subagent_host()
                .and_then(|host| host.frozen_claude_local_md.as_ref().map(|v| (**v).clone())),
            frozen_skill_summary: None,
            frozen_date: None,
        },
    )
    .await
    .expect("child spawn 必须成功");
    assert_eq!(&*child.session.store().frozen.claude_md, "");
    assert_eq!(&*child.session.store().frozen.skill_summary, "");
    let child_ctx = recorded.lock().unwrap().clone().expect("child context");
    assert_eq!(child_ctx.frozen_claude_md.as_deref(), Some(""));
    assert_eq!(child_ctx.frozen_skill_summary.as_deref(), Some(""));
}
