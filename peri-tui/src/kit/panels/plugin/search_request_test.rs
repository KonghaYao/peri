//! Request/notification/bridge/render regression for plugin search completion.

use super::{
    discover::{DiscoverState, SearchSession},
    render, search_request,
};
use crate::acp_client::AcpTuiClient;
use crate::kit::atoms::{ACTIVE_SESSION_ID, BRIDGE_RESET_COUNTER};
use peri_acp::transport::{
    AcpTransport,
    mpsc::mpsc_transport_pair,
    types::{AcpError, IncomingMessage},
};
use ratatui_kit::ratatui::style::{Color, Style};
use serde_json::json;
use serial_test::serial;
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct SearchAtomsGuard {
    session: String,
    reset: u64,
}

impl SearchAtomsGuard {
    fn capture() -> Self {
        Self {
            session: ACTIVE_SESSION_ID.state().read().clone(),
            reset: BRIDGE_RESET_COUNTER.get(),
        }
    }
}

impl Drop for SearchAtomsGuard {
    fn drop(&mut self) {
        // Each test joins its request/notifier/bridge tasks before restoring
        // shared atoms, including the same-session reset counter.
        *ACTIVE_SESSION_ID.state().write() = std::mem::take(&mut self.session);
        *BRIDGE_RESET_COUNTER.state().write() = self.reset;
    }
}

fn discover_text(state: &DiscoverState) -> String {
    let entries = state.visible_items(&[]);
    let mut lines = Vec::new();
    render::render_discover_list(
        &mut lines,
        &state.editor.text,
        false,
        &state.status,
        &entries,
        0,
        0,
        5,
        Style::default(),
        Style::default(),
        Style::default(),
        Style::default(),
        Style::default(),
        Color::Reset,
        Color::Reset,
        Style::default(),
    );
    lines
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn completed_search_view(fail: bool) -> String {
    crate::kit::atoms::init_atoms();
    *ACTIVE_SESSION_ID.state().write() = "plugin-search-session".into();
    let (client_transport, server) = mpsc_transport_pair();
    let (client, notification_tx, notification_rx) = AcpTuiClient::new(client_transport);
    client.force_stable_for_test("plugin-search-session", true);
    client.spawn_pump(notification_tx);
    let shutdown = CancellationToken::new();
    let (bridge_tx, bridge_rx) = mpsc::unbounded_channel();
    let notifier =
        crate::kit::acp_notifier::spawn_kit_notifier(notification_rx, bridge_tx, shutdown.clone());
    let (observed_tx, mut observed_rx) = mpsc::unbounded_channel();
    let bridge = crate::kit::acp_bridge::spawn_acp_bridge_observed_with_client(
        bridge_rx,
        shutdown.clone(),
        client.clone(),
        observed_tx,
    );
    let status = Arc::new(parking_lot::Mutex::new(DiscoverState::default()));
    status.lock().reset_session(SearchSession::current());
    status.lock().editor.insert_str("compiler");
    let ticket = status
        .lock()
        .begin_search(SearchSession::current())
        .unwrap();
    let status_for_request = Arc::downgrade(&status);
    let client = Arc::new(client);
    let request =
        search_request::launch_search(ticket, Some(client.clone()), move |ticket, result| {
            if let Some(status) = status_for_request.upgrade() {
                status.lock().complete(ticket, result);
            }
            None
        });

    let incoming = tokio::time::timeout(Duration::from_secs(2), server.recv())
        .await
        .expect("search request must reach transport")
        .unwrap();
    let IncomingMessage::Request { id, method, params } = incoming else {
        panic!("expected plugin search request, got {incoming:?}");
    };
    assert_eq!(method, "plugin/search");
    assert_eq!(
        params,
        json!({"query":"compiler", "sessionId":"plugin-search-session"})
    );
    assert!(
        discover_text(&status.lock()).contains(&crate::i18n::tr("panel-plugin-search-loading"))
    );

    if fail {
        server
            .send_response(id, Err(AcpError::new(-32603, "catalog unavailable")))
            .await
            .unwrap();
    } else {
        // The host pushes this notification before its response; observe the
        // actual bridge completion separately from the request future.
        server
            .send_notification(
                "peri/unstable_event",
                json!({
                    "sessionId":"plugin-search-session", "event":"plugin-search-result",
                    "data":{"query":"compiler", "from_cache":true, "results":[{
                        "name":"compiler-helper", "version":"1.2.3", "enabled":false,
                        "root":"", "description":"Compiler tooling", "marketplace":"fixture",
                        "author":null, "skills_count":0, "commands_count":0,
                        "agents_count":0, "mcp_count":0, "install_scope":"user"
                    }]}
                }),
            )
            .await
            .unwrap();
        server
            .send_response(
                id,
                Ok(json!({"results":[{
                    "name":"compiler-helper", "version":"1.2.3",
                    "description":"Compiler tooling", "marketplace":"fixture"
                }]})),
            )
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(2), observed_rx.recv())
                .await
                .expect("legacy search notification must pass through the bridge")
                .expect("bridge observation channel remains open")
        );
    }
    tokio::time::timeout(Duration::from_secs(2), request)
        .await
        .expect("search response settles its request task")
        .unwrap();
    if !fail {
        // A delayed, same-query legacy notification has no request identity.
        // Even after bridge dispatch it must not replace the owned response.
        server
            .send_notification(
                "peri/unstable_event",
                json!({
                    "sessionId":"plugin-search-session", "event":"plugin-search-result",
                    "data":{"query":"compiler", "from_cache":true, "results":[{
                        "name":"late-unowned-result", "version":"1", "enabled":false,
                        "root":"", "description":"Old result", "marketplace":"fixture",
                        "author":null, "skills_count":0, "commands_count":0,
                        "agents_count":0, "mcp_count":0, "install_scope":"user"
                    }]}
                }),
            )
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(2), observed_rx.recv())
                .await
                .unwrap()
                .unwrap()
        );
        assert!(!discover_text(&status.lock()).contains("late-unowned-result"));
    }
    let text = discover_text(&status.lock());
    if fail {
        let retry = launch_owned(&status, &client);
        let id = receive_search(&server).await;
        server
            .send_response(id, Ok(response("retry-result")))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), retry)
            .await
            .unwrap()
            .unwrap();
        assert!(discover_text(&status.lock()).contains("retry-result"));
    }
    shutdown.cancel();
    notifier.await.unwrap();
    bridge.await.unwrap();
    text
}

#[tokio::test]
#[serial]
async fn plugin_search_completion_shows_results_and_search_input() {
    let _atoms = SearchAtomsGuard::capture();
    let text = completed_search_view(false).await;
    assert!(
        text.contains("compiler-helper"),
        "a projected result must replace loading: {text}"
    );
    assert!(
        text.contains("> compiler"),
        "the completed search remains editable: {text}"
    );
    assert!(!text.contains(&crate::i18n::tr("panel-plugin-search-loading")));
}

#[tokio::test]
#[serial]
async fn plugin_search_completion_reports_error_and_allows_retry() {
    let _atoms = SearchAtomsGuard::capture();
    let text = completed_search_view(true).await;
    assert!(
        text.contains("catalog unavailable"),
        "request failure must leave loading with an error: {text}"
    );
    assert!(
        text.contains("> compiler"),
        "failed search keeps its input available for retry: {text}"
    );
    assert!(!text.contains(&crate::i18n::tr("panel-plugin-search-loading")));
}

fn item(name: &str) -> super::PluginSearchResultItem {
    super::PluginSearchResultItem {
        name: name.into(),
        version: "1".into(),
        marketplace: "remote-market".into(),
        description: "compiler".into(),
        author: None,
    }
}

fn search_owner() -> Arc<parking_lot::Mutex<DiscoverState>> {
    let mut state = DiscoverState::default();
    state.reset_session(SearchSession::current());
    state.editor.insert_str("compiler");
    Arc::new(parking_lot::Mutex::new(state))
}

fn launch_owned(
    owner: &Arc<parking_lot::Mutex<DiscoverState>>,
    client: &Arc<AcpTuiClient>,
) -> tokio::task::JoinHandle<()> {
    let ticket = owner.lock().begin_search(SearchSession::current()).unwrap();
    let weak = Arc::downgrade(owner);
    search_request::launch_search(ticket, Some(client.clone()), move |ticket, result| {
        if let Some(owner) = weak.upgrade() {
            owner.lock().complete(ticket, result);
        }
        None
    })
}

async fn receive_search(
    server: &peri_acp::transport::mpsc::MpscServerTransport,
) -> peri_acp::transport::types::RequestId {
    let incoming = tokio::time::timeout(Duration::from_secs(2), server.recv())
        .await
        .unwrap()
        .unwrap();
    let IncomingMessage::Request { id, method, params } = incoming else {
        panic!("expected search request");
    };
    assert_eq!(method, "plugin/search");
    assert_eq!(params["query"], "compiler");
    id
}

fn response(name: &str) -> serde_json::Value {
    json!({"results":[{"name":name,"version":"1","description":"compiler","marketplace":"remote-market"}]})
}

#[tokio::test]
#[serial]
async fn plugin_search_same_query_late_response_cannot_replace_newer_result() {
    let _atoms = SearchAtomsGuard::capture();
    *ACTIVE_SESSION_ID.state().write() = "same-query".into();
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("same-query", true);
    let client = Arc::new(client);
    let owner = search_owner();
    let first = launch_owned(&owner, &client);
    let first_id = receive_search(&server).await;
    let second = launch_owned(&owner, &client);
    let second_id = receive_search(&server).await;
    server
        .send_response(second_id, Ok(response("new-result")))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), second)
        .await
        .unwrap()
        .unwrap();
    // The transport may reject a response whose superseded request was cancelled.
    let _ = server
        .send_response(first_id, Ok(response("old-result")))
        .await;
    tokio::time::timeout(Duration::from_secs(2), first)
        .await
        .unwrap()
        .unwrap();
    let text = discover_text(&owner.lock());
    assert!(text.contains("new-result"));
    assert!(!text.contains("old-result"));
}

#[tokio::test]
#[serial]
async fn plugin_search_session_switch_and_close_invalidate_pending_response() {
    let _atoms = SearchAtomsGuard::capture();
    for reason in ["switch", "reset", "close"] {
        *ACTIVE_SESSION_ID.state().write() = "before".into();
        let (transport, server) = mpsc_transport_pair();
        let (client, _, _) = AcpTuiClient::new(transport);
        client.force_stable_for_test("before", true);
        let owner = search_owner();
        let request = launch_owned(&owner, &Arc::new(client));
        let id = receive_search(&server).await;
        match reason {
            "close" => owner.lock().close(),
            "reset" => {
                *BRIDGE_RESET_COUNTER.state().write() += 1;
                owner.lock().reset_session(SearchSession::current());
            }
            _ => {
                *ACTIVE_SESSION_ID.state().write() = "after".into();
                owner.lock().reset_session(SearchSession::current());
            }
        }
        let _ = server.send_response(id, Ok(response("stale-result"))).await;
        tokio::time::timeout(Duration::from_secs(2), request)
            .await
            .unwrap()
            .unwrap();
        assert!(!discover_text(&owner.lock()).contains("stale-result"));
        assert_eq!(owner.lock().editor.all_text(), "");
        assert_eq!(owner.lock().status, super::SearchState::Idle);
    }
}

#[tokio::test]
#[serial]
async fn plugin_search_dropped_owner_cancels_task_without_resurrecting_state() {
    let _atoms = SearchAtomsGuard::capture();
    *ACTIVE_SESSION_ID.state().write() = "dropped".into();
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("dropped", true);
    let owner = search_owner();
    let weak = Arc::downgrade(&owner);
    let request = launch_owned(&owner, &Arc::new(client));
    let _id = receive_search(&server).await;
    drop(owner);
    tokio::time::timeout(Duration::from_secs(2), request)
        .await
        .expect("owner removal cancels the pending request")
        .unwrap();
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
#[serial]
async fn plugin_search_busy_projection_retries_once_accepted_or_exits_on_cancel() {
    let _atoms = SearchAtomsGuard::capture();
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    // The hook constructor is private to ratatui-kit. Control its delivery
    // refusal at the callback seam while keeping the actual RPC and render.
    for cancel in [false, true] {
        *ACTIVE_SESSION_ID.state().write() = "busy-projection".into();
        let (transport, server) = mpsc_transport_pair();
        let (client, _, _) = AcpTuiClient::new(transport);
        client.force_stable_for_test("busy-projection", true);
        let owner = search_owner();
        let ticket = owner.lock().begin_search(SearchSession::current()).unwrap();
        let weak = Arc::downgrade(&owner);
        let accepted = Arc::new(AtomicBool::new(false));
        let accepted_for_callback = accepted.clone();
        let projections = Arc::new(AtomicUsize::new(0));
        let projections_for_callback = projections.clone();
        let (refused_tx, mut refused_rx) = mpsc::unbounded_channel();
        let request =
            search_request::launch_search(ticket, Some(Arc::new(client)), move |ticket, result| {
                if !accepted_for_callback.load(Ordering::SeqCst) {
                    let _ = refused_tx.send(());
                    return Some(result);
                }
                if let Some(owner) = weak.upgrade() {
                    assert!(owner.lock().complete(ticket, result));
                    projections_for_callback.fetch_add(1, Ordering::SeqCst);
                }
                None
            });
        let id = receive_search(&server).await;
        server
            .send_response(id, Ok(response("delivered-after-render")))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), refused_rx.recv())
            .await
            .expect("completion must encounter the simulated render borrow")
            .unwrap();
        assert!(
            discover_text(&owner.lock()).contains(&crate::i18n::tr("panel-plugin-search-loading"))
        );
        if cancel {
            owner.lock().close();
        } else {
            accepted.store(true, Ordering::SeqCst);
        }
        tokio::time::timeout(Duration::from_secs(2), request)
            .await
            .expect("delivery must finish after acceptance or cancellation")
            .unwrap();
        assert_eq!(projections.load(Ordering::SeqCst), usize::from(!cancel));
        let text = discover_text(&owner.lock());
        assert_eq!(text.contains("delivered-after-render"), !cancel);
        assert!(!text.contains(&crate::i18n::tr("panel-plugin-search-loading")));
    }
}

#[tokio::test]
#[serial]
async fn plugin_search_empty_response_does_not_fall_back_to_local_catalog() {
    let _atoms = SearchAtomsGuard::capture();
    *ACTIVE_SESSION_ID.state().write() = "empty".into();
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("empty", true);
    let owner = search_owner();
    assert_eq!(owner.lock().visible_items(&[item("local-match")]).len(), 1);
    let request = launch_owned(&owner, &Arc::new(client));
    let id = receive_search(&server).await;
    server
        .send_response(id, Ok(json!({"results":[]})))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), request)
        .await
        .unwrap()
        .unwrap();
    assert!(
        owner
            .lock()
            .visible_items(&[item("local-match")])
            .is_empty()
    );
    assert!(
        discover_text(&owner.lock()).contains(&crate::i18n::tr("panel-plugin-search-no-results"))
    );
}

#[tokio::test]
#[serial]
async fn plugin_search_invalid_response_leaves_loading_with_editable_input() {
    let _atoms = SearchAtomsGuard::capture();
    *ACTIVE_SESSION_ID.state().write() = "invalid".into();
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("invalid", true);
    let client = Arc::new(client);
    let owner = search_owner();
    for value in [json!({}), json!({"results":[{"name":"incomplete"}]})] {
        let request = launch_owned(&owner, &client);
        let id = receive_search(&server).await;
        server.send_response(id, Ok(value)).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), request)
            .await
            .unwrap()
            .unwrap();
        let text = discover_text(&owner.lock());
        assert!(text.contains(&crate::i18n::tr("panel-plugin-search-invalid-response")));
        assert!(text.contains("> compiler"));
        assert!(!text.contains(&crate::i18n::tr("panel-plugin-search-loading")));
    }
}

#[test]
#[serial]
fn plugin_search_keyboard_and_mouse_reach_the_same_submission_action() {
    let _atoms = SearchAtomsGuard::capture();
    use super::discover_handler::{SearchAction, SearchEffect, apply, decide};
    use ratatui_kit::crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui_kit::ratatui::layout::Rect;
    let mut state = DiscoverState::default();
    state.reset_session(SearchSession::current());
    let area = Some(Rect::new(0, 0, 80, 20));
    let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
    for c in "compiler".chars() {
        let action = decide(&key(KeyCode::Char(c)), area, &state, &[]);
        assert_eq!(apply(&mut state, action, &[]), SearchEffect::Consumed);
    }
    assert!(
        state.focused,
        "typing makes the remote search entry reachable"
    );
    assert_eq!(
        decide(&key(KeyCode::Enter), area, &state, &[]),
        SearchAction::Submit
    );
    let click = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 3,
        row: 4,
        modifiers: KeyModifiers::NONE,
    });
    state.focused = false;
    assert_eq!(decide(&click, area, &state, &[]), SearchAction::FocusInput);
    assert_eq!(
        apply(&mut state, SearchAction::FocusInput, &[]),
        SearchEffect::Consumed
    );
    assert_eq!(decide(&click, area, &state, &[]), SearchAction::Submit);
    assert_eq!(
        apply(&mut state, SearchAction::Submit, &[]),
        SearchEffect::Submit
    );
}

#[test]
#[serial]
fn plugin_search_remote_selection_uses_the_displayed_identity_for_detail_and_install() {
    let _atoms = SearchAtomsGuard::capture();
    use super::discover_handler::{SearchAction, SearchEffect, apply, decide};
    use ratatui_kit::crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui_kit::ratatui::layout::Rect;
    let local = vec![item("different-local-plugin")];
    for mouse in [false, true] {
        let owner = search_owner();
        let mut state = owner.lock();
        let ticket = state.begin_search(SearchSession::current()).unwrap();
        state.complete(&ticket, Ok(vec![item("remote-selection")]));
        state.focused = false;
        let event = if mouse {
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 3,
                row: 6,
                modifiers: KeyModifiers::NONE,
            })
        } else {
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        };
        let action = decide(&event, Some(Rect::new(0, 0, 80, 20)), &state, &local);
        assert_eq!(apply(&mut state, action, &local), SearchEffect::Consumed);
        assert_eq!(state.detail.as_ref().unwrap().name, "remote-selection");
        assert_eq!(
            apply(&mut state, SearchAction::DetailAction(1), &local),
            SearchEffect::Install {
                item: item("remote-selection"),
                scope: "project"
            }
        );
        assert!(state.detail.is_none());
    }
}
