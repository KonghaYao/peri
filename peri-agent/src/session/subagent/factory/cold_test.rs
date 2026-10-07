use super::*;
use crate::agent::async_tasks::TaskManager;
use crate::agent::model_bridge::AgentModelBridge;
use crate::agent::stages::{run_react_loop, LoopResult};
use crate::middleware::MiddlewareChain;
use crate::session::subagent::SubagentChainContext;
use crate::session::test_resources::mock::admission::FixtureAdmission;
use crate::session::test_resources::TestSession;
use crate::session::{MessageQueue, MessageSource, QueuedMessage};
use peri_acp_types::execution_admission::{
    AdmissionOutcome, AdmissionRequest, ExecutionAdmissionPort,
};
use peri_acp_types::ports::{McpPoolPort, McpPoolShutdownReport};
use peri_acp_types::session::{InboxHandle, SessionInbox};
use peri_acp_types::session_resources::work::*;
use peri_acp_types::session_resources::{
    BindingState, FrozenSnapshotBytes, NewSession, NewSessionMeta,
};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct ChildPool {
    child_id: String,
    queue: MessageQueue,
    manager: Arc<TaskManager>,
}

#[async_trait::async_trait]
impl McpPoolPort for ChildPool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn snapshot(&self) -> Value {
        Value::Null
    }
    async fn shutdown(&self) -> McpPoolShutdownReport {
        panic!("cold child cannot shutdown shared owners")
    }
    async fn agent_session_binding_for_lifecycle(
        self: Arc<Self>,
        session_id: &str,
        lifecycle: u64,
    ) -> Result<Option<(InboxHandle, Arc<dyn peri_acp_types::tasks::TaskManager>)>, String> {
        assert_eq!(session_id, self.child_id);
        assert_eq!(lifecycle, 1);
        Ok(Some((
            SessionInbox::new(Arc::new(self.queue.clone())).handle(),
            self.manager.clone(),
        )))
    }
    fn bind_agent_session_for_lifecycle(
        &self,
        session_id: &str,
        lifecycle: u64,
        inbox: InboxHandle,
        manager: Arc<dyn peri_acp_types::tasks::TaskManager>,
    ) -> Result<(), String> {
        assert_eq!(session_id, self.child_id);
        assert_eq!(lifecycle, 1);
        let erased: Arc<dyn std::any::Any + Send + Sync> = manager;
        assert!(Arc::ptr_eq(
            &Arc::downcast::<TaskManager>(erased).unwrap(),
            &self.manager
        ));
        assert_eq!(inbox.queue().len(), self.queue.len());
        Ok(())
    }
    fn bind_agent_session_resources(
        &self,
        session_id: &str,
        lifecycle: u64,
        _: Arc<dyn SessionResources>,
    ) -> Result<(), String> {
        assert_eq!(session_id, self.child_id);
        assert_eq!(lifecycle, 1);
        Ok(())
    }
}

struct ChildChain;
impl SubagentChainAssembler for ChildChain {
    fn assemble(&self, context: &SubagentChainContext) -> MiddlewareChain {
        assert_eq!(
            context.frozen_claude_md.as_deref(),
            Some("child-only instructions")
        );
        MiddlewareChain::new()
    }
}

struct ProbeTool;
#[async_trait::async_trait]
impl BaseTool for ProbeTool {
    fn name(&self) -> &str {
        "probe"
    }
    fn description(&self) -> &str {
        "child authorized tool"
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{}})
    }
    fn mcp_server_name(&self) -> Option<&str> {
        Some("child-owner")
    }
    async fn invoke(
        &self,
        _: Value,
        _: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        panic!("answer-only child must not dispatch a tool")
    }
}

struct Fixture {
    parent: TestSession,
    resources: Arc<dyn SessionResources>,
    admission: WorkAdmission,
    metadata: ChildResumeMetadata,
    pool: Arc<ChildPool>,
}

async fn mutate(resources: &dyn SessionResources, session_id: &str, action: WorkAction) {
    let receipt = resources
        .apply_work_mutation(&WorkCommand {
            session_id: session_id.into(),
            recipient_lifecycle: 1,
            mutation_id: uuid::Uuid::now_v7().to_string(),
            action,
        })
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted, "{receipt:?}");
}

async fn load(resources: &dyn SessionResources, session_id: &str) -> WorkSnapshot {
    resources
        .load_session_work(&WorkQuery {
            session_id: session_id.into(),
            limit: 10,
        })
        .await
        .unwrap()
}

impl Fixture {
    async fn new(persist_metadata: bool, corrupt_frozen: bool, corrupt_origin: bool) -> Self {
        Self::with_delegation(
            persist_metadata,
            corrupt_frozen,
            corrupt_origin,
            true,
            false,
        )
        .await
    }

    async fn with_delegation(
        persist_metadata: bool,
        corrupt_frozen: bool,
        corrupt_origin: bool,
        bind_pointer: bool,
        current_delegation: bool,
    ) -> Self {
        let parent = TestSession::open().await;
        let resources = parent.resources();
        let snapshot = resources
            .load_session_snapshot(&parent.thread_id())
            .await
            .unwrap();
        let BindingState::Bound(binding) = snapshot.binding else {
            panic!("real workspace binding")
        };
        let child_id = uuid::Uuid::now_v7().to_string();
        let frozen = "{\"version\":1,\"child\":true}";
        resources
            .create_session(&NewSession {
                thread_id: child_id.clone(),
                created_at: peri_time::now_utc_rfc3339(),
                binding,
                meta: NewSessionMeta {
                    title: Some("cold child".into()),
                    cwd: snapshot.meta.cwd,
                    parent_thread_id: Some(parent.thread_id()),
                    hidden: true,
                    cancel_policy: Default::default(),
                    snapshot_at_message_id: None,
                },
                frozen: FrozenSnapshotBytes::new(frozen),
            })
            .await
            .unwrap();
        let metadata = ChildResumeMetadata {
            version: 1,
            child_session_id: child_id.clone(),
            recipient_lifecycle: 1,
            agent_name: "saved-child".into(),
            model_name: "frozen-model".into(),
            direct_initiator_session_id: parent.thread_id(),
            direct_initiator_lifecycle: 1,
            delegation_invocation_id: "original-delegation".into(),
            delegation_task_id: "original-task".into(),
            authorization_ref: "original-auth".into(),
            frozen_digest: format!(
                "{:x}",
                Sha256::digest(if corrupt_frozen {
                    b"wrong".as_slice()
                } else {
                    frozen.as_bytes()
                })
            ),
            tool_ceiling: BTreeSet::from(["probe".into()]),
            tool_origins: BTreeMap::from([(
                "probe".into(),
                Some(
                    if corrupt_origin {
                        "other-owner"
                    } else {
                        "child-owner"
                    }
                    .into(),
                ),
            )]),
            skill_names: vec![],
            max_iterations: 5,
            persona: Some("child-only persona".into()),
            system_prompt: "child-only system".into(),
            claude_md: "child-only instructions".into(),
            claude_local_md: None,
            skill_summary: String::new(),
            date: "2026-10-06".into(),
            language: None,
            section_overrides: BTreeMap::new(),
            disabled_middlewares: BTreeSet::new(),
            built_in_subagents_enabled: false,
        };
        mutate(
            resources.as_ref(),
            &parent.thread_id(),
            WorkAction::BindResourceOwners {
                expected_revision: 0,
                connections_json: "[]".into(),
                authorization_ref: metadata.authorization_ref.clone(),
            },
        )
        .await;
        let arguments = "{}".to_owned();
        let intent = InvocationIntent {
            invocation_id: metadata.delegation_invocation_id.clone(),
            tool_call_id: "original-call".into(),
            tool_name: "subagent".into(),
            arguments_json: arguments.clone(),
            arguments_digest: format!("{:x}", Sha256::digest(arguments.as_bytes())),
            effective_tool_name: "subagent".into(),
            effective_arguments_json: arguments.clone(),
            effective_arguments_digest: format!("{:x}", Sha256::digest(arguments.as_bytes())),
            owner_identity: "original-child-owner".into(),
            scope_id: parent.thread_id(),
            scope_epoch: Some(1),
            authorization_ref: metadata.authorization_ref.clone(),
            recovery_locator: child_id.clone(),
        };
        mutate(
            resources.as_ref(),
            &parent.thread_id(),
            WorkAction::PrepareInvocation {
                expected_revision: load(resources.as_ref(), &parent.thread_id())
                    .await
                    .state
                    .revision,
                intent,
            },
        )
        .await;
        let parent_work = load(resources.as_ref(), &parent.thread_id()).await;
        bind_delegation_task(
            resources.as_ref(),
            &parent.thread_id(),
            &parent_work.state.invocations[&metadata.delegation_invocation_id],
            &metadata.delegation_task_id,
        )
        .await
        .unwrap();
        copy_child_resource_owners(resources.as_ref(), &metadata)
            .await
            .unwrap();
        if persist_metadata {
            persist_child_resume_metadata(resources.as_ref(), &metadata)
                .await
                .unwrap();
        }
        let queue = MessageQueue::new();
        let message = QueuedMessage::prompt(
            MessageSource::UserInput,
            crate::messages::BaseMessage::human("saved child input"),
        );
        if bind_pointer {
            let invocation_id = if current_delegation {
                "current-delegation"
            } else {
                &metadata.delegation_invocation_id
            };
            let task_id = if current_delegation {
                "current-task"
            } else {
                &metadata.delegation_task_id
            };
            if current_delegation {
                let parent_work = load(resources.as_ref(), &parent.thread_id()).await;
                let mut intent = parent_work.state.invocations[&metadata.delegation_invocation_id]
                    .intent
                    .clone();
                intent.invocation_id = invocation_id.into();
                intent.tool_call_id = "current-call".into();
                mutate(
                    resources.as_ref(),
                    &parent.thread_id(),
                    WorkAction::PrepareInvocation {
                        expected_revision: parent_work.state.revision,
                        intent,
                    },
                )
                .await;
            }
            super::super::delegation::publish_work_delegation(
                resources.clone(),
                &child_id,
                1,
                &queue,
                message,
                &parent.thread_id(),
                invocation_id,
                task_id,
                super::super::delegation::DelegationInputMode::FollowUp,
            )
            .await
            .unwrap();
        } else {
            queue.push(message);
            crate::agent::stages::publish_session_inbox(resources.clone(), &child_id, 1, &queue)
                .await
                .unwrap();
        }
        let issued = FixtureAdmission(resources.clone())
            .admit(AdmissionRequest {
                request_id: "cold-sdk-admission".into(),
                snapshot: load(resources.as_ref(), &child_id).await.into(),
                existing_admission: None,
            })
            .await
            .unwrap();
        let AdmissionOutcome::Admitted { admission } = issued else {
            panic!("SDK candidate admission")
        };
        mutate(
            resources.as_ref(),
            &child_id,
            WorkAction::RegisterAdmission {
                admission: admission.clone(),
            },
        )
        .await;
        let pool = Arc::new(ChildPool {
            child_id,
            queue,
            manager: Arc::new(TaskManager::new()),
        });
        Self {
            parent,
            resources,
            admission,
            metadata,
            pool,
        }
    }

    fn runtime(&self, endpoint: &str) -> ColdChildRuntime {
        let model = peri_model::OpenAiModel::new(peri_model::OpenAiConfig::new(
            endpoint.parse().unwrap(),
            "unpersisted-secret",
            "frozen-model",
        ));
        ColdChildRuntime {
            resources: self.resources.clone(),
            llm: Box::new(AgentModelBridge::new(Arc::new(model)).with_system("child-only system")),
            chain_assembler: Arc::new(ChildChain),
            tools: vec![Arc::new(ProbeTool)],
            host: Arc::new(SubagentHost {
                mcp_pool: Some(self.pool.clone()),
                execution_admission_port: Some(Arc::new(FixtureAdmission(self.resources.clone()))),
                frozen_system_prompt: Some(Arc::new("root persona must not leak".into())),
                frozen_claude_md: Some(Arc::new("root instructions must not leak".into())),
                ..Default::default()
            }),
            tool_invocation_resolver: None,
            authorized_ceiling: self.metadata.tool_ceiling.clone(),
            authorization_ref: self.metadata.authorization_ref.clone(),
        }
    }
}

async fn serve(listener: tokio::net::TcpListener) -> Value {
    let (mut socket, _) = listener.accept().await.unwrap();
    let mut request = Vec::new();
    let (body_start, body_length) = loop {
        let mut chunk = [0_u8; 8192];
        let count = socket.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0);
        request.extend_from_slice(&chunk[..count]);
        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..end]);
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            break (end + 4, length);
        }
    };
    while request.len() < body_start + body_length {
        let mut chunk = [0_u8; 8192];
        let count = socket.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0);
        request.extend_from_slice(&chunk[..count]);
    }
    let body = serde_json::from_slice(&request[body_start..body_start + body_length]).unwrap();
    let delta = json!({"id":"cold-reply","model":"frozen-model","choices":[{"index":0,"delta":{"role":"assistant","content":"cold child completed"},"finish_reason":null}]});
    let finish = json!({"id":"cold-reply","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":3,"total_tokens":13}});
    let response = format!("data: {delta}\n\ndata: {finish}\n\ndata: [DONE]\n\n");
    socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
    body
}

async fn run_cold_child(current_delegation: bool) {
    let fixture = Fixture::with_delegation(true, false, false, true, current_delegation).await;
    let parent_before = load(fixture.resources.as_ref(), &fixture.parent.thread_id()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let runtime = fixture.runtime(&format!("http://{}", listener.local_addr().unwrap()));
    let server = tokio::spawn(serve(listener));
    let mut execution = super::super::SessionFactory::prepare_cold_child_execution(
        fixture.admission.clone(),
        runtime,
    )
    .await
    .unwrap();
    assert_eq!(
        execution.session.store().frozen.system_prompt.as_ref(),
        "child-only system"
    );
    assert_eq!(
        execution.session.store().frozen.claude_md.as_ref(),
        "child-only instructions"
    );
    assert!(Arc::ptr_eq(
        execution
            .session
            .subagent_host()
            .unwrap()
            .task_manager
            .as_ref()
            .unwrap(),
        &fixture.pool.manager
    ));
    let context = execution.context.clone();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run_react_loop(context, execution.metadata.max_iterations),
    )
    .await
    .unwrap();
    assert!(matches!(result, LoopResult::Completed), "{result:?}");
    let stopped = load(fixture.resources.as_ref(), &fixture.admission.session_id).await;
    assert!(stopped.control.attempt.is_none());
    assert!(stopped.state.admissions[&fixture.admission.admission_id]
        .settled_receipt
        .is_none());
    let flush = execution
        .session
        .transcript()
        .read()
        .persist_tx_handle()
        .unwrap();
    crate::session::MessageTranscript::flush_via_tx(&flush)
        .await
        .unwrap();
    let mut handles = execution.event_handles.take().unwrap();
    while handles.render_rx.try_recv().is_ok() {}
    while handles.state_rx.try_recv().is_ok() {}
    while handles.observe_rx.try_recv().is_ok() {}
    let settlement = FixtureAdmission(fixture.resources.clone())
        .settle(peri_acp_types::execution_admission::SettlementRequest {
            admission: fixture.admission.clone(),
            proof: peri_acp_types::execution_admission::AttemptStoppedProof::AttemptStopped {
                instance_id: fixture.admission.instance_id.clone(),
                generation_id: fixture.admission.generation_id.clone(),
                execution: fixture.admission.execution.clone(),
                evidence_id: "cold-owner-history-and-forwarder-barriers-drained".into(),
            },
        })
        .await
        .unwrap();
    assert!(matches!(
        settlement,
        peri_acp_types::execution_admission::SettlementOutcome::Applied { .. }
    ));
    let body = server.await.unwrap();
    assert!(body.to_string().contains("saved child input"));
    assert!(body.to_string().contains("child-only system"));
    assert!(!body.to_string().contains("root persona"));
    assert!(!body.to_string().contains("root instructions"));
    let child = load(fixture.resources.as_ref(), &fixture.admission.session_id).await;
    assert_eq!(child.state.admissions.len(), 1);
    assert!(child.state.admissions[&fixture.admission.admission_id]
        .settled_receipt
        .is_some());
    assert!(child.control.attempt.is_none());
    assert!(child.state.resource_owners.contains_key(&1));
    let reopened = fixture.parent.read_only_resources().await;
    let reopened_child = load(reopened.as_ref(), &fixture.admission.session_id).await;
    assert_eq!(reopened_child.state, child.state);
    let reopened_metadata: ChildResumeMetadata =
        serde_json::from_str(&reopened_child.state.child_resume_metadata[&1]).unwrap();
    assert_eq!(
        reopened_metadata.child_session_id,
        fixture.admission.session_id
    );
    assert_eq!(reopened_metadata.delegation_task_id, "original-task");
    assert_eq!(reopened_metadata.authorization_ref, "original-auth");
    assert_eq!(
        reopened_child.state.resource_owners[&1].authorization_ref,
        "original-auth"
    );
    assert_eq!(fixture.pool.manager.active_count(), 0);
    assert!(fixture.pool.manager.list_tasks().is_empty());
    let terminal = peri_acp_types::event::BackgroundTaskResult {
        task_id: execution.delegation_binding.owner_task_id.clone(),
        agent_name: fixture.metadata.agent_name.clone(),
        prompt_summary: "saved child input".into(),
        success: true,
        output: "cold child completed".into(),
        tool_calls_count: 0,
        duration_ms: 1,
        child_thread_id: Some(fixture.admission.session_id.clone()),
        timed_out: false,
        subagent_failure: None,
        shell_output: None,
    };
    let terminal_command = execution.terminal_command(&terminal).await.unwrap();
    assert_eq!(terminal_command.session_id, fixture.parent.thread_id());
    assert_eq!(terminal_command.recipient_lifecycle, 1);
    let WorkAction::PublishTaskSettlement { binding, .. } = &terminal_command.action else {
        panic!("terminal must carry original immutable binding")
    };
    assert_eq!(
        binding,
        &parent_before.state.task_bindings[&execution.delegation_binding.invocation_id]
    );
    let parent_after = load(fixture.resources.as_ref(), &fixture.parent.thread_id()).await;
    assert_eq!(parent_after.state, parent_before.state);
    assert_eq!(
        parent_after.state.task_bindings.len(),
        if current_delegation { 2 } else { 1 }
    );
    assert_eq!(
        parent_after.state.task_bindings[&fixture.metadata.delegation_invocation_id].owner_task_id,
        "original-task"
    );
    if current_delegation {
        assert_eq!(
            execution.metadata.delegation_invocation_id,
            "original-delegation"
        );
        assert_eq!(
            execution.delegation_binding.invocation_id,
            "current-delegation"
        );
        assert_eq!(binding.owner_task_id, "current-task");
        let receipt = fixture
            .resources
            .apply_work_mutation(&terminal_command)
            .await
            .unwrap();
        assert_eq!(receipt.decision, WorkDecision::Accepted);
        let routed = load(fixture.resources.as_ref(), &fixture.parent.thread_id()).await;
        assert_eq!(
            routed.state.task_bindings["original-delegation"],
            parent_before.state.task_bindings["original-delegation"]
        );
        assert_eq!(
            routed.state.invocations["original-delegation"],
            parent_before.state.invocations["original-delegation"]
        );
        assert_eq!(routed.state.deliveries.len(), 1);
        assert!(serde_json::to_string(&routed.state.deliveries)
            .unwrap()
            .contains("current-task"));
    }
    let persisted = fixture
        .resources
        .load_session_snapshot(&fixture.admission.session_id)
        .await
        .unwrap();
    assert!(persisted.payloads.iter().any(
        |payload| matches!(payload, peri_acp_types::store::PersistedPayload::Message(message)
            if message.content().contains("cold child completed"))
    ));
    execution.session.queue().push(QueuedMessage::info(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("typed pool queue identity probe"),
    ));
    assert!(fixture
        .pool
        .queue
        .drain_all()
        .iter()
        .any(|message| message.message().is_some_and(|message| message
            .content()
            .contains("typed pool queue identity probe"))));
}

#[tokio::test]
async fn cold_child_runs_same_durable_rcra_without_parent_runtime_or_secondary_task() {
    run_cold_child(false).await;
}

#[tokio::test]
async fn current_work_delegation_routes_terminal_to_d2_without_changing_original_d1() {
    run_cold_child(true).await;
}

async fn assert_blocked(fixture: Fixture, runtime: ColdChildRuntime, expected: &str) {
    let before = load(fixture.resources.as_ref(), &fixture.admission.session_id).await;
    let blocked = match super::super::SessionFactory::prepare_cold_child_execution(
        fixture.admission.clone(),
        runtime,
    )
    .await
    {
        Ok(_) => panic!("unsafe reconstruction must be blocked"),
        Err(error) => error,
    };
    assert!(blocked.reason.contains(expected), "{blocked}");
    assert!(blocked
        .to_string()
        .starts_with("cold child execution blocked:"));
    assert_eq!(
        load(fixture.resources.as_ref(), &fixture.admission.session_id)
            .await
            .state,
        before.state
    );
    assert!(fixture.pool.manager.list_tasks().is_empty());
}

#[tokio::test]
async fn missing_authorization_blocks_cold_child_before_model_or_task_creation() {
    let fixture = Fixture::new(true, false, false).await;
    let mut runtime = fixture.runtime("http://127.0.0.1:1");
    runtime.authorization_ref.clear();
    assert_blocked(fixture, runtime, "authorization ceiling unavailable").await;
}

#[tokio::test]
async fn changed_tool_origin_blocks_cold_child_before_model_or_task_creation() {
    let fixture = Fixture::new(true, false, true).await;
    let runtime = fixture.runtime("http://127.0.0.1:1");
    assert_blocked(fixture, runtime, "tool origin cannot be reconstructed").await;
}

#[tokio::test]
async fn inconsistent_frozen_snapshot_blocks_cold_child_before_model_or_task_creation() {
    let fixture = Fixture::new(true, true, false).await;
    let runtime = fixture.runtime("http://127.0.0.1:1");
    assert_blocked(
        fixture,
        runtime,
        "frozen data or authorization ceiling unavailable",
    )
    .await;
}

#[tokio::test]
async fn missing_resume_metadata_blocks_cold_child_before_model_or_task_creation() {
    let fixture = Fixture::new(false, false, false).await;
    let runtime = fixture.runtime("http://127.0.0.1:1");
    assert_blocked(fixture, runtime, "authorization metadata missing").await;
}

#[tokio::test]
async fn missing_current_work_delegation_blocks_without_metadata_fallback() {
    let fixture = Fixture::with_delegation(true, false, false, false, false).await;
    let child = load(fixture.resources.as_ref(), &fixture.admission.session_id).await;
    assert!(child.state.child_resume_metadata.contains_key(&1));
    assert!(!child
        .state
        .work_delegations
        .contains_key(&fixture.admission.work_id));
    let parent = load(fixture.resources.as_ref(), &fixture.parent.thread_id()).await;
    assert!(parent
        .state
        .task_bindings
        .contains_key("original-delegation"));
    let runtime = fixture.runtime("http://127.0.0.1:1");
    assert_blocked(
        fixture,
        runtime,
        "current immutable work delegation reference missing",
    )
    .await;
}
