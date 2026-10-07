use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use futures::TryStreamExt;
use peri_acp_types::{
    execution_admission::*,
    identity::{AttemptId, EventDeliveryClass},
    runtime::UnstampedEvent,
    session::{MessageQueue, SessionAccessPort, SessionInbox, TurnId},
    session_resources::{
        work::*, ControlAttempt, FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources,
    },
    workspace::SessionBinding,
};
use peri_agent::session::user_input_mailbox::UserInputMailbox;
use peri_model::{
    Model, ModelCapabilities, ModelError, ModelRequest, ModelResult, ModelStream,
    PreparedModelCall, PreparedModelRequest, ProtocolErrorKind, ProviderProtocol,
};
use tokio_util::sync::CancellationToken;

use crate::session::executor::{PromptResult, SessionContext, TurnInput};

pub(in crate::host) async fn run_session_loop(
    mut ctx: SessionContext,
    mut turn: TurnInput,
) -> PromptResult {
    let mut input_guard = if !turn.continuation && !turn.content.is_empty() {
        ctx.user_input_mailbox.as_ref().map(|mailbox| {
            let ticket = mailbox
                .attach_external_attempt(ctx.cancel.clone(), false)
                .expect("fixture initial prompt must own its execution attachment");
            crate::host::user_input::InputAttemptGuard::new(mailbox.clone(), ticket)
        })
    } else {
        None
    };
    let observed = Arc::new(std::sync::OnceLock::new());
    let publish = ctx.sdk_admission_observed.clone();
    let capture = observed.clone();
    ctx.sdk_admission_observed = Some(Arc::new(move |admission| {
        let _ = capture.set(admission.clone());
        if let Some(publish) = &publish {
            publish(admission);
        }
    }));
    let stage = turn.stage_build.clone();
    let observer = ctx.sdk_admission_observed.clone();
    turn.stage_build = Arc::new(move |request| {
        let (mut output, cache) = stage(request)?;
        output.context = output
            .context
            .with_sdk_admission_observed(observer.clone().unwrap());
        Ok((output, cache))
    });
    let resources = ctx.session_resources.clone();
    let sdk = ctx.execution_admission_port.clone();
    let result = crate::session::executor::run_session_loop(ctx, turn).await;
    if let Some(guard) = &mut input_guard {
        guard.finish(&result);
    }
    if !result.persistence_inconsistent {
        if let Some(admission) = observed.get() {
            let snapshot = resources
                .as_ref()
                .unwrap()
                .inspect_work(&WorkQuery::new(
                    &admission.session_id,
                    WorkSelector::Availability,
                ))
                .await
                .expect("fixture root must inspect durable work before settlement");
            if matches!(snapshot.page, WorkPage::Availability(ref availability) if availability.pending)
            {
                return result;
            }
            let evidence_id = super::super::execution::finish_admission(
                resources.as_ref().unwrap().as_ref(),
                admission,
            )
            .await
            .expect("fixture root must durably finish its exact returned execution");
            let outcome = sdk
                .unwrap()
                .settle(SettlementRequest {
                    admission: admission.clone(),
                    proof: AttemptStoppedProof::AttemptStopped {
                        instance_id: admission.instance_id.clone(),
                        generation_id: admission.generation_id.clone(),
                        execution: admission.execution.clone(),
                        evidence_id,
                    },
                })
                .await
                .expect("fixture SDK must acknowledge the exact stopped execution");
            assert!(
                matches!(outcome, SettlementOutcome::Applied { receipt } if receipt.admission == *admission)
            );
        }
    }
    result
}

pub(in crate::host) async fn new_resources(
    session_id: &str,
) -> (Arc<dyn SessionResources>, Arc<tempfile::TempDir>) {
    let directory = Arc::new(tempfile::tempdir().unwrap());
    let resources: Arc<dyn SessionResources> = Arc::new(
        peri_resources::sessions::SessionResourcesImpl::open(directory.path().join("execution.db"))
            .await
            .unwrap(),
    );
    let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
    resources
        .create_session(&NewSession {
            thread_id: session_id.into(),
            created_at: peri_time::now_utc_rfc3339(),
            meta: NewSessionMeta {
                title: None,
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: SessionBinding::from_workspace(&workspace),
            frozen: FrozenSnapshotBytes::new(r#"{"v":1}"#),
        })
        .await
        .unwrap();
    (resources, directory)
}

pub(in crate::host) fn bind_execution(
    ctx: &mut SessionContext,
    directory: Option<Arc<tempfile::TempDir>>,
) {
    let resources = ctx
        .session_resources
        .as_ref()
        .expect("fixture requires real Store")
        .clone();
    if ctx.session_access.is_none() {
        let inbox = Arc::new(SessionInbox::new(Arc::new(MessageQueue::new())));
        ctx.session_access = Some(Arc::new(MockSessionAccess {
            session_id: ctx.session_id.clone(),
            inbox: inbox.clone(),
            tasks: Arc::new(peri_agent::agent::async_tasks::TaskManager::new()),
            idle: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            terminal: peri_agent::agent::async_tasks::durable_task_terminal_delivery(
                resources.clone(),
                ctx.session_id.clone(),
                ctx.recipient_lifecycle,
                inbox.queue().clone(),
            ),
        }));
    }
    let inbox = ctx
        .session_access
        .as_ref()
        .and_then(|access| access.session_inbox(&ctx.session_id))
        .expect("fixture session access must own its inbox");
    let mailbox = UserInputMailbox::new_durable(
        ctx.session_id.clone(),
        inbox,
        Arc::new(|_| {}),
        resources.clone(),
        ctx.recipient_lifecycle,
    );
    ctx.execution_admission_port = Some(Arc::new(MockSdkAdmission {
        session_id: ctx.session_id.clone(),
        resources,
        _directory: directory,
        admissions: tokio::sync::Mutex::new(BTreeMap::new()),
    }));
    ctx.user_input_mailbox = Some(mailbox.clone());
    let publisher = ctx.event_publisher.clone();
    let session_id = ctx.session_id.clone();
    ctx.sdk_run_started = Some(Arc::new(move |admission| {
        let mailbox = mailbox.clone();
        let publisher = publisher.clone();
        let session_id = session_id.clone();
        Box::pin(async move {
            let ticket = mailbox
                .observe_sdk_run(&admission)
                .await
                .map_err(|error| error.to_string())?;
            let event = mailbox
                .run_started_event(&ticket)
                .ok_or("fixture SDK run has no bound cancellation token")?;
            let source = UnstampedEvent::new(
                event.turn_id().to_string(),
                event.agent_id().to_string(),
                None,
                EventDeliveryClass::Critical,
            );
            let event = peri_acp_types::event_v2::state_event_to_executor(event)
                .ok_or("fixture SDK run-started event conversion failed")?;
            publisher.publish_event(&session_id, &source, event);
            Ok(())
        })
    }));
}

struct MockSessionAccess {
    session_id: String,
    inbox: Arc<SessionInbox>,
    tasks: Arc<dyn peri_acp_types::tasks::TaskManager>,
    idle: Arc<std::sync::atomic::AtomicBool>,
    terminal: Arc<dyn peri_acp_types::tasks::TaskTerminalDelivery>,
}

impl SessionAccessPort for MockSessionAccess {
    fn task_terminal_delivery(
        &self,
        session_id: &str,
    ) -> Option<Arc<dyn peri_acp_types::tasks::TaskTerminalDelivery>> {
        (session_id == self.session_id).then(|| self.terminal.clone())
    }
    fn v2_message_queue(&self, session_id: &str) -> Option<MessageQueue> {
        (session_id == self.session_id).then(|| self.inbox.queue().clone())
    }
    fn session_inbox(&self, session_id: &str) -> Option<Arc<SessionInbox>> {
        (session_id == self.session_id).then(|| self.inbox.clone())
    }
    fn idle_suspended_flag(&self, session_id: &str) -> Option<Arc<std::sync::atomic::AtomicBool>> {
        (session_id == self.session_id).then(|| self.idle.clone())
    }
    fn task_manager(
        &self,
        session_id: &str,
    ) -> Option<Arc<dyn peri_acp_types::tasks::TaskManager>> {
        (session_id == self.session_id).then(|| self.tasks.clone())
    }
    fn goal_controller(
        &self,
        _session_id: &str,
    ) -> Option<Arc<dyn peri_acp_types::goal::GoalController>> {
        None
    }
    fn register_runtime(
        &self,
        _session_id: &str,
    ) -> Option<peri_acp_types::frozen::RegisterRuntimeFn> {
        None
    }
    fn deregister_runtime(
        &self,
        _session_id: &str,
    ) -> Option<peri_acp_types::frozen::DeregisterRuntimeFn> {
        None
    }
    fn cancel_cascade_children(&self, _session_id: &str) {}
    fn cron_bridge_for(&self, _session_id: &str) -> bool {
        false
    }
}

struct MockSdkAdmission {
    session_id: String,
    resources: Arc<dyn SessionResources>,
    _directory: Option<Arc<tempfile::TempDir>>,
    admissions: tokio::sync::Mutex<BTreeMap<String, WorkAdmission>>,
}

fn protocol(error: impl ToString) -> ExecutionAdmissionError {
    ExecutionAdmissionError::Protocol(error.to_string())
}

#[async_trait]
impl ExecutionAdmissionPort for MockSdkAdmission {
    async fn admit(
        &self,
        request: AdmissionRequest,
    ) -> Result<AdmissionOutcome, ExecutionAdmissionError> {
        let mut admissions = self.admissions.lock().await;
        if request.snapshot.session_id != self.session_id {
            return Err(protocol("fixture SDK cannot retarget a different session"));
        }
        if let Some(admission) = admissions.get(&request.request_id) {
            return Ok(AdmissionOutcome::Admitted {
                admission: admission.clone(),
            });
        }
        let snapshot = self
            .resources
            .inspect_work(&WorkQuery {
                session_id: self.session_id.clone(),
                selector: WorkSelector::Availability,
                limit: 64,
                cursor: None,
            })
            .await
            .map_err(protocol)?;
        let availability = crate::host::work_query::availability(&snapshot).map_err(protocol)?;
        if availability.blocked || availability.pending {
            return Ok(AdmissionOutcome::Blocked {
                reason: "fixture has unresolved durable Work".into(),
            });
        }
        if AdmissionSnapshot::from(&snapshot) != request.snapshot {
            return Err(protocol("fixture SDK received stale Work facts"));
        }
        let candidate = availability
            .candidates
            .first()
            .ok_or_else(|| protocol("fixture SDK requires an actual durable candidate"))?;
        let admission = WorkAdmission {
            session_id: self.session_id.clone(),
            admission_id: format!("mock-sdk:{}", request.request_id),
            instance_id: "explicit-test-sdk".into(),
            generation_id: uuid::Uuid::now_v7().to_string(),
            lifecycle: snapshot.control.lifecycle,
            control_generation: snapshot.control.control_generation,
            work_id: candidate.work_id.clone(),
            work_revision: candidate.work_revision,
            execution: ControlAttempt {
                turn_id: TurnId::new(),
                attempt_id: AttemptId::new(),
            },
        };
        admissions.insert(request.request_id, admission.clone());
        Ok(AdmissionOutcome::Admitted { admission })
    }

    async fn entered(
        &self,
        request: EntryRequest,
    ) -> Result<EntryOutcome, ExecutionAdmissionError> {
        let snapshot = self
            .resources
            .inspect_work(&WorkQuery {
                session_id: request.admission.session_id.clone(),
                selector: WorkSelector::Admission {
                    admission_id: request.admission.admission_id.clone(),
                },
                limit: 1,
                cursor: None,
            })
            .await
            .map_err(protocol)?;
        let valid = crate::host::work_query::admission(&snapshot, &request.admission.admission_id)
            .map_err(protocol)?
            .is_some_and(|record| {
                record.admission == request.admission
                    && record.leaving_evidence_id.is_none()
                    && record.entering_mutation_id == request.entry_evidence_id
            })
            && snapshot.control.lifecycle == request.admission.lifecycle
            && snapshot.control.control_generation == request.admission.control_generation
            && snapshot.control.attempt.as_ref() == Some(&request.admission.execution);
        if !valid {
            return Err(protocol(
                "fixture SDK entered requires exact durable Register receipt",
            ));
        }
        Ok(EntryOutcome::Applied {
            receipt: EntryReceipt {
                admission: request.admission,
                entry_evidence_id: request.entry_evidence_id,
            },
        })
    }

    async fn settle(
        &self,
        request: SettlementRequest,
    ) -> Result<SettlementOutcome, ExecutionAdmissionError> {
        let snapshot = self
            .resources
            .inspect_work(&WorkQuery {
                session_id: request.admission.session_id.clone(),
                selector: WorkSelector::Admission {
                    admission_id: request.admission.admission_id.clone(),
                },
                limit: 1,
                cursor: None,
            })
            .await
            .map_err(protocol)?;
        if !request.proof.validates(&request.admission)
            || snapshot.control.attempt.is_some()
            || !crate::host::work_query::admission(&snapshot, &request.admission.admission_id)
                .map_err(protocol)?
                .is_some_and(|record| {
                    record.admission == request.admission && record.leaving_evidence_id.is_some()
                })
        {
            return Err(protocol(
                "fixture SDK settle requires exact stopped proof and durable terminal control",
            ));
        }
        Ok(SettlementOutcome::Applied {
            receipt: SettlementReceipt {
                admission: request.admission,
                evidence_id: request.proof.evidence_id().into(),
            },
        })
    }
}

pub(in crate::host) fn wrap_model(inner: Arc<dyn Model>) -> Arc<dyn Model> {
    Arc::new(MockPreparedModel { inner })
}

pub(in crate::host) async fn publish_continuation(ctx: &SessionContext) {
    use peri_acp_types::system_reminder::*;
    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Lifecycle,
            source: ReminderSource("execution_fixture".into()),
            kind: "continuation".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Required,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: "Explicit durable continuation fixture event".into(),
            summary: None,
            metadata: serde_json::json!({}),
        })
        .unwrap();
    let event_id = uuid::Uuid::now_v7().to_string();
    let receipt = ctx
        .session_resources
        .as_ref()
        .unwrap()
        .apply_work_mutation(&WorkCommand {
            session_id: ctx.session_id.clone(),
            recipient_lifecycle: ctx.recipient_lifecycle,
            mutation_id: format!("fixture-continuation:{event_id}"),
            action: WorkAction::PublishDelivery {
                delivery: PublishDelivery {
                    delivery_id: event_id.clone(),
                    purpose: DeliveryPurpose::Continuation,
                    policy: peri_acp_types::session::MessagePolicy::ensure_processing(),
                    event: WorkEvent {
                        producer_namespace: "execution-fixture".into(),
                        event_id,
                        event_kind: "continuation".into(),
                        causation_id: None,
                        content: crate::host::work_query::test_payload(
                            ctx.session_resources.as_ref().unwrap().as_ref(),
                            &ctx.session_id,
                            &peri_acp_types::store::PersistedPayload::SystemReminder {
                                id: peri_acp_types::messages::MessageId::new(),
                                reminder,
                            },
                        )
                        .await,
                    },
                },
            },
        })
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
}

pub(in crate::host) async fn seed_history(
    ctx: &SessionContext,
    content: &str,
    response: &str,
) -> Vec<peri_acp_types::messages::BaseMessage> {
    let mut seed = ctx.clone();
    seed.cancel = CancellationToken::new();
    let response = response.to_owned();
    seed.primary_llm_factory = Some(Arc::new(move || {
        Arc::new(MockHistoryModel(response.clone()))
    }));
    bind_execution(&mut seed, None);
    let turn = super::make_turn_input(
        Arc::new(super::MockEventSink::new()),
        peri_acp_types::messages::MessageContent::text(content),
        false,
        vec![],
        super::make_stage_build(&seed),
    );
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        run_session_loop(seed, turn),
    )
    .await
    .expect("fixture history seed must finish");
    assert!(
        result.ok,
        "fixture history requires actual completed Work: {:?}",
        result.failure
    );
    assert!(!result.persistence_inconsistent);
    result.messages
}

struct MockHistoryModel(String);

#[async_trait]
impl Model for MockHistoryModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_streaming: true,
            ..Default::default()
        }
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        let response = peri_model::ModelResponse::new(
            peri_model::ModelMessage::assistant_text(&self.0),
            peri_model::StopReason::EndTurn,
            None,
            None,
        )?;
        Ok(ModelStream::with_parent_cancellation(
            futures::stream::iter(vec![Ok(peri_model::ModelStreamEvent::Completed(response))]),
            cancellation,
        ))
    }
}

struct MockPreparedModel {
    inner: Arc<dyn Model>,
}

#[async_trait]
impl Model for MockPreparedModel {
    fn capabilities(&self) -> ModelCapabilities {
        self.inner.capabilities()
    }

    fn prepare_stream(&self, request: ModelRequest) -> ModelResult<PreparedModelCall> {
        let body = serde_json::to_value(&request)
            .map_err(|_| ModelError::protocol(ProtocolErrorKind::Provider))?;
        let checkpoint = serde_json::json!({
            "provider": "explicit-scripted-test", "model": "scripted-test-model",
            "endpoint": "https://scripted.test.invalid/stream", "credentialRef": "fixture:no-credential", "body": body,
        });
        let inner = self.inner.clone();
        Ok(PreparedModelCall::new(checkpoint, move |cancellation| {
            let stream_cancellation = cancellation.clone();
            let events =
                futures::stream::once(
                    async move { inner.stream(request, stream_cancellation).await },
                )
                .try_flatten();
            Ok(ModelStream::with_parent_cancellation(events, cancellation))
        }))
    }

    fn prepare_request(&self, request: &ModelRequest) -> ModelResult<PreparedModelRequest> {
        PreparedModelRequest::observe(
            ProviderProtocol::Other {
                value: "explicit-scripted-test".into(),
            },
            "scripted-test-model",
            "https://scripted.test.invalid/stream".parse().unwrap(),
            serde_json::to_value(request)
                .map_err(|_| ModelError::protocol(ProtocolErrorKind::Provider))?,
            BTreeMap::new(),
        )
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        self.inner.stream(request, cancellation).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use peri_acp_types::{event::ExecutorEvent, session::EnqueueUserInputRequest};
    use peri_model::{ModelMessage, ModelResponse, ModelStreamEvent, StopReason};
    use std::sync::Mutex;

    struct MockCaptureModel(Arc<Mutex<Vec<ModelRequest>>>);

    #[async_trait]
    impl Model for MockCaptureModel {
        fn capabilities(&self) -> ModelCapabilities {
            ModelCapabilities {
                supports_streaming: true,
                ..Default::default()
            }
        }

        async fn stream(
            &self,
            request: ModelRequest,
            cancellation: CancellationToken,
        ) -> ModelResult<ModelStream> {
            self.0.lock().unwrap().push(request);
            let response = ModelResponse::new(
                ModelMessage::assistant_text("done"),
                StopReason::EndTurn,
                None,
                None,
            )?;
            Ok(ModelStream::with_parent_cancellation(
                futures::stream::iter(vec![Ok(ModelStreamEvent::Completed(response))]),
                cancellation,
            ))
        }
    }

    #[tokio::test]
    async fn scripted_preparation_freezes_complete_request_and_streams_same_owned_body() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = wrap_model(Arc::new(MockCaptureModel(requests.clone())));
        let mut request = ModelRequest::new(vec![
            ModelMessage::system_text("full frozen system"),
            ModelMessage::user_text("multimodal prompt boundary".repeat(2000)),
        ]);
        request.max_tokens = Some(1234);
        request.temperature = Some(0.25);
        request.session_id = Some("fixture-request-session".into());
        request.tools.push(serde_json::from_value(serde_json::json!({
            "name": "frozen_tool", "description": "complete frozen schema",
            "input_schema": {"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}
        })).unwrap());
        request.messages.push(serde_json::from_value(serde_json::json!({
            "role":"user", "content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"frozen-image-bytes"}}]
        })).unwrap());
        let frozen = request.clone();
        let prepared = model.prepare_stream(request.clone()).unwrap();
        assert_eq!(
            prepared.checkpoint()["body"],
            serde_json::to_value(&request).unwrap()
        );
        assert!(requests.lock().unwrap().is_empty());
        request.messages.clear();
        request.tools.clear();
        let mut stream = prepared.start(CancellationToken::new()).unwrap();
        assert!(stream.next().await.unwrap().is_ok());
        assert_eq!(*requests.lock().unwrap(), vec![frozen]);
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(model
            .prepare_stream(request)
            .unwrap()
            .start(cancelled)
            .is_err());
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn explicit_sdk_requires_durable_register_before_entered_and_started() {
        let ctx = super::super::make_session_context("fixture-sdk-proof").await;
        let mailbox = ctx.user_input_mailbox.as_ref().unwrap();
        mailbox
            .enqueue_durable(&EnqueueUserInputRequest {
                session_id: ctx.session_id.clone(),
                generation: mailbox.generation().into(),
                command_id: "fixture-publication".into(),
                input_id: uuid::Uuid::now_v7().to_string(),
                content: peri_acp_types::messages::MessageContent::text("durable required input"),
                original_draft: "durable required input".into(),
            })
            .await
            .unwrap();
        let resources = ctx.session_resources.as_ref().unwrap();
        let query = WorkQuery {
            session_id: ctx.session_id.clone(),
            selector: WorkSelector::Availability,
            limit: 64,
            cursor: None,
        };
        let snapshot = resources.inspect_work(&query).await.unwrap();
        let sdk = ctx.execution_admission_port.as_ref().unwrap();
        let AdmissionOutcome::Admitted { admission } = sdk
            .admit(AdmissionRequest {
                request_id: "fixture-sdk-request".into(),
                snapshot: snapshot.into(),
                existing_admission: None,
            })
            .await
            .unwrap()
        else {
            panic!("actual Store candidate must be admitted by mock SDK")
        };
        assert!(resources
            .inspect_work(&query)
            .await
            .unwrap()
            .head
            .current_admission_id
            .is_none());
        assert!(sdk
            .entered(EntryRequest {
                admission: admission.clone(),
                entry_evidence_id: "unregistered".into()
            })
            .await
            .is_err());
        let receipt = resources
            .apply_work_mutation(&WorkCommand {
                session_id: ctx.session_id.clone(),
                recipient_lifecycle: admission.lifecycle,
                mutation_id: "fixture-register-sdk".into(),
                action: WorkAction::RegisterAdmission {
                    admission: admission.clone(),
                },
            })
            .await
            .unwrap();
        assert_eq!(receipt.decision, WorkDecision::Accepted);
        assert!(matches!(
            sdk.entered(EntryRequest {
                admission: admission.clone(),
                entry_evidence_id: receipt.mutation_id
            })
            .await
            .unwrap(),
            EntryOutcome::Applied { .. }
        ));
        let ticket = mailbox.observe_sdk_run(&admission).await.unwrap();
        assert!(mailbox.attach_sdk_attempt(&admission, CancellationToken::new()));
        let mut subscriber = (ctx.subscribe)();
        (ctx.sdk_run_started.as_ref().unwrap())(admission.clone())
            .await
            .unwrap();
        let event = subscriber.try_recv().unwrap().unwrap();
        assert!(
            matches!(event.event, Some(ExecutorEvent::UserInputRunStarted { request_id, .. }) if request_id == ticket.id)
        );
        assert!(mailbox.reserve_run().is_none());
        assert_eq!(
            usize::from(
                resources
                    .inspect_work(&query)
                    .await
                    .unwrap()
                    .head
                    .current_admission_id
                    .is_some()
            ),
            1
        );
    }
}
