use super::*;
use peri_acp::transport::{
    AcpTransport,
    mpsc::mpsc_transport_pair,
    types::{AcpError, IncomingMessage},
};

use serial_test::serial;
use std::cell::RefCell;

thread_local! {
    static REQUESTS: RefCell<Option<Vec<OperationTicket>>> = const { RefCell::new(None) };
}

pub(super) fn intercept_request(ticket: &OperationTicket) -> bool {
    REQUESTS.with(|requests| {
        let mut requests = requests.borrow_mut();
        let Some(requests) = requests.as_mut() else {
            return false;
        };
        requests.push(ticket.clone());
        true
    })
}

struct ScopeGuard {
    session: String,
    reset: u64,
    error: Option<String>,
    notice: Option<crate::kit::atoms::Notification>,
}

impl ScopeGuard {
    fn install() -> Self {
        let guard = Self {
            session: ACTIVE_SESSION_ID.state().read().clone(),
            reset: BRIDGE_RESET_COUNTER.get(),
            error: CRON_ACTION_ERROR.state().read().clone(),
            notice: crate::kit::atoms::NOTIFICATION.state().write().take(),
        };
        ACTIVE_SESSION_ID.set("session-a".to_string());
        CRON_ACTION_ERROR.set(None);
        REQUESTS.with(|requests| *requests.borrow_mut() = Some(Vec::new()));
        guard
    }
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        ACTIVE_SESSION_ID.set(std::mem::take(&mut self.session));
        BRIDGE_RESET_COUNTER.set(self.reset);
        CRON_ACTION_ERROR.set(self.error.take());
        crate::kit::atoms::NOTIFICATION.set(self.notice.take());
        REQUESTS.with(|requests| *requests.borrow_mut() = None);
    }
}

#[tokio::test]
#[serial]
async fn duplicate_target_dispatch_has_one_pending_request() {
    let _guard = ScopeGuard::install();
    let (transport, _server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session-a", false);
    super::super::tests::mount_panel_frames(2, move |frame, panel| {
        if frame != 1 {
            return;
        }
        let scope = current_scope();
        for method in ["cron/remove", "cron/toggle"] {
            dispatch_with_client(
                client.clone().into(),
                method,
                "job-a",
                scope.clone(),
                panel.action_gates,
            );
        }
        assert_eq!(
            REQUESTS.with(|requests| requests.borrow().as_ref().unwrap().len()),
            1
        );
        assert!(panel.action_gates.read()["job-a"].is_pending());
    });
}

#[test]
#[serial]
fn stale_scope_or_reset_completion_does_not_publish() {
    let _guard = ScopeGuard::install();
    for change_session in [false, true] {
        super::super::tests::mount_panel_frames(2, move |frame, panel| {
            if frame != 1 {
                return;
            }
            let ticket = panel
                .action_gates
                .write()
                .entry("job-a".into())
                .or_default()
                .begin(current_scope());
            CRON_ACTION_ERROR.set(Some("current error".into()));
            if change_session {
                ACTIVE_SESSION_ID.set("session-b".into());
            } else {
                BRIDGE_RESET_COUNTER.set(BRIDGE_RESET_COUNTER.get() + 1);
            }
            // 新帧尚未运行时也应拒收，不能依赖 effect 的时序。
            assert!(!complete(
                panel.action_gates,
                &ticket,
                "job-a",
                Err(AcpError::new(-32603, "late failure"))
            ));
            assert_eq!(
                CRON_ACTION_ERROR.state().read().clone().as_deref(),
                Some("current error")
            );
            assert!(crate::kit::atoms::NOTIFICATION.state().read().is_none());
            reset_scope(&mut panel.action_gates.write(), current_scope());
            assert!(ticket.is_cancelled());
            assert!(!complete(panel.action_gates, &ticket, "job-a", Ok(())));
            assert_eq!(
                CRON_ACTION_ERROR.state().read().clone().as_deref(),
                Some("current error")
            );
        });
    }
}

#[tokio::test]
#[serial]
async fn dispatch_without_session_keeps_existing_no_session_hint() {
    let _guard = ScopeGuard::install();
    let (transport, _server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    super::super::tests::mount_panel_frames(2, move |frame, panel| {
        if frame != 1 {
            return;
        }
        dispatch_with_client(
            client.clone().into(),
            "cron/toggle",
            "job-a",
            current_scope(),
            panel.action_gates,
        );
        assert_eq!(
            CRON_ACTION_ERROR.state().read().as_deref(),
            Some(i18n::tr("panel-cron-no-session").as_str())
        );
        assert!(REQUESTS.with(|requests| requests.borrow().as_ref().unwrap().is_empty()));
        assert!(panel.action_gates.read().is_empty());
    });
}

#[test]
#[serial]
fn completed_ticket_cannot_publish_over_next_request() {
    let _guard = ScopeGuard::install();
    super::super::tests::mount_panel_frames(2, |frame, panel| {
        if frame != 1 {
            return;
        }
        let old = panel
            .action_gates
            .write()
            .entry("job-a".into())
            .or_default()
            .begin(current_scope());
        assert!(complete(panel.action_gates, &old, "job-a", Ok(())));
        reset_scope(&mut panel.action_gates.write(), current_scope());
        let next = panel
            .action_gates
            .write()
            .get_mut("job-a")
            .unwrap()
            .begin(current_scope());
        CRON_ACTION_ERROR.set(Some("keep".into()));
        assert!(!complete(panel.action_gates, &old, "job-a", Ok(())));
        assert!(panel.action_gates.read()["job-a"].is_pending());
        assert_eq!(CRON_ACTION_ERROR.state().read().as_deref(), Some("keep"));
        assert!(complete(
            panel.action_gates,
            &next,
            "job-a",
            Err(AcpError::new(-32603, "failure"))
        ));
        assert!(
            CRON_ACTION_ERROR
                .state()
                .read()
                .as_ref()
                .unwrap()
                .contains("failure")
        );
    });
}

#[test]
#[serial]
fn cancelled_ticket_cannot_clear_existing_error() {
    let _guard = ScopeGuard::install();
    super::super::tests::mount_panel_frames(2, |frame, panel| {
        if frame != 1 {
            return;
        }
        let ticket = panel
            .action_gates
            .write()
            .entry("job-a".into())
            .or_default()
            .begin(current_scope());
        panel
            .action_gates
            .write()
            .get_mut("job-a")
            .unwrap()
            .cancel();
        CRON_ACTION_ERROR.set(Some("keep".into()));
        assert!(!complete(panel.action_gates, &ticket, "job-a", Ok(())));
        assert_eq!(
            CRON_ACTION_ERROR.state().read().clone().as_deref(),
            Some("keep")
        );
    });
}

#[tokio::test]
async fn mutations_send_session_and_job_identity_and_surface_server_failure() {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session-a", false);
    let identity = client.stable_session_identity().unwrap();
    let server_task = tokio::spawn(async move {
        for expected in ["cron/toggle", "cron/remove"] {
            let Some(IncomingMessage::Request { id, method, params }) = server.recv().await else {
                panic!("expected cron wire request");
            };
            assert_eq!(method, expected);
            assert_eq!(params, json!({"sessionId":"session-a","id":"job-a"}));
            let response = if expected == "cron/toggle" {
                Ok(json!({"id":"job-a","success":true}))
            } else {
                Err(AcpError::new(-32602, "job not found"))
            };
            server.send_response(id, response).await.unwrap();
        }
    });
    request(&client, &identity, "cron/toggle", "job-a")
        .await
        .unwrap();
    assert_eq!(
        request(&client, &identity, "cron/remove", "job-a")
            .await
            .unwrap_err()
            .code,
        -32602
    );
    server_task.await.unwrap();
}

#[tokio::test]
async fn stale_generation_is_rejected_before_sending_even_for_same_session() {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session-a", false);
    let mut identity = client.stable_session_identity().unwrap();
    identity.1 += 1;
    assert_eq!(
        request(&client, &identity, "cron/remove", "job-a")
            .await
            .unwrap_err()
            .code,
        -32602
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), server.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn malformed_success_cannot_be_reported_as_mutation_success() {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session-a", false);
    let identity = client.stable_session_identity().unwrap();
    tokio::spawn(async move {
        let Some(IncomingMessage::Request { id, .. }) = server.recv().await else {
            panic!("request")
        };
        server
            .send_response(id, Ok(json!({"id":"other","success":true})))
            .await
            .unwrap();
    });
    assert_eq!(
        request(&client, &identity, "cron/toggle", "job-a")
            .await
            .unwrap_err()
            .code,
        -32603
    );
}
