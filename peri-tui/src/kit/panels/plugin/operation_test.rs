use super::*;
use crate::kit::atoms::{ACTIVE_SESSION_ID, BRIDGE_RESET_COUNTER};
use peri_acp::transport::{
    AcpTransport,
    mpsc::mpsc_transport_pair,
    types::{AcpError, IncomingMessage},
};
use ratatui_kit::crossterm::event::{
    KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use serial_test::serial;

struct SessionGuard(String, u64);

impl SessionGuard {
    fn new() -> Self {
        let guard = Self(
            ACTIVE_SESSION_ID.state().read().clone(),
            BRIDGE_RESET_COUNTER.get(),
        );
        *ACTIVE_SESSION_ID.state().write() = "plugin-operation-session".into();
        guard
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        *ACTIVE_SESSION_ID.state().write() = self.0.clone();
        BRIDGE_RESET_COUNTER.set(self.1);
    }
}

fn plugin(scope: &str) -> PluginSummary {
    PluginSummary {
        name: "compiler".into(),
        marketplace: "tools".into(),
        install_scope: scope.into(),
        ..Default::default()
    }
}

#[test]
fn keyboard_and_mouse_select_the_same_semantic_request_for_all_scopes() {
    let area = Rect::new(4, 7, 60, 30);
    for scope in ["user", "project", "local"] {
        let plugin = plugin(scope);
        for (index, action) in ["enable", "uninstall", "update", "back"]
            .into_iter()
            .enumerate()
        {
            let key = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            let mouse = Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: area.x + 2,
                row: area.y + 1 + 16 + index as u16,
                modifiers: KeyModifiers::NONE,
            });
            let key_index = detail_action_index(&key, Some(area), &plugin, index).unwrap();
            let mouse_index = detail_action_index(&mouse, Some(area), &plugin, 0).unwrap();
            assert_eq!(key_index, mouse_index);
            let actions = crate::kit::panels::plugin::action_list(false);
            assert_eq!(actions[key_index], action);
            assert_eq!(
                PluginOperation::installed(actions[key_index], &plugin),
                PluginOperation::installed(actions[mouse_index], &plugin),
            );
            if action == "enable" {
                let operation = PluginOperation::installed(action, &plugin).unwrap();
                assert_eq!(operation.params["scope"], scope);
                assert_eq!(operation.params["pluginId"], "compiler@tools");
            }
        }
    }
}

#[test]
fn old_session_completion_cannot_clear_a_new_pending_operation() {
    let mut state = OperationState::default();
    let first = state.begin(
        PluginOperation::installed("enable", &plugin("local")).unwrap(),
        SearchSession::default(),
    );
    let mut new_session = SearchSession::default();
    new_session.id = "another-session".into();
    let second = state.begin(
        PluginOperation::installed("update", &plugin("local")).unwrap(),
        new_session,
    );
    assert!(!state.complete(&first, Err("late failure".into())));
    assert_eq!(state.pending_action(), Some("update"));
    assert!(state.complete(&second, Ok(())));
    assert!(state.pending_action().is_none());
    assert!(state.error.is_none());
}

#[test]
fn marketplace_operations_encode_host_catalog_intent_without_project_paths() {
    for (operation, method, action, params) in [
        (
            PluginOperation::marketplace_add("owner/tools".into()),
            "marketplace/add",
            "add_marketplace",
            json!({"source": "owner/tools"}),
        ),
        (
            PluginOperation::marketplace_remove("tools".into()),
            "marketplace/remove",
            "delete_marketplace",
            json!({"name": "tools"}),
        ),
        (
            PluginOperation::marketplace_refresh("tools".into()),
            "marketplace/refresh",
            "refresh_marketplace",
            json!({"name": "tools"}),
        ),
    ] {
        assert_eq!(operation.method, method);
        assert_eq!(operation.action, action);
        assert_eq!(operation.params, params);
    }
}

#[tokio::test]
#[serial]
async fn marketplace_requests_preserve_ticket_context_and_surface_backend_failures() {
    let _guard = SessionGuard::new();
    for operation in [
        PluginOperation::marketplace_add("owner/tools".into()),
        PluginOperation::marketplace_remove("tools".into()),
        PluginOperation::marketplace_refresh("tools".into()),
    ] {
        let (transport, server) = mpsc_transport_pair();
        let (client, notification_tx, _notification_rx) = AcpTuiClient::new(transport);
        client.force_stable_for_test("plugin-operation-session", true);
        client.spawn_pump(notification_tx);
        let state = Arc::new(parking_lot::Mutex::new(OperationState::default()));
        let ticket = state
            .lock()
            .begin(operation.clone(), SearchSession::current());
        let complete_state = state.clone();
        let task = launch(ticket, Some(Arc::new(client)), move |ticket, result| {
            complete_state.lock().complete(ticket, result);
            None
        });
        let incoming = tokio::time::timeout(Duration::from_secs(2), server.recv())
            .await
            .unwrap()
            .unwrap();
        let IncomingMessage::Request { id, method, params } = incoming else {
            panic!("expected marketplace request");
        };
        assert_eq!(method, operation.method);
        let mut expected = operation.params;
        expected["sessionId"] = json!("plugin-operation-session");
        assert_eq!(params, expected);
        server
            .send_response(id, Err(AcpError::new(-32603, "catalog persistence failed")))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert!(state.lock().pending_action().is_none());
        assert!(
            state
                .lock()
                .error
                .as_ref()
                .unwrap()
                .contains("catalog persistence failed")
        );
    }
}

#[tokio::test]
#[serial]
async fn missing_client_leaves_loading_and_preserves_visible_error() {
    let _guard = SessionGuard::new();
    let state = Arc::new(parking_lot::Mutex::new(OperationState::default()));
    let ticket = state.lock().begin(
        PluginOperation::installed("enable", &plugin("user")).unwrap(),
        SearchSession::current(),
    );
    let complete_state = state.clone();
    launch(ticket, None, move |ticket, result| {
        complete_state.lock().complete(ticket, result);
        None
    })
    .await
    .unwrap();
    assert!(state.lock().pending_action().is_none());
    assert_eq!(
        state.lock().error.as_deref(),
        Some("ACP client not available")
    );
}

async fn wire_result(response: Result<Value, AcpError>, late: bool) -> Option<String> {
    let (transport, server) = mpsc_transport_pair();
    let (client, notification_tx, _notification_rx) = AcpTuiClient::new(transport);
    client.force_stable_for_test("plugin-operation-session", true);
    client.spawn_pump(notification_tx);
    let state = Arc::new(parking_lot::Mutex::new(OperationState::default()));
    let ticket = state.lock().begin(
        PluginOperation::installed("disable", &plugin("project")).unwrap(),
        SearchSession::current(),
    );
    let complete_state = state.clone();
    let task = launch(ticket, Some(Arc::new(client)), move |ticket, result| {
        complete_state.lock().complete(ticket, result);
        None
    });
    let incoming = tokio::time::timeout(Duration::from_secs(2), server.recv())
        .await
        .unwrap()
        .unwrap();
    let IncomingMessage::Request { id, method, params } = incoming else {
        panic!("expected request");
    };
    assert_eq!(method, "plugin/toggle");
    assert_eq!(params["sessionId"], "plugin-operation-session");
    assert_eq!(params["scope"], "project");
    assert_eq!(params["enable"], false);
    if late {
        BRIDGE_RESET_COUNTER.set(BRIDGE_RESET_COUNTER.get().wrapping_add(1));
        state.lock().reset_session(SearchSession::current());
    }
    server.send_response(id, response).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    assert!(state.lock().pending_action().is_none());
    let error = state.lock().error.clone();
    error
}

#[tokio::test]
#[serial]
async fn success_clears_pending_without_waiting_for_a_keyboard_event() {
    let _guard = SessionGuard::new();
    assert_eq!(wire_result(Ok(json!({"success": true})), false).await, None);
}

#[tokio::test]
#[serial]
async fn server_failure_clears_pending_and_exposes_actual_error() {
    let _guard = SessionGuard::new();
    let error = wire_result(
        Err(AcpError::new(-32603, "settings permission denied")),
        false,
    )
    .await
    .unwrap();
    assert!(error.contains("settings permission denied"));
}

#[tokio::test]
#[serial]
async fn late_same_session_reset_response_is_discarded() {
    let _guard = SessionGuard::new();
    assert_eq!(
        wire_result(Err(AcpError::new(-32603, "old session error")), true).await,
        None
    );
}
