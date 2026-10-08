use super::*;
use crate::event::AcpEvent;
use peri_acp_types::permission::PermissionMode;

fn execution_identity(events: &[(String, Value)]) -> String {
    let starts = events
        .iter()
        .filter_map(|(method, params)| {
            if method != "peri/agent_event" {
                return None;
            }
            let event: AcpEvent = serde_json::from_str(params["event_json"].as_str()?).ok()?;
            match event {
                AcpEvent::ExecutionStarted { request_id, .. } => Some(request_id),
                _ => None,
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 1);
    assert!(!starts[0].is_empty());
    let terminals = events
        .iter()
        .filter(|(method, _)| method == "peri/agent_event_done")
        .collect::<Vec<_>>();
    assert_eq!(terminals.len(), 1);
    assert_eq!(terminals[0].1["requestId"], starts[0]);
    starts[0].clone()
}

#[tokio::test]
#[serial]
async fn internal_execution_announces_identity_and_matching_terminal() {
    let harness = ActivationHarness::new().await;
    harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .allow();
    harness.complete_child("identity-child");
    let response = crate::host::dispatch_prompt_turn(
        json!({"sessionId": harness.session_id, "message":{"role":"user", "content":[]}}),
        crate::host::PromptOrigin::Continuation { mq_steering: true },
        Some(0),
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    )
    .await
    .unwrap();
    assert_eq!(response["stopReason"], "end_turn");
    execution_identity(&harness.recorded.notifications());
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

#[tokio::test]
#[serial]
async fn failed_execution_assembly_closes_the_announced_identity_once() {
    let harness = ActivationHarness::new().await;
    harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .allow();
    harness.complete_child("failed-child");
    harness
        .sessions
        .lock()
        .await
        .get_mut(&harness.session_id)
        .unwrap()
        .frozen = None;
    let error = crate::host::dispatch_prompt_turn(
        json!({"sessionId": harness.session_id, "message":{"role":"user", "content":[]}}),
        crate::host::PromptOrigin::Continuation { mq_steering: true },
        Some(0),
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("frozen"));
    execution_identity(&harness.recorded.notifications());
    let sessions = harness.sessions.lock().await;
    assert!(sessions[&harness.session_id].cancel_token.is_none());
    assert!(!sessions[&harness.session_id].continuation_in_flight);
    drop(sessions);
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

struct ApprovalTransport {
    recorded: Arc<MockTransport>,
}

#[async_trait]
impl crate::transport::AcpTransport for ApprovalTransport {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.recorded
            .notifications
            .lock()
            .unwrap()
            .push((method.into(), params));
        Ok(json!({"outcome": {"outcome": "selected", "optionId": "allow_once"}}))
    }
    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        self.recorded.send_notification(method, params).await
    }
    async fn recv(&self) -> Option<IncomingMessage> {
        None
    }
    async fn send_response(
        &self,
        _id: RequestId,
        _result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        Ok(())
    }
}

#[tokio::test]
#[serial]
async fn idle_schedule_approval_is_bracketed_by_its_own_identity() {
    let harness = ActivationHarness::new().await;
    harness.cfg.permission_mode.store(PermissionMode::Default);
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(ApprovalTransport {
        recorded: harness.recorded.clone(),
    });
    let request = peri_acp_types::cron::CronContinuationRequest {
        session_id: harness.session_id.clone(),
        inbox: harness
            .cfg
            .session_manager
            .v2_queue_for(&harness.session_id)
            .unwrap(),
        trigger: peri_acp_types::cron::CronTrigger {
            task_id: "approval".into(),
            firing_id: "2026-10-07T00:00:00+00:00".into(),
            prompt: "run scheduled work".into(),
        },
    };
    assert!(crate::host::execution::approve_schedule(
        &request,
        &harness.sessions,
        &harness.locks,
        &harness.cfg,
        &transport
    )
    .await
    .unwrap());
    let events = harness.recorded.notifications();
    execution_identity(&events);
    let start = events
        .iter()
        .position(|(method, params)| {
            method == "peri/agent_event"
                && params["event_json"]
                    .as_str()
                    .unwrap_or("")
                    .contains("execution_started")
        })
        .unwrap();
    let approval = events
        .iter()
        .position(|(method, _)| method == "session/request_permission")
        .unwrap();
    let terminal = events
        .iter()
        .position(|(method, _)| method == "peri/agent_event_done")
        .unwrap();
    assert!(start < approval && approval < terminal);
    let sessions = harness.sessions.lock().await;
    assert!(sessions[&harness.session_id].cancel_token.is_none());
    assert!(!sessions[&harness.session_id].continuation_in_flight);
    drop(sessions);
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}
