use super::work_test_support::*;
use super::*;
use crate::agent::react::Reasoning;
use crate::session::test_resources::TestSession;
use crate::session::{FrozenContext, Session};
use peri_acp_types::session_resources::work::*;
use peri_acp_types::session_resources::{ControlAction, ControlCommand};
use std::sync::atomic::{AtomicUsize, Ordering};

struct ProbeTool(Arc<AtomicUsize>);

#[tokio::test]
async fn admitted_batch_claim_precedes_late_inbox_publication() {
    let (model, _listener) = native_model().await;
    let fixture = fixture(Arc::new(model), Vec::new()).await;
    let late = BaseMessage::human("late notification");
    let late_id = late.id();
    fixture
        .context
        .session
        .queue
        .push(crate::session::QueuedMessage::prompt(
            crate::session::MessageSource::SystemInjected,
            late,
        ));
    let received = receive::run_receive(ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    assert_eq!(received.consumed_count, 1);
    assert_eq!(received.input_message_ids.len(), 1);
    assert!(!received.input_message_ids.contains(&late_id));
    let processing = saved_processing(&fixture).await;
    assert_eq!(processing.processing_id, fixture.admission.work_id);
    assert_eq!(processing.delivery_count, 1);
    let inspection = fixture
        .bound
        .resources()
        .inspect_work(&WorkQuery::new(
            fixture.bound.thread_id(),
            WorkSelector::Delivery {
                delivery_id: late_id.as_uuid().to_string(),
            },
        ))
        .await
        .unwrap();
    let WorkPage::Deliveries(deliveries) = inspection.page else {
        panic!("expected late delivery");
    };
    assert_eq!(deliveries.len(), 1);
    assert!(deliveries[0].processing_id.is_none());
}

#[tokio::test]
async fn admitted_batch_claim_retains_registered_members_after_external_publication() {
    let (model, _listener) = native_model().await;
    let fixture = fixture(Arc::new(model), Vec::new()).await;
    let resources = fixture.bound.resources();
    let late = BaseMessage::human("external publication after admission");
    let late_id = late.id().as_uuid().to_string();
    let content = crate::agent::stages::prepare_work_payload(
        resources.as_ref(),
        &fixture.bound.thread_id(),
        &peri_acp_types::store::PersistedPayload::Message(late),
    )
    .await
    .unwrap();
    let receipt = resources
        .apply_work_mutation(&WorkCommand {
            session_id: fixture.bound.thread_id(),
            recipient_lifecycle: fixture.admission.lifecycle,
            mutation_id: format!("external:{late_id}"),
            action: WorkAction::PublishDelivery {
                delivery: PublishDelivery {
                    delivery_id: late_id.clone(),
                    event: WorkEvent {
                        producer_namespace: "external-producer".into(),
                        event_id: late_id.clone(),
                        event_kind: "agentMessage".into(),
                        causation_id: None,
                        content,
                    },
                    purpose: DeliveryPurpose::UserInput,
                    policy: crate::session::MessagePolicy::ensure_processing(),
                },
            },
        })
        .await
        .unwrap();
    assert_eq!(receipt.decision, WorkDecision::Accepted);
    let received = receive::run_receive(ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    assert_eq!(received.consumed_count, 1);
    assert_eq!(received.input_message_ids.len(), 1);
    assert_eq!(saved_processing(&fixture).await.delivery_count, 1);
    let inspection = resources
        .inspect_work(&WorkQuery::new(
            fixture.bound.thread_id(),
            WorkSelector::Delivery {
                delivery_id: late_id,
            },
        ))
        .await
        .unwrap();
    let WorkPage::Deliveries(deliveries) = inspection.page else {
        panic!("expected external delivery");
    };
    assert_eq!(deliveries.len(), 1);
    assert!(deliveries[0].processing_id.is_none());
    assert_eq!(deliveries[0].obligation, ObligationStatus::Pending);
}

#[async_trait::async_trait]
impl BaseTool for ProbeTool {
    fn name(&self) -> &str {
        "probe"
    }
    fn description(&self) -> &str {
        "assert durable barriers before effect"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{"value":{"type":"string"}}})
    }
    async fn invoke(
        &self,
        input: serde_json::Value,
        ctx: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        assert_eq!(input, serde_json::json!({"value":"exact"}));
        let resources = ctx.session_resources.unwrap();
        let intent = ctx.invocation_intent.unwrap();
        assert_eq!(
            ctx.invocation_id.as_deref(),
            Some(intent.invocation_id.as_str())
        );
        let snapshot = resources
            .inspect_work(&WorkQuery::new(ctx.session_id.unwrap(), WorkSelector::Head))
            .await?;
        let effect = crate::session::work_access::effect(
            resources.as_ref(),
            &snapshot.session_id,
            &intent.invocation_id,
        )
        .await?;
        assert_eq!(effect.status, InvocationStatus::DispatchAccepted);
        let processing_id = effect.processing_id.as_deref().unwrap();
        let processing = crate::session::work_access::processing(
            resources.as_ref(),
            &snapshot.session_id,
            processing_id,
        )
        .await?;
        assert!(processing.response.is_some());
        let inspection = resources
            .inspect_work(&WorkQuery::new(
                &snapshot.session_id,
                WorkSelector::ProcessingDeliveries {
                    processing_id: processing_id.into(),
                },
            ))
            .await?;
        let WorkPage::Deliveries(deliveries) = inspection.page else {
            panic!("expected deliveries")
        };
        assert!(deliveries
            .iter()
            .all(|delivery| delivery.obligation == ObligationStatus::Satisfied));
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok("confirmed probe output".into())
    }
}

#[tokio::test]
async fn production_full_store_and_native_http_checkpoint_before_model_send_and_single_projection()
{
    let (model, listener) = native_model().await;
    let reads = Arc::new(AtomicUsize::new(0));
    let reads_for_model = Arc::clone(&reads);
    let model = model.with_system_contribution_provider(Arc::new(move || {
        let sequence = reads_for_model.fetch_add(1, Ordering::SeqCst);
        format!("dynamic system {sequence}")
    }));
    let fixture = fixture(Arc::new(model), Vec::new()).await;
    let server = serve(
        listener,
        fixture.bound.resources(),
        fixture.bound.thread_id(),
        false,
    );
    let received = receive::run_receive(ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    assert_eq!(received.wake_up_count, 1);
    let output = reason::run_reason(ReasonInput {
        context: fixture.context.clone(),
        has_tool_calls: false,
    })
    .await
    .unwrap();
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    let body = server.await.unwrap();
    assert!(body.to_string().contains("dynamic system 0"));
    assert_eq!(
        saved_delivery(&fixture).await.obligation,
        ObligationStatus::Satisfied
    );
    let before = fixture
        .bound
        .resources
        .load_session_history(&fixture.bound.thread_id())
        .await
        .unwrap();
    act::run_act(ActInput {
        context: fixture.context.clone(),
        reasoning: output.reasoning,
        catalog: output.catalog,
    })
    .await
    .unwrap();
    let writer = fixture
        .context
        .session
        .transcript
        .read()
        .persist_tx_handle()
        .unwrap();
    MessageTranscript::flush_via_tx(&writer).await.unwrap();
    let after = fixture
        .bound
        .resources
        .load_session_history(&fixture.bound.thread_id())
        .await
        .unwrap();
    assert_eq!(before.len(), 2);
    assert_eq!(after.len(), before.len());
    assert_eq!(fixture.context.session.transcript.read().entries().len(), 2);
    let received = receive::run_receive(ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    assert_eq!(received.wake_up_count, 0);
    assert_eq!(received.consumed_count, 0);
    assert!(received.input_message_ids.is_empty());
}

#[tokio::test]
async fn production_commit_response_and_begin_dispatch_precede_tool_effect_and_results_precede_reason(
) {
    let (model, listener) = native_model().await;
    let effects = Arc::new(AtomicUsize::new(0));
    let fixture = fixture(
        Arc::new(model),
        vec![Arc::new(ProbeTool(Arc::clone(&effects)))],
    )
    .await;
    let server = serve(
        listener,
        fixture.bound.resources(),
        fixture.bound.thread_id(),
        true,
    );
    receive::run_receive(ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    let output = reason::run_reason(ReasonInput {
        context: fixture.context.clone(),
        has_tool_calls: false,
    })
    .await
    .unwrap();
    server.await.unwrap();
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    let result = act::run_act(ActInput {
        context: fixture.context.clone(),
        reasoning: output.reasoning,
        catalog: output.catalog,
    })
    .await
    .unwrap();
    assert!(result.has_tool_calls);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let snapshot = fixture
        .bound
        .resources
        .inspect_work(&WorkQuery::new(
            fixture.bound.thread_id(),
            WorkSelector::Head,
        ))
        .await
        .unwrap();
    assert_eq!(
        saved_processing(&fixture).await.stage,
        WorkStage::ReasonReady
    );
    assert!(saved_effects(&fixture)
        .await
        .iter()
        .all(|invocation| invocation.status == InvocationStatus::Settled));

    assert_eq!(saved_processing(&fixture).await.budget.dispatches, 1);
    let history = fixture
        .bound
        .resources
        .load_session_history(&fixture.bound.thread_id())
        .await
        .unwrap();
    assert_eq!(history.len(), 3);
}

#[tokio::test]
async fn production_stale_dispatch_is_rejected_without_invoking_new_or_old_target() {
    let (model, listener) = native_model().await;
    let effects = Arc::new(AtomicUsize::new(0));
    let fixture = fixture(
        Arc::new(model),
        vec![Arc::new(ProbeTool(Arc::clone(&effects)))],
    )
    .await;
    let server = serve(
        listener,
        fixture.bound.resources(),
        fixture.bound.thread_id(),
        true,
    );
    receive::run_receive(ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    let output = reason::run_reason(ReasonInput {
        context: fixture.context.clone(),
        has_tool_calls: false,
    })
    .await
    .unwrap();
    server.await.unwrap();
    let control = fixture
        .bound
        .resources
        .load_session_control(&fixture.bound.thread_id())
        .await
        .unwrap();
    fixture
        .bound
        .resources
        .apply_session_control(&ControlCommand {
            session_id: fixture.bound.thread_id(),
            command_id: "stop-exact-test".into(),
            expected_lifecycle: control.lifecycle,
            expected_revision: control.revision,
            expected_control_generation: control.control_generation,
            action: ControlAction::Stop {
                target: fixture.admission.execution.clone(),
            },
        })
        .await
        .unwrap();
    let call = fixture
        .context
        .work
        .bound_invocation(&output.reasoning.tool_calls[0].id)
        .await
        .unwrap()
        .policy_call;
    assert!(work_dispatch::begin(&fixture.context, &call).await.is_err());
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    let snapshot = fixture
        .bound
        .resources
        .inspect_work(&WorkQuery::new(
            fixture.bound.thread_id(),
            WorkSelector::Head,
        ))
        .await
        .unwrap();
    assert!(saved_effects(&fixture)
        .await
        .iter()
        .all(|invocation| invocation.status == InvocationStatus::Prepared));
}

struct ForbiddenModel(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl ReactLLM for ForbiddenModel {
    async fn generate_reasoning(
        &self,
        _: &[BaseMessage],
        _: &[&dyn BaseTool],
        _: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        self.0.fetch_add(1, Ordering::SeqCst);
        panic!("required execution must not reach model without SDK admission")
    }
}

#[tokio::test]
async fn production_bound_store_without_port_or_ticket_has_zero_model_effects() {
    let bound = TestSession::open().await;
    let session = Session::new(
        Arc::from("/tmp/required-no-port"),
        FrozenContext::builder().build(),
        None,
    );
    *session.transcript().write() =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id());
    let effects = Arc::new(AtomicUsize::new(0));
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_recipient_lifecycle(1)
    .with_llm(Arc::new(ForbiddenModel(effects.clone())))
    .build();
    assert!(matches!(
        run_react_loop(context, 2).await,
        LoopResult::Error(_)
    ));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert!(bound
        .resources
        .load_session_history(&bound.thread_id())
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn production_native_model_failure_freezes_checkpoint_without_regeneration() {
    let (model, listener) = native_model().await;
    let fixture = fixture(Arc::new(model), Vec::new()).await;
    let resources = fixture.bound.resources();
    let session_id = fixture.bound.thread_id();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let body = read_http_body(&mut socket).await;
        let snapshot = resources
            .inspect_work(&WorkQuery::new(&session_id, WorkSelector::Head))
            .await
            .unwrap();
        let work = crate::session::work_access::processing(
            resources.as_ref(),
            &session_id,
            snapshot.head.current_processing_id.as_deref().unwrap(),
        )
        .await
        .unwrap();
        let evidence = resources
            .read_evidence(&EvidenceQuery {
                session_id,
                reference: work.request.unwrap(),
            })
            .await
            .unwrap();
        let checkpoint: serde_json::Value = serde_json::from_slice(&evidence.bytes).unwrap();
        assert_eq!(checkpoint["request"]["body"], body);
        use tokio::io::AsyncWriteExt;
        socket.write_all(b"HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}").await.unwrap();
    });
    receive::run_receive(ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    assert!(reason::run_reason(ReasonInput {
        context: fixture.context.clone(),
        has_tool_calls: false
    })
    .await
    .is_err());
    server.await.unwrap();
    let snapshot = fixture
        .bound
        .resources
        .inspect_work(&WorkQuery::new(
            fixture.bound.thread_id(),
            WorkSelector::Head,
        ))
        .await
        .unwrap();
    let work = saved_processing(&fixture).await;
    assert_eq!(work.stage, WorkStage::Blocked);
    assert!(work.request.is_some());
    assert!(work.response.is_none());
    assert_eq!(work.budget.reason_requests, 1);
    assert!(reason::run_reason(ReasonInput {
        context: fixture.context.clone(),
        has_tool_calls: false
    })
    .await
    .is_err());
    assert!(receive::run_receive(ReceiveInput {
        context: fixture.context.clone()
    })
    .await
    .is_err());
    let after = fixture
        .bound
        .resources
        .inspect_work(&WorkQuery::new(
            fixture.bound.thread_id(),
            WorkSelector::Head,
        ))
        .await
        .unwrap();
    assert_eq!(saved_processing(&fixture).await.budget.reason_requests, 1);
    assert_eq!(saved_processing(&fixture).await.request, work.request);
}

struct UncertainEntry {
    entries: Arc<AtomicUsize>,
    observed: Arc<std::sync::OnceLock<WorkAdmission>>,
}

#[async_trait::async_trait]
impl peri_acp_types::execution_admission::ExecutionAdmissionPort for UncertainEntry {
    async fn admit(
        &self,
        _: peri_acp_types::execution_admission::AdmissionRequest,
    ) -> Result<
        peri_acp_types::execution_admission::AdmissionOutcome,
        peri_acp_types::execution_admission::ExecutionAdmissionError,
    > {
        panic!("registered ticket must not double admit")
    }

    async fn entered(
        &self,
        request: peri_acp_types::execution_admission::EntryRequest,
    ) -> Result<
        peri_acp_types::execution_admission::EntryOutcome,
        peri_acp_types::execution_admission::ExecutionAdmissionError,
    > {
        assert_eq!(self.observed.get(), Some(&request.admission));
        self.entries.fetch_add(1, Ordering::SeqCst);
        Ok(peri_acp_types::execution_admission::EntryOutcome::Unknown)
    }

    async fn settle(
        &self,
        _: peri_acp_types::execution_admission::SettlementRequest,
    ) -> Result<
        peri_acp_types::execution_admission::SettlementOutcome,
        peri_acp_types::execution_admission::ExecutionAdmissionError,
    > {
        panic!("unconfirmed SDK entry cannot fabricate stopped-attempt settlement")
    }
}

#[tokio::test]
async fn production_uncertain_sdk_entry_has_no_model_effect_or_ticket_replacement() {
    let effects = Arc::new(AtomicUsize::new(0));
    let mut fixture = fixture(Arc::new(ForbiddenModel(effects.clone())), Vec::new()).await;
    let entries = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(std::sync::OnceLock::new());
    let capture = observed.clone();
    fixture.context.sdk_admission_observed = Some(Arc::new(move |admission| {
        assert!(capture.set(admission).is_ok());
    }));
    fixture.context.execution_admission_port = Some(Arc::new(UncertainEntry {
        entries: entries.clone(),
        observed: observed.clone(),
    }));
    assert!(matches!(
        run_react_loop(fixture.context.clone(), 2).await,
        LoopResult::Error(_)
    ));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(entries.load(Ordering::SeqCst), 1);
    assert_eq!(observed.get(), Some(&fixture.admission));
    let snapshot = fixture
        .bound
        .resources
        .inspect_work(&WorkQuery::new(
            fixture.bound.thread_id(),
            WorkSelector::Head,
        ))
        .await
        .unwrap();
    assert_eq!(
        usize::from(
            crate::session::work_access::admission(
                fixture.bound.resources().as_ref(),
                &fixture.admission
            )
            .await
            .is_ok()
        ),
        1
    );
    assert_eq!(
        snapshot.control.attempt.as_ref(),
        Some(&fixture.admission.execution)
    );
    assert!(saved_effects(&fixture).await.is_empty());
}

#[tokio::test]
async fn production_finished_exact_work_exits_without_premature_sdk_settlement_or_local_wait() {
    let (model, listener) = native_model().await;
    let mut fixture = fixture(Arc::new(model), Vec::new()).await;
    fixture.context.async_ctx.idle_should_wait = Some(Arc::new(|| true));
    fixture.context.async_ctx.idle_wait_enabled = true;
    let server = serve(
        listener,
        fixture.bound.resources(),
        fixture.bound.thread_id(),
        false,
    );
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run_react_loop(fixture.context.clone(), 2),
    )
    .await
    .unwrap();
    assert!(matches!(result, LoopResult::Completed));
    server.await.unwrap();
    let snapshot = fixture
        .bound
        .resources
        .inspect_work(&WorkQuery::new(
            fixture.bound.thread_id(),
            WorkSelector::Head,
        ))
        .await
        .unwrap();
    assert!(snapshot.control.attempt.is_none());
    assert!(crate::session::work_access::admission(
        fixture.bound.resources().as_ref(),
        &fixture.admission
    )
    .await
    .unwrap()
    .leaving_evidence_id
    .is_none());
    assert_eq!(saved_processing(&fixture).await.stage, WorkStage::Settled);
}

#[tokio::test]
async fn production_before_effect_rejection_mirrors_same_canonical_paired_tool_result() {
    let (model, listener) = native_model().await;
    let effects = Arc::new(AtomicUsize::new(0));
    let fixture = fixture(Arc::new(model), vec![Arc::new(ProbeTool(effects.clone()))]).await;
    let server = serve(
        listener,
        fixture.bound.resources(),
        fixture.bound.thread_id(),
        true,
    );
    receive::run_receive(ReceiveInput {
        context: fixture.context.clone(),
    })
    .await
    .unwrap();
    let output = reason::run_reason(ReasonInput {
        context: fixture.context.clone(),
        has_tool_calls: false,
    })
    .await
    .unwrap();
    server.await.unwrap();
    let call = output.reasoning.tool_calls[0].clone();
    let mut rejected =
        crate::agent::react::ToolResult::error(&call.id, &call.name, "user rejected before effect");
    rejected.effective_error_code = Some(crate::tools::EffectiveToolErrorCode::UserRejected);
    let projections = work_dispatch::commit_results(&fixture.context, &[(call.clone(), rejected)])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(projections.len(), 1);
    let snapshot = fixture
        .bound
        .resources
        .inspect_work(&WorkQuery::new(
            fixture.bound.thread_id(),
            WorkSelector::Head,
        ))
        .await
        .unwrap();
    let invocation = saved_effects(&fixture)
        .await
        .into_iter()
        .find(|invocation| invocation.intent.tool_call_id == call.id)
        .unwrap();
    assert_eq!(invocation.status, InvocationStatus::Settled);
    assert!(matches!(
        invocation.outcome,
        Some(InvocationOutcome::Cancelled { .. })
    ));
    let Some(InvocationOutcome::Cancelled {
        result: canonical, ..
    }) = &invocation.outcome
    else {
        panic!("expected canonical cancellation result");
    };
    let payload = fixture
        .bound
        .resources()
        .read_evidence(&EvidenceQuery {
            session_id: fixture.bound.thread_id(),
            reference: canonical.content.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        std::str::from_utf8(&payload.bytes).unwrap(),
        peri_acp_types::store::serialize_persisted_payload(
            &peri_acp_types::store::PersistedPayload::Message(projections[0].clone())
        )
        .unwrap()
    );
    fixture
        .context
        .session
        .transcript
        .write()
        .mirror_committed_payload(peri_acp_types::store::PersistedPayload::Message(
            projections[0].clone(),
        ));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(saved_processing(&fixture).await.budget.dispatches, 0);
    let history = fixture
        .bound
        .resources
        .load_session_history(&fixture.bound.thread_id())
        .await
        .unwrap();
    assert_eq!(history.len(), 3);
    assert!(
        matches!(&projections[0], BaseMessage::Tool { tool_call_id, .. } if tool_call_id == &call.id)
    );
}

#[tokio::test]
async fn production_sdk_observer_double_attach_preserves_managed_parent_cancel() {
    let effects = Arc::new(AtomicUsize::new(0));
    let mut fixture = fixture(Arc::new(ForbiddenModel(effects.clone())), Vec::new()).await;
    let mailbox = crate::session::user_input_mailbox::UserInputMailbox::new_durable(
        fixture.bound.thread_id(),
        Arc::new(peri_acp_types::session::SessionInbox::new(Arc::new(
            fixture.context.session.queue.clone(),
        ))),
        Arc::new(|_| {}),
        fixture.bound.resources(),
        fixture.admission.lifecycle,
    );
    let parent_cancel = tokio_util::sync::CancellationToken::new();
    let ticket = mailbox.observe_sdk_run(&fixture.admission).await.unwrap();
    assert!(mailbox.attach_attempt(&ticket, parent_cancel.clone()));
    let started = Arc::new(AtomicUsize::new(0));
    let expected = fixture.admission.clone();
    let observed = started.clone();
    fixture.context.sdk_run_started = Some(Arc::new(move |actual| {
        assert_eq!(actual, expected);
        observed.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }));
    fixture.context.session.user_input_mailbox = Some(mailbox.clone());
    fixture.context.work.ensure(&fixture.context).await.unwrap();
    fixture.context.work.ensure(&fixture.context).await.unwrap();
    assert_eq!(started.load(Ordering::SeqCst), 1);
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert!(mailbox.stop_attempt(&ticket.id, mailbox.generation()));
    assert!(parent_cancel.is_cancelled());
    assert!(!fixture.context.session.turn.cancel_token.is_cancelled());
}
