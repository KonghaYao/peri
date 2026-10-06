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
        install_scope: Some(scope.into()),
        toggle_supported: Some(true),
        ..Default::default()
    }
}

#[test]
fn keyboard_and_mouse_select_the_same_semantic_request_for_all_scopes() {
    let area = Rect::new(4, 7, 60, 30);
    for scope in ["user", "project", "local"] {
        for enabled in [false, true] {
            let mut plugin = plugin(scope);
            plugin.enabled = enabled;
            let actions = crate::kit::panels::plugin::action_list(enabled);
            for (index, action) in actions.iter().copied().enumerate() {
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
                assert_eq!(actions[key_index], action);
                assert_eq!(
                    PluginOperation::installed(actions[key_index], &plugin),
                    PluginOperation::installed(actions[mouse_index], &plugin),
                );
                if action != "back" {
                    let operation = PluginOperation::installed(action, &plugin).unwrap();
                    let (method, expected) = match action {
                        "enable" | "disable" => (
                            "plugin/toggle",
                            json!({
                                "pluginId": "compiler@tools", "scope": scope, "enable": action == "enable",
                            }),
                        ),
                        "uninstall" => (
                            "plugin/uninstall",
                            json!({"pluginId": "compiler@tools", "scope": scope}),
                        ),
                        "update" => (
                            "plugin/update",
                            json!({"pluginId": "compiler@tools", "scope": scope}),
                        ),
                        _ => unreachable!(),
                    };
                    assert_eq!(operation.method, method);
                    assert_eq!(operation.params, expected);
                }
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
fn installed_mutations_require_a_writable_scope_and_explicit_capability() {
    for scope in [None, Some("session"), Some(""), Some("workspace")] {
        let mut summary = plugin("user");
        summary.install_scope = scope.map(str::to_string);
        for action in ["enable", "disable", "uninstall", "update"] {
            assert_eq!(
                PluginOperation::installed(action, &summary).unwrap_err(),
                crate::i18n::tr("panel-plugin-management-unsupported"),
            );
        }
    }
    for capability in [None, Some(false)] {
        let mut summary = plugin("project");
        summary.toggle_supported = capability;
        for action in ["enable", "disable", "uninstall", "update"] {
            assert!(PluginOperation::installed(action, &summary).is_err());
        }
    }
    for scope in ["user", "project", "local"] {
        for action in ["enable", "disable", "uninstall", "update"] {
            let operation = PluginOperation::installed(action, &plugin(scope)).unwrap();
            assert_eq!(operation.params["scope"], scope);
            if matches!(action, "enable" | "disable") {
                assert_eq!(operation.params["enable"], action == "enable");
            } else {
                assert!(operation.params.get("enable").is_none());
            }
        }
    }
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

pub(crate) struct MountedPanel {
    pub operation: State<OperationState>,
    pub selected: State<usize>,
    pub active_tab: State<crate::kit::atoms::PluginViewTab>,
    pub discover: State<super::super::discover::DiscoverState>,
    pub action_index: State<usize>,
    pub confirm_action: State<Option<String>>,
    pub operation_loading: State<Option<String>>,
    pub detail_plugin_idx: State<Option<usize>>,
    pub marketplace_detail: State<Option<usize>>,
    pub marketplace_detail_action: State<usize>,
    pub add_marketplace_input: State<crate::components::textarea::TextAreaState>,
    pub add_marketplace_active: State<bool>,
}

thread_local! {
    static MOUNT_VISITOR: std::cell::RefCell<Option<Box<dyn FnOnce(MountedPanel)>>> =
        const { std::cell::RefCell::new(None) };
}

pub(crate) fn visit_mounted_panel(panel: MountedPanel) {
    let visitor = MOUNT_VISITOR.with(|slot| slot.borrow_mut().take());
    if let Some(visitor) = visitor {
        visitor(panel);
    }
}

impl MountedPanel {
    fn event(&self, event: Event) -> ratatui_kit::EventResult {
        use super::super::{panel_handler, search_handler};
        use ratatui_kit::EventResult;
        let result = search_handler::handle_search_event(
            event.clone(),
            None,
            self.active_tab,
            self.discover,
            self.detail_plugin_idx,
            self.marketplace_detail,
            self.marketplace_detail_action,
            self.confirm_action,
            self.operation_loading,
            self.operation,
            self.add_marketplace_input,
            self.add_marketplace_active,
        );
        if matches!(result, EventResult::Consumed) {
            return result;
        }
        panel_handler::handle_panel_event(
            event,
            None,
            self.selected,
            self.active_tab,
            self.discover,
            self.action_index,
            self.confirm_action,
            self.operation_loading,
            self.operation,
            self.detail_plugin_idx,
            self.marketplace_detail,
            self.marketplace_detail_action,
            self.add_marketplace_input,
            self.add_marketplace_active,
        )
    }

    fn start(
        &self,
        client: Arc<AcpTuiClient>,
        operation: PluginOperation,
    ) -> (OperationTicket, tokio::task::JoinHandle<()>) {
        let ticket = self
            .operation
            .write()
            .begin(operation, SearchSession::current());
        let state = self.operation;
        let task = launch(ticket.clone(), Some(client), move |ticket, result| {
            if let Some(mut state) = state.try_write() {
                state.complete(ticket, result);
                None
            } else {
                Some(result)
            }
        });
        (ticket, task)
    }
}

fn mount_panel(visitor: impl FnOnce(MountedPanel) + 'static) -> String {
    let _catalog_guard = super::super::data::empty_catalog_for_test();
    use crate::app::panel_types::PanelKind;
    use crate::kit::atoms::{ACTIVE_PANEL, OPEN_PANELS};
    use ratatui_kit::prelude::*;
    struct PanelGuard(Option<PanelKind>, Vec<PanelKind>);
    impl Drop for PanelGuard {
        fn drop(&mut self) {
            ACTIVE_PANEL.set(self.0);
            OPEN_PANELS.set(std::mem::take(&mut self.1));
        }
    }
    let _guard = PanelGuard(ACTIVE_PANEL.get(), OPEN_PANELS.state().read().clone());
    ACTIVE_PANEL.set(None);
    OPEN_PANELS.set(Vec::new());
    crate::kit::panel_registry::open_panel(PanelKind::Plugin);
    MOUNT_VISITOR.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Box::new(visitor));
    });
    let buffer =
        ratatui_kit::test_util::render_frame(element!(super::super::PluginPanel()), 160, 40);
    buffer.content.iter().map(|cell| cell.symbol()).collect()
}

fn paused_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap()
}

fn controlled_client() -> (
    Arc<AcpTuiClient>,
    peri_acp::transport::mpsc::MpscServerTransport,
) {
    let (transport, server) = mpsc_transport_pair();
    let (client, notification_tx, _notification_rx) = AcpTuiClient::new(transport);
    client.force_stable_for_test("plugin-operation-session", false);
    client.spawn_pump(notification_tx);
    (Arc::new(client), server)
}

async fn mutation_request(
    server: &peri_acp::transport::mpsc::MpscServerTransport,
    method: &str,
) -> peri_acp::transport::types::RequestId {
    let incoming = tokio::time::timeout(Duration::from_secs(2), server.recv())
        .await
        .unwrap()
        .unwrap();
    let IncomingMessage::Request {
        id,
        method: actual,
        params,
    } = incoming
    else {
        panic!("expected mutation request");
    };
    assert_eq!(actual, method);
    assert_eq!(params["sessionId"], "plugin-operation-session");
    id
}

#[test]
#[serial]
fn mounted_mutations_without_responses_timeout_with_unknown_outcome() {
    let _guard = SessionGuard::new();
    crate::i18n::init(Some("en"));
    for operation in [
        PluginOperation::installed("enable", &plugin("user")).unwrap(),
        PluginOperation::installed("uninstall", &plugin("user")).unwrap(),
        PluginOperation::installed("update", &plugin("user")).unwrap(),
        PluginOperation::install("compiler".into(), "tools".into(), "project"),
        PluginOperation::marketplace_add("owner/tools".into()),
        PluginOperation::marketplace_remove("tools".into()),
        PluginOperation::marketplace_refresh("tools".into()),
    ] {
        let text = mount_panel(move |panel| {
            paused_runtime().block_on(async {
                let (client, server) = controlled_client();
                let (_, task) = panel.start(client, operation.clone());
                mutation_request(&server, operation.method).await;
                assert_eq!(
                    panel.operation.read().pending_action(),
                    Some(operation.action)
                );
                tokio::time::advance(OPERATION_TIMEOUT).await;
                task.await.unwrap();
                assert!(panel.operation.read().pending_action().is_none());
                assert!(
                    panel
                        .operation
                        .read()
                        .error
                        .as_ref()
                        .unwrap()
                        .contains("outcome unknown")
                );
                assert!(
                    tokio::time::timeout(Duration::from_millis(1), server.recv())
                        .await
                        .is_err()
                );
            });
        });
        assert!(text.contains("outcome unknown"));
        assert!(!text.contains("operation failed"));
    }
}

#[test]
#[serial]
fn mounted_late_timeout_response_cannot_clear_new_pending_ticket() {
    let _guard = SessionGuard::new();
    mount_panel(|panel| {
        paused_runtime().block_on(async {
            let (client, server) = controlled_client();
            let (old, old_task) = panel.start(
                client.clone(),
                PluginOperation::installed("disable", &plugin("project")).unwrap(),
            );
            let old_id = mutation_request(&server, "plugin/toggle").await;
            tokio::time::advance(OPERATION_TIMEOUT).await;
            old_task.await.unwrap();
            let (new, new_task) = panel.start(
                client,
                PluginOperation::installed("update", &plugin("project")).unwrap(),
            );
            let new_id = mutation_request(&server, "plugin/update").await;
            server
                .send_response(old_id, Ok(json!({"success": true})))
                .await
                .unwrap();
            tokio::task::yield_now().await;
            assert!(
                !panel
                    .operation
                    .write()
                    .complete(&old, Err("late failure".into()))
            );
            assert_eq!(panel.operation.read().pending_action(), Some("update"));
            assert!(panel.operation.read().error.is_none());
            assert_eq!(
                panel.operation.read().pending.as_ref().unwrap().generation,
                new.generation
            );
            server
                .send_response(new_id, Ok(json!({"success": true})))
                .await
                .unwrap();
            new_task.await.unwrap();
            assert!(panel.operation.read().pending_action().is_none());
        });
    });
}

#[test]
#[serial]
fn mounted_escape_cancels_only_client_wait_then_allows_panel_exit() {
    let _guard = SessionGuard::new();
    use crate::kit::atoms::{ACTIVE_PANEL, PluginViewTab};
    for tab in [
        PluginViewTab::Installed,
        PluginViewTab::Discover,
        PluginViewTab::Marketplaces,
    ] {
        mount_panel(move |panel| {
            *panel.active_tab.write() = tab;
            paused_runtime().block_on(async {
                let (client, server) = controlled_client();
                let (old, task) = panel.start(
                    client.clone(),
                    PluginOperation::installed("enable", &plugin("local")).unwrap(),
                );
                let old_id = mutation_request(&server, "plugin/toggle").await;
                let escape = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                assert!(matches!(
                    panel.event(escape.clone()),
                    ratatui_kit::EventResult::Consumed
                ));
                task.await.unwrap();
                assert!(panel.operation.read().pending_action().is_none());
                assert!(old.cancelled.is_cancelled());
                assert!(
                    panel
                        .operation
                        .read()
                        .error
                        .as_ref()
                        .unwrap()
                        .contains("outcome unknown")
                );
                assert!(ACTIVE_PANEL.get().is_some());
                assert!(
                    tokio::time::timeout(Duration::from_millis(1), server.recv())
                        .await
                        .is_err()
                );
                let (_, next_task) = panel.start(
                    client,
                    PluginOperation::installed("update", &plugin("local")).unwrap(),
                );
                let new_id = mutation_request(&server, "plugin/update").await;
                server
                    .send_response(old_id, Ok(json!({"success": true})))
                    .await
                    .unwrap();
                tokio::task::yield_now().await;
                assert_eq!(panel.operation.read().pending_action(), Some("update"));
                server
                    .send_response(new_id, Ok(json!({"success": true})))
                    .await
                    .unwrap();
                next_task.await.unwrap();
                panel.event(escape);
                assert!(ACTIVE_PANEL.get().is_none());
            });
        });
    }
}

#[test]
#[serial]
fn mounted_same_session_reload_isolates_real_client_generation() {
    let _guard = SessionGuard::new();
    mount_panel(|panel| {
        paused_runtime().block_on(async {
            let (client, server) = controlled_client();
            let original = client.stable_session_identity().unwrap();
            let reset = BRIDGE_RESET_COUNTER.get();
            let (_, old_task) = panel.start(
                client.clone(),
                PluginOperation::installed("disable", &plugin("user")).unwrap(),
            );
            let old_id = mutation_request(&server, "plugin/toggle").await;
            let loader = client.clone();
            let load = tokio::spawn(async move {
                loader
                    .load_session("plugin-operation-session", "/tmp", None)
                    .await
            });
            let load_id = mutation_request(&server, "session/load").await;
            server.send_response(load_id, Ok(json!({}))).await.unwrap();
            load.await.unwrap().unwrap();
            assert_ne!(client.stable_session_identity().unwrap(), original);
            assert_eq!(BRIDGE_RESET_COUNTER.get(), reset);
            server
                .send_response(old_id, Ok(json!({"success": true})))
                .await
                .unwrap();
            old_task.await.unwrap();
            assert!(panel.operation.read().pending_action().is_none());
            assert!(
                panel
                    .operation
                    .read()
                    .error
                    .as_ref()
                    .unwrap()
                    .contains("outcome unknown")
            );
        });
    });
}

#[test]
#[serial]
fn mounted_missing_client_and_server_failure_remain_visible() {
    let _guard = SessionGuard::new();
    crate::i18n::init(Some("en"));
    for missing_client in [true, false] {
        let text = mount_panel(move |panel| {
            paused_runtime().block_on(async {
                let (client, server) = controlled_client();
                let ticket = panel.operation.write().begin(
                    PluginOperation::installed("enable", &plugin("user")).unwrap(),
                    SearchSession::current(),
                );
                let state = panel.operation;
                let task = launch(
                    ticket,
                    (!missing_client).then_some(client),
                    move |ticket, result| {
                        state.write().complete(ticket, result);
                        None
                    },
                );
                if !missing_client {
                    let id = mutation_request(&server, "plugin/toggle").await;
                    server
                        .send_response(id, Err(AcpError::new(-32603, "settings permission denied")))
                        .await
                        .unwrap();
                }
                task.await.unwrap();
                assert!(panel.operation.read().pending_action().is_none());
            });
        });
        assert!(text.contains(if missing_client {
            "ACP client not available"
        } else {
            "settings permission denied"
        }));
    }
}

#[test]
#[serial]
fn mounted_unmanaged_or_ambiguous_mutations_report_reason_without_pending() {
    let _guard = SessionGuard::new();
    crate::i18n::init(Some("en"));
    for (scope, capability, management_error) in [
        (None, Some(false), Some("Plugin source is not managed")),
        (
            None,
            Some(true),
            Some("Plugin installation scope is ambiguous"),
        ),
        (
            Some("session"),
            Some(true),
            Some("Plugin installation scope is ambiguous"),
        ),
        (
            Some("user"),
            Some(false),
            Some("Plugin source is not managed"),
        ),
        (Some("project"), None, None),
        (None, Some(false), None),
        (None, Some(false), Some(" ")),
    ] {
        let mut summary = plugin("user");
        summary.install_scope = scope.map(str::to_string);
        summary.toggle_supported = capability;
        summary.management_error = management_error.map(str::to_string);
        let expected = management_error
            .filter(|error| !error.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| crate::i18n::tr("panel-plugin-management-unsupported"));
        for action in ["enable", "disable", "uninstall", "update"] {
            let summary = summary.clone();
            let expected = expected.clone();
            mount_panel(move |panel| {
                paused_runtime().block_on(async {
                    let (_client, server) = controlled_client();
                    *panel.detail_plugin_idx.write() = Some(0);
                    installed_action(
                        action,
                        &summary,
                        panel.operation,
                        panel.confirm_action,
                        panel.detail_plugin_idx,
                    );
                    assert_eq!(
                        panel.operation.read().error.as_deref(),
                        Some(expected.as_str())
                    );
                    assert!(panel.operation.read().pending_action().is_none());
                    assert!(panel.operation.read().confirmation.is_none());
                    assert_eq!(*panel.detail_plugin_idx.read(), Some(0));
                    assert!(
                        tokio::time::timeout(Duration::from_millis(1), server.recv())
                            .await
                            .is_err()
                    );
                });
            });
        }
    }
}
