use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use futures::future::BoxFuture;
use serde_json::json;
use tokio::sync::{broadcast, mpsc};
use tokio::time::timeout;

use super::*;
use crate::protocol::{
    AgentState, ConsoleEntry, HumanStatus, KeyEventKind, MouseButton, MouseEventKind, NetworkEntry,
    ServerMessage, YieldReason,
};

fn settings() -> Settings {
    Settings {
        idle_yield: Duration::from_millis(15_000),
        yield_timeout: Duration::from_millis(5_000),
        request_human_timeout: Duration::from_millis(120_000),
        has_peer: true,
    }
}

fn machine() -> Machine {
    Machine::new(
        settings(),
        "s1",
        "peri-abc",
        AgentState::Idle,
        Some("t1".to_owned()),
    )
}

fn mouse() -> ControllerCommand {
    ControllerCommand::Mouse {
        event: MouseEventKind::Down,
        x: 10.0,
        y: 20.0,
        button: Some(MouseButton::Left),
        click_count: 1,
        modifiers: 0,
    }
}

/// Drives the machine through takeover and a successful controller attach.
fn holds(machine: &mut Machine, now: Instant) {
    machine.on(Event::Takeover, now);
    machine.on(Event::YieldOk("t1".to_owned()), now);
    machine.on(
        Event::ControllerOpened {
            page_id: "t1".to_owned(),
            error: None,
        },
        now,
    );
}

#[test]
fn machine_starts_with_the_agent_driving() {
    let machine = machine();
    assert_eq!(machine.state(), OwnershipState::AgentDriving);
    assert!(!machine.is_interactive());
    assert!(!machine.accepts_input());
    assert_eq!(machine.page(), Some("t1"));
    assert_eq!(machine.next_deadline(), None);
}

#[test]
fn idle_yield_needs_a_viewer_and_an_idle_agent() {
    let now = Instant::now();
    let mut machine = machine();

    // No viewer: the deadline is never armed, so taking the controller would
    // only cost a reclaim round trip.
    machine.on(Event::Tick, now + Duration::from_secs(60));
    assert_eq!(machine.state(), OwnershipState::AgentDriving);
    assert_eq!(machine.next_deadline(), None);

    machine.on(Event::ViewerJoined, now);
    assert_eq!(machine.next_deadline(), Some(now + settings().idle_yield));

    // A driving agent clears the deadline again.
    machine.on(Event::AgentState(AgentState::Driving), now);
    assert_eq!(machine.next_deadline(), None);
    machine.on(Event::AgentState(AgentState::Idle), now);
    assert_eq!(machine.next_deadline(), Some(now + settings().idle_yield));
}

#[test]
fn idle_yield_asks_once_and_reports_yield_pending() {
    let now = Instant::now();
    let mut machine = machine();
    machine.on(Event::ViewerJoined, now);

    let effects = machine.on(Event::Tick, now + settings().idle_yield);
    assert_eq!(
        effects,
        vec![Effect::Yield {
            page_id: "t1".to_owned(),
            reason: YieldReason::Idle,
        }]
    );
    assert_eq!(machine.state(), OwnershipState::YieldPending);

    // The MCP did not answer in time: report and stay pending, never assume
    // the page is free (a second controller would be refused anyway).
    let effects = machine.on(
        Event::Tick,
        now + settings().idle_yield + settings().yield_timeout,
    );
    assert_eq!(effects.len(), 1);
    assert!(matches!(
        &effects[0],
        Effect::Broadcast(ServerMessage::Error { code, .. }) if code == "yield_pending"
    ));
    assert_eq!(machine.state(), OwnershipState::YieldPending);
}

#[test]
fn a_takeover_opens_the_controller_only_after_the_mcp_yields() {
    let now = Instant::now();
    let mut machine = machine();

    let effects = machine.on(Event::Takeover, now);
    assert_eq!(
        effects,
        vec![Effect::Yield {
            page_id: "t1".to_owned(),
            reason: YieldReason::Takeover,
        }]
    );
    assert_eq!(machine.state(), OwnershipState::YieldPending);
    assert!(!machine.is_interactive());

    // Input sent in the gap is dropped, not queued for later replay.
    assert!(machine.on(Event::Input(mouse()), now).is_empty());

    let effects = machine.on(Event::YieldOk("t1".to_owned()), now);
    assert_eq!(
        effects,
        vec![Effect::OpenController {
            page_id: "t1".to_owned()
        }]
    );
    // Still no controller: a failed connect must not leave an interactive page.
    assert!(!machine.is_interactive());

    let effects = machine.on(
        Event::ControllerOpened {
            page_id: "t1".to_owned(),
            error: None,
        },
        now,
    );
    assert_eq!(
        effects,
        vec![
            Effect::EnableDiagnostics,
            Effect::RefreshViewport,
            Effect::RefreshNavigation,
            Effect::Broadcast(ServerMessage::Hello {
                session: "s1".to_owned(),
                name: "peri-abc".to_owned(),
                agent: AgentState::Idle,
                interactive: true,
                caps: Caps {
                    new_tab: true,
                    diagnostics: true,
                },
            }),
        ]
    );
    assert_eq!(machine.state(), OwnershipState::HumanHolds);
    assert!(machine.is_interactive());
    assert!(machine.accepts_input());
    assert_eq!(
        machine.on(Event::Input(mouse()), now),
        vec![Effect::Dispatch(mouse())]
    );
}

#[test]
fn a_controller_attach_failure_returns_to_read_only() {
    let now = Instant::now();
    let mut machine = machine();
    machine.on(Event::Takeover, now);
    machine.on(Event::YieldOk("t1".to_owned()), now);

    let effects = machine.on(
        Event::ControllerOpened {
            page_id: "t1".to_owned(),
            error: Some("HTTP 409 Conflict".to_owned()),
        },
        now,
    );
    assert!(matches!(
        &effects[0],
        Effect::Broadcast(ServerMessage::Error { code, .. }) if code == "controller_unavailable"
    ));
    assert_eq!(machine.state(), OwnershipState::AgentDriving);
    assert!(!machine.has_controller());
    assert!(machine.on(Event::Input(mouse()), now).is_empty());
}

#[test]
fn release_closes_the_controller_and_tells_the_mcp() {
    let now = Instant::now();
    let mut machine = machine();
    holds(&mut machine, now);

    let effects = machine.on(Event::Release, now);
    let last = effects.last().expect("effects");
    assert_eq!(
        last,
        &Effect::Reclaimed {
            page_id: "t1".to_owned(),
            ok: true,
        }
    );
    assert!(effects.contains(&Effect::CloseController));
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::Broadcast(ServerMessage::Hello {
            interactive: false,
            ..
        })
    )));
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::Broadcast(ServerMessage::Console { entries, .. }) if entries.is_empty()
    )));
    assert_eq!(machine.state(), OwnershipState::AgentDriving);
    assert!(!machine.has_controller());
}

#[test]
fn a_release_before_the_yield_is_acknowledged_answers_the_late_ack() {
    let now = Instant::now();
    let mut machine = machine();
    machine.on(Event::Takeover, now);
    assert!(machine.on(Event::Release, now).is_empty());
    assert_eq!(machine.state(), OwnershipState::AgentDriving);

    // The page has no writer right now; the MCP must be free to reconnect.
    assert_eq!(
        machine.on(Event::YieldOk("t1".to_owned()), now),
        vec![Effect::Reclaimed {
            page_id: "t1".to_owned(),
            ok: true,
        }]
    );
    assert!(!machine.has_controller());
}

#[test]
fn release_without_a_controller_is_an_error_not_a_silent_success() {
    let now = Instant::now();
    let mut machine = machine();
    let effects = machine.on(Event::Release, now);
    assert!(matches!(
        &effects[0],
        Effect::Broadcast(ServerMessage::Error { code, .. }) if code == "read_only"
    ));
}

#[test]
fn reclaim_while_holding_closes_the_controller_and_acknowledges() {
    let now = Instant::now();
    let mut machine = machine();
    holds(&mut machine, now);

    let effects = machine.on(Event::Reclaim("t1".to_owned()), now);
    assert!(effects.contains(&Effect::CloseController));
    assert_eq!(
        effects.last(),
        Some(&Effect::Reclaimed {
            page_id: "t1".to_owned(),
            ok: true,
        })
    );
    assert_eq!(machine.state(), OwnershipState::AgentDriving);
}

#[test]
fn a_human_request_broadcasts_the_banner_and_forces_the_yield() {
    let now = Instant::now();
    let mut machine = machine();

    let effects = machine.on(
        Event::HumanRequest {
            request_id: "r1".to_owned(),
            page_id: "t1".to_owned(),
            prompt: "Please log in".to_owned(),
            timeout_ms: 300_000,
        },
        now,
    );
    assert_eq!(
        effects,
        vec![
            Effect::Broadcast(ServerMessage::Handoff {
                active: true,
                prompt: Some("Please log in".to_owned()),
                request_id: Some("r1".to_owned()),
            }),
            Effect::Yield {
                page_id: "t1".to_owned(),
                reason: YieldReason::RequestHuman,
            },
        ]
    );
    assert_eq!(machine.pending_handoff(), Some("r1"));
    // The gateway's own bound, not the MCP's, decides when it gives up.
    assert_eq!(
        machine.handoff_view(),
        Some(PendingHandoff {
            request_id: "r1".to_owned(),
            prompt: "Please log in".to_owned(),
        })
    );

    // A second request while one is pending is ignored.
    assert!(machine
        .on(
            Event::HumanRequest {
                request_id: "r2".to_owned(),
                page_id: "t1".to_owned(),
                prompt: "and approve MFA".to_owned(),
                timeout_ms: 1_000,
            },
            now,
        )
        .is_empty());
    assert_eq!(machine.pending_handoff(), Some("r1"));

    // "I'll take it": the same yield handshake, no result yet.
    let effects = machine.on(
        Event::Answer {
            request_id: "r1".to_owned(),
            accepted: true,
        },
        now,
    );
    assert!(matches!(effects[0], Effect::Yield { .. }));
    assert!(machine.handoff_accepted());
    assert_eq!(
        machine.next_deadline(),
        Some(now + settings().yield_timeout)
    );

    machine.on(Event::YieldOk("t1".to_owned()), now);
    machine.on(
        Event::ControllerOpened {
            page_id: "t1".to_owned(),
            error: None,
        },
        now,
    );
    assert_eq!(machine.state(), OwnershipState::HumanHolds);

    // "Done": the tool call returns `done` with the page state at hand-off.
    let effects = machine.on(
        Event::Answer {
            request_id: "r1".to_owned(),
            accepted: false,
        },
        now,
    );
    assert!(effects.contains(&Effect::HumanResult {
        request_id: "r1".to_owned(),
        status: HumanStatus::Done,
    }));
    assert!(effects.contains(&Effect::Reclaimed {
        page_id: "t1".to_owned(),
        ok: true,
    }));
    assert_eq!(machine.state(), OwnershipState::AgentDriving);
    assert_eq!(machine.pending_handoff(), None);
}

#[test]
fn a_handoff_nobody_answers_times_out_and_returns_the_page() {
    let now = Instant::now();
    let mut machine = machine();
    machine.on(
        Event::HumanRequest {
            request_id: "r1".to_owned(),
            page_id: "t1".to_owned(),
            prompt: "Please log in".to_owned(),
            timeout_ms: 300_000,
        },
        now,
    );

    let effects = machine.on(Event::Tick, now + settings().request_human_timeout);
    assert!(effects.contains(&Effect::HumanResult {
        request_id: "r1".to_owned(),
        status: HumanStatus::Timeout,
    }));
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::Broadcast(ServerMessage::Handoff { active: false, .. })
    )));
    assert_eq!(machine.state(), OwnershipState::AgentDriving);
    // The yield the MCP may still answer is acknowledged instead of leaving
    // the page writerless.
    assert_eq!(
        machine.on(Event::YieldOk("t1".to_owned()), now),
        vec![Effect::Reclaimed {
            page_id: "t1".to_owned(),
            ok: true,
        }]
    );
}

#[test]
fn switching_pages_while_holding_swaps_the_controller() {
    let now = Instant::now();
    let mut machine = machine();
    holds(&mut machine, now);

    let effects = machine.on(Event::Page(Some("t2".to_owned())), now);
    assert_eq!(
        effects,
        vec![
            Effect::CloseController,
            Effect::OpenController {
                page_id: "t2".to_owned()
            },
        ]
    );
    assert!(!machine.has_controller());

    let effects = machine.on(
        Event::ControllerOpened {
            page_id: "t2".to_owned(),
            error: None,
        },
        now,
    );
    assert!(effects.contains(&Effect::EnableDiagnostics));
    assert_eq!(machine.state(), OwnershipState::HumanHolds);
    assert!(machine.is_interactive());
}

#[test]
fn a_page_switch_while_the_controller_connects_closes_the_stale_one() {
    let now = Instant::now();
    let mut machine = machine();
    machine.on(Event::Takeover, now);
    machine.on(Event::YieldOk("t1".to_owned()), now);
    machine.on(Event::Page(Some("t2".to_owned())), now);

    // The connection for t1 landed after the switch: it is not the page the
    // viewer shows, so it is closed instead of becoming the writer.
    let effects = machine.on(
        Event::ControllerOpened {
            page_id: "t1".to_owned(),
            error: None,
        },
        now,
    );
    assert_eq!(effects, vec![Effect::CloseController]);
    assert!(!machine.has_controller());
}

#[test]
fn a_missing_page_while_holding_returns_the_page() {
    let now = Instant::now();
    let mut machine = machine();
    holds(&mut machine, now);

    let effects = machine.on(Event::Page(None), now);
    assert!(effects.contains(&Effect::CloseController));
    assert!(effects.contains(&Effect::Reclaimed {
        page_id: "t1".to_owned(),
        ok: true,
    }));
    assert_eq!(machine.state(), OwnershipState::AgentDriving);
}

#[test]
fn the_last_viewer_leaving_returns_the_page() {
    let now = Instant::now();
    let mut machine = machine();
    machine.on(Event::ViewerJoined, now);
    machine.on(Event::ViewerJoined, now);
    holds(&mut machine, now);

    assert!(machine.on(Event::ViewerLeft, now).is_empty());
    assert_eq!(machine.state(), OwnershipState::HumanHolds);

    let effects = machine.on(Event::ViewerLeft, now);
    assert!(effects.contains(&Effect::CloseController));
    assert!(effects.contains(&Effect::Reclaimed {
        page_id: "t1".to_owned(),
        ok: true,
    }));
    assert_eq!(machine.state(), OwnershipState::AgentDriving);
}

#[test]
fn diagnostics_are_broadcast_only_while_holding_and_cleared_on_exit() {
    let now = Instant::now();
    let mut machine = machine();
    let entry = ConsoleEntry {
        level: "error".to_owned(),
        text: "boom".to_owned(),
        ts: 1,
    };

    assert!(machine.on(Event::Console(entry.clone()), now).is_empty());

    holds(&mut machine, now);
    let effects = machine.on(Event::Console(entry.clone()), now);
    assert_eq!(
        effects,
        vec![Effect::Broadcast(ServerMessage::Console {
            page_id: "t1".to_owned(),
            entries: vec![entry.clone()],
        })]
    );
    assert_eq!(machine.console_entries(), vec![entry]);

    let effects = machine.on(Event::Release, now);
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::Broadcast(ServerMessage::Console { entries, .. }) if entries.is_empty()
    )));
    assert!(machine.console_entries().is_empty());
}

#[test]
fn network_entries_start_unfinished_and_get_their_status() {
    let now = Instant::now();
    let mut machine = machine();
    holds(&mut machine, now);

    let entry = NetworkEntry {
        method: "GET".to_owned(),
        url: "https://example.test/api".to_owned(),
        status: None,
        ts: 1,
    };
    machine.on(
        Event::NetworkStarted {
            request_id: "n1".to_owned(),
            entry: entry.clone(),
        },
        now,
    );
    let effects = machine.on(
        Event::NetworkStatus {
            request_id: "n1".to_owned(),
            status: 200,
        },
        now,
    );
    let entries = machine.network_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].status, Some(200));
    assert!(matches!(
        &effects[0],
        Effect::Broadcast(ServerMessage::Network { entries, .. }) if entries[0].status == Some(200)
    ));
}

#[test]
fn the_diagnostics_buffer_keeps_the_newest_entries() {
    let now = Instant::now();
    let mut machine = machine();
    holds(&mut machine, now);
    for index in 0..(DIAGNOSTICS_CAPACITY + 5) {
        machine.on(
            Event::Console(ConsoleEntry {
                level: "log".to_owned(),
                text: format!("line {index}"),
                ts: u64::try_from(index).unwrap(),
            }),
            now,
        );
    }
    let entries = machine.console_entries();
    assert_eq!(entries.len(), DIAGNOSTICS_CAPACITY);
    assert_eq!(entries[0].text, "line 5");
    assert_eq!(
        entries.last().map(|entry| entry.text.clone()),
        Some(format!("line {}", DIAGNOSTICS_CAPACITY + 4))
    );
}

#[test]
fn a_lost_controller_drops_back_to_read_only() {
    let now = Instant::now();
    let mut machine = machine();
    holds(&mut machine, now);

    let effects = machine.on(Event::ControllerClosed("socket closed".to_owned()), now);
    assert!(matches!(
        &effects[0],
        Effect::Broadcast(ServerMessage::Error { code, .. }) if code == "controller_lost"
    ));
    assert!(effects.contains(&Effect::Reclaimed {
        page_id: "t1".to_owned(),
        ok: true,
    }));
    assert_eq!(machine.state(), OwnershipState::AgentDriving);
}

#[test]
fn console_events_decode_the_cdp_payload() {
    let params = json!({
        "type": "error",
        "args": [{"value": "boom"}, {"description": "Error: x"}]
    });
    assert_eq!(
        console_entry(&params, 7),
        Some(ConsoleEntry {
            level: "error".to_owned(),
            text: "boom Error: x".to_owned(),
            ts: 7,
        })
    );

    let exception = json!({
        "exceptionDetails": {
            "text": "Uncaught",
            "exception": {"description": "TypeError: nope"}
        }
    });
    assert_eq!(
        exception_entry(&exception, 8).map(|entry| (entry.level, entry.text)),
        Some(("error".to_owned(), "TypeError: nope".to_owned()))
    );

    let event = CdpEvent {
        method: "Network.requestWillBeSent".to_owned(),
        params: json!({
            "requestId": "n1",
            "request": {"method": "POST", "url": "https://example.test/login"}
        }),
    };
    match diagnostic_event(&event, 9) {
        Some(Event::NetworkStarted { request_id, entry }) => {
            assert_eq!(request_id, "n1");
            assert_eq!(entry.method, "POST");
            assert_eq!(entry.status, None);
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let event = CdpEvent {
        method: "Network.responseReceived".to_owned(),
        params: json!({"requestId": "n1", "response": {"status": 204}}),
    };
    assert_eq!(
        diagnostic_event(&event, 9),
        Some(Event::NetworkStatus {
            request_id: "n1".to_owned(),
            status: 204,
        })
    );

    assert_eq!(
        diagnostic_event(
            &CdpEvent {
                method: "Page.frameNavigated".to_owned(),
                params: json!({}),
            },
            9
        ),
        None
    );
}

/// A link the page opens must reach the address bar; the gateway only learns
/// about it from `Page.frameNavigated`.
#[test]
fn main_frame_navigations_refresh_the_address_bar() {
    let now = Instant::now();
    let mut machine = machine();
    holds(&mut machine, now);
    assert_eq!(machine.state(), OwnershipState::HumanHolds);

    let event = CdpEvent {
        method: "Page.frameNavigated".to_owned(),
        params: json!({"frame": {"id": "f1", "url": "https://example.test/next"}}),
    };
    let decoded = diagnostic_event(&event, 9).expect("main frame");
    assert_eq!(
        decoded,
        Event::FrameNavigated {
            url: "https://example.test/next".to_owned(),
        }
    );
    assert_eq!(machine.on(decoded, now), vec![Effect::RefreshNavigation]);

    // Iframe swaps are not address-bar material.
    let iframe = CdpEvent {
        method: "Page.frameNavigated".to_owned(),
        params: json!({"frame": {"id": "f2", "parentId": "f1", "url": "https://ads.test/"}}),
    };
    assert_eq!(diagnostic_event(&iframe, 9), None);

    // The URL the machine already reports does not trigger another read.
    let current = machine.navigation().url.clone();
    assert!(machine.on(Event::FrameNavigated { url: current }, now).is_empty());
}

/// Kitesurf's history never learns about `location.assign` navigations, so the
/// back/forward buttons follow the stack the machine keeps.
#[test]
fn the_visit_stack_drives_back_and_forward() {
    let now = Instant::now();
    let mut machine = machine();
    holds(&mut machine, now);

    let navigate = |machine: &mut Machine, url: &str| {
        machine.on(
            Event::Navigation(cdp::NavigationState {
                url: url.to_owned(),
                ..cdp::NavigationState::default()
            }),
            now,
        );
    };
    for url in [
        "https://example.test/",
        "https://example.test/?a=1",
        "https://example.test/?a=2",
    ] {
        navigate(&mut machine, url);
    }
    assert_eq!(machine.navigation().url, "https://example.test/?a=2");
    assert!(machine.navigation().can_go_back);
    assert!(!machine.navigation().can_go_forward);

    // A link the page opens lands in the same stack.
    machine.on(
        Event::FrameNavigated {
            url: "https://example.test/?a=3".to_owned(),
        },
        now,
    );
    assert_eq!(
        machine.step_history(-1),
        Some("https://example.test/?a=2".to_owned())
    );

    // The replay reports the URL the cursor already points at; that must not
    // drop the entries ahead of it.
    navigate(&mut machine, "https://example.test/?a=2");
    assert!(machine.navigation().can_go_back);
    assert!(
        machine.navigation().can_go_forward,
        "forward survives a replay"
    );
    assert_eq!(
        machine.step_history(1),
        Some("https://example.test/?a=3".to_owned())
    );

    // A fresh navigation drops the entries it makes unreachable.
    navigate(&mut machine, "https://example.test/?b=1");
    assert!(!machine.navigation().can_go_forward);
    assert_eq!(
        machine.step_history(-1),
        Some("https://example.test/?a=3".to_owned())
    );

    // Out-of-range steps stay put.
    assert_eq!(machine.step_history(5), None);
    assert_eq!(machine.step_history(-5), None);
}

/// Controller double: records every command and never emits events.
struct FakeController {
    commands: Arc<Mutex<Vec<(String, Value)>>>,
    /// Held so the channel stays open: a closed event stream means "socket
    /// gone" to the driver, which would drop straight back to read-only.
    _events: mpsc::UnboundedSender<CdpEvent>,
    events: mpsc::UnboundedReceiver<CdpEvent>,
    history: Arc<AtomicI64>,
    endpoint: String,
}

#[async_trait]
impl PageController for FakeController {
    async fn command(&self, method: &str, params: Value) -> Result<Value, CdpError> {
        self.commands
            .lock()
            .expect("command log poisoned")
            .push((method.to_owned(), params.clone()));
        Ok(match method {
            "Page.getLayoutMetrics" => {
                json!({"cssLayoutViewport": {"clientWidth": 800, "clientHeight": 600}})
            }
            "Page.getNavigationHistory" => json!({
                "currentIndex": self.history.load(Ordering::SeqCst),
                "entries": [
                    {"id": 1, "url": "https://example.test/", "title": "Example"},
                    {"id": 2, "url": "https://example.test/next", "title": "Next"}
                ]
            }),
            "Runtime.evaluate" => fake_evaluate(&self.history, &params),
            _ => json!({}),
        })
    }

    async fn next_event(&mut self) -> Option<CdpEvent> {
        self.events.recv().await
    }

    fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

/// Models the two expressions the navigation helpers send: `location.assign`
/// commits the document (the fake's history follows) and `location.href` reads
/// it back.
fn fake_evaluate(history: &AtomicI64, params: &Value) -> Value {
    let expression = params
        .get("expression")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if let Some(url) = expression
        .strip_prefix("location.assign(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let url: String = serde_json::from_str(url).expect("string literal");
        history.store(if url.ends_with("/next") { 1 } else { 0 }, Ordering::SeqCst);
        return json!({"result": {"type": "undefined"}});
    }
    match expression {
        "location.href" => json!({"result": {"value": fake_url(history.load(Ordering::SeqCst))}}),
        _ => json!({"result": {"value": "Example"}}),
    }
}

fn fake_url(history: i64) -> &'static str {
    if history == 1 {
        "https://example.test/next"
    } else {
        "https://example.test/"
    }
}

struct Harness {
    store: Arc<OwnershipStore>,
    handle: OwnershipHandle,
    gateway: mpsc::Receiver<GatewayMessage>,
    messages: broadcast::Receiver<ServerMessage>,
    commands: Arc<Mutex<Vec<(String, Value)>>>,
    attached: Arc<Mutex<Vec<String>>>,
}

impl Harness {
    async fn new() -> Self {
        let registry = Arc::new(RegistryHandle::new());
        let (gateway_tx, gateway) = mpsc::channel(32);
        registry.bind("s1", gateway_tx).await;

        let commands: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let attached: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let connector: Connector = {
            let commands = commands.clone();
            let attached = attached.clone();
            Arc::new(move |cdp_url: String, page_id: String| -> BoxFuture<
                'static,
                Result<Box<dyn PageController>, CdpError>,
            > {
                let commands = commands.clone();
                let attached = attached.clone();
                Box::pin(async move {
                    attached
                        .lock()
                        .expect("attach log poisoned")
                        .push(format!("{cdp_url}#{page_id}"));
                    let (sender, receiver) = mpsc::unbounded_channel();
                    Ok(Box::new(FakeController {
                        commands,
                        _events: sender,
                        events: receiver,
                        history: Arc::new(AtomicI64::new(0)),
                        endpoint: "fake://page".to_owned(),
                    }) as Box<dyn PageController>)
                })
            })
        };

        let store = Arc::new(OwnershipStore::with_connector(
            settings(),
            registry,
            connector,
        ));
        let (messages, receiver) = broadcast::channel(64);
        let handle = store.spawn(
            "s1".to_owned(),
            "peri-abc".to_owned(),
            "ws://127.0.0.1:8827".to_owned(),
            AgentState::Idle,
            Some("t1".to_owned()),
            messages,
        );
        Self {
            store,
            handle,
            gateway,
            messages: receiver,
            commands,
            attached,
        }
    }

    async fn next_gateway(&mut self) -> GatewayMessage {
        timeout(Duration::from_secs(1), self.gateway.recv())
            .await
            .expect("gateway message timed out")
            .expect("gateway channel closed")
    }

    /// Reads until a message matching `want` arrives, dropping unrelated
    /// broadcasts (tab strips, diagnostics clearing, ...).
    async fn next_matching(
        &mut self,
        describe: &str,
        want: fn(&ServerMessage) -> bool,
    ) -> ServerMessage {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let message = timeout(remaining, self.messages.recv())
                .await
                .unwrap_or_else(|_| panic!("{describe}: broadcast timed out"))
                .expect("broadcast channel closed");
            if want(&message) {
                return message;
            }
        }
    }

    async fn next_hello(&mut self, describe: &str) -> bool {
        match self
            .next_matching(describe, |message| {
                matches!(message, ServerMessage::Hello { .. })
            })
            .await
        {
            ServerMessage::Hello { interactive, .. } => interactive,
            other => panic!("unexpected message: {other:?}"),
        }
    }

    async fn next_nav(&mut self, describe: &str) -> (String, String, String) {
        match self
            .next_matching(describe, |message| {
                matches!(message, ServerMessage::Nav { .. })
            })
            .await
        {
            ServerMessage::Nav {
                page_id,
                url,
                title,
                ..
            } => (page_id, url, title),
            other => panic!("unexpected message: {other:?}"),
        }
    }

    async fn next_handoff(&mut self, describe: &str) -> (bool, Option<String>, Option<String>) {
        match self
            .next_matching(describe, |message| {
                matches!(message, ServerMessage::Handoff { .. })
            })
            .await
        {
            ServerMessage::Handoff {
                active,
                prompt,
                request_id,
            } => (active, prompt, request_id),
            other => panic!("unexpected message: {other:?}"),
        }
    }

    fn methods(&self) -> Vec<String> {
        self.commands
            .lock()
            .expect("command log poisoned")
            .iter()
            .map(|(method, _)| method.clone())
            .collect()
    }

    fn params(&self, method: &str) -> Vec<Value> {
        self.commands
            .lock()
            .expect("command log poisoned")
            .iter()
            .filter(|(recorded, _)| recorded == method)
            .map(|(_, params)| params.clone())
            .collect()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.store.remove("s1");
    }
}

#[tokio::test]
async fn the_driver_attaches_the_controller_and_holds_the_page() {
    let mut harness = Harness::new().await;
    harness.handle.takeover();

    match harness.next_gateway().await {
        GatewayMessage::YieldRequest {
            session_id,
            page_id,
            reason,
            timeout_ms,
        } => {
            assert_eq!(session_id, "s1");
            assert_eq!(page_id, "t1");
            assert_eq!(reason, YieldReason::Takeover);
            assert_eq!(timeout_ms, 5_000);
        }
        other => panic!("unexpected gateway message: {other:?}"),
    }

    harness.handle.yield_ok("t1");
    assert!(harness.next_hello("interactive hello").await);

    // The controller is attached on the writable page endpoint, without
    // `role=viewer`, and diagnostics + viewport + history are refreshed.
    assert_eq!(
        harness.attached.lock().expect("attach log").as_slice(),
        ["ws://127.0.0.1:8827#t1"]
    );
    let methods = harness.methods();
    for expected in [
        "Runtime.enable",
        "Network.enable",
        "Page.getLayoutMetrics",
        "Page.getNavigationHistory",
    ] {
        assert!(
            methods.iter().any(|method| method == expected),
            "{methods:?}"
        );
    }
    assert_eq!(
        harness.handle.snapshot().viewport,
        Some(crate::cdp::Viewport {
            width: 800,
            height: 600
        })
    );

    let (page_id, url, title) = harness.next_nav("navigation state").await;
    assert_eq!(page_id, "t1");
    assert_eq!(url, "https://example.test/");
    assert_eq!(title, "Example");
}

#[tokio::test]
async fn the_driver_dispatches_input_only_while_holding() {
    let mut harness = Harness::new().await;

    // Not holding: the driver drops input even if it arrives.
    harness
        .handle
        .input(ControllerCommand::Text("ignored".to_owned()));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!harness
        .methods()
        .iter()
        .any(|method| method == "Input.insertText"));

    harness.handle.takeover();
    assert!(matches!(
        harness.next_gateway().await,
        GatewayMessage::YieldRequest { .. }
    ));
    harness.handle.yield_ok("t1");
    assert!(harness.next_hello("interactive hello").await);

    harness
        .handle
        .input(ControllerCommand::Text("hello".to_owned()));
    harness.handle.input(mouse());
    harness.handle.input(ControllerCommand::Key {
        event: KeyEventKind::Down,
        key: "a".to_owned(),
        code: "KeyA".to_owned(),
        text: Some("a".to_owned()),
        modifiers: 0,
    });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        let methods = harness.methods();
        if methods.iter().any(|method| method == "Input.insertText")
            && methods
                .iter()
                .any(|method| method == "Input.dispatchMouseEvent")
            && methods
                .iter()
                .any(|method| method == "Input.dispatchKeyEvent")
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "input was not dispatched: {methods:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert_eq!(
        harness.params("Input.insertText"),
        vec![json!({"text": "hello"})]
    );
    let mouse_params = &harness.params("Input.dispatchMouseEvent")[0];
    assert_eq!(mouse_params["type"], "mousePressed");
    assert_eq!(mouse_params["x"], 10.0);
    assert_eq!(mouse_params["y"], 20.0);
}

#[tokio::test]
async fn navigation_goes_to_the_controller_and_is_reported() {
    let mut harness = Harness::new().await;
    harness.handle.takeover();
    assert!(matches!(
        harness.next_gateway().await,
        GatewayMessage::YieldRequest { .. }
    ));
    harness.handle.yield_ok("t1");
    assert!(harness.next_hello("interactive hello").await);
    assert_eq!(harness.next_nav("navigation state").await.0, "t1");

    harness.handle.input(ControllerCommand::Navigate(
        "https://example.test/next".to_owned(),
    ));
    let (_, url, _) = harness.next_nav("navigation after navigate").await;
    assert_eq!(url, "https://example.test/next");
    assert!(
        harness.params("Page.navigate").is_empty(),
        "Page.navigate wedges Kitesurf, so the driver must not use it"
    );
    assert!(
        harness.params("Runtime.evaluate").iter().any(|params| {
            params["expression"] == json!("location.assign(\"https://example.test/next\")")
        }),
        "{:?}",
        harness.params("Runtime.evaluate")
    );

    // The machine kept the two visits, so stepping back replays the first one
    // through the same `location.assign` path the address bar uses.
    harness.handle.input(ControllerCommand::History(-1));
    let (_, url, _) = harness.next_nav("navigation after history step").await;
    assert_eq!(url, "https://example.test/");
    assert!(
        harness.params("Runtime.evaluate").iter().any(|params| {
            params["expression"] == json!("location.assign(\"https://example.test/\")")
        }),
        "{:?}",
        harness.params("Runtime.evaluate")
    );
}

#[tokio::test]
async fn a_human_request_reaches_the_viewer_and_returns_a_result() {
    let mut harness = Harness::new().await;
    harness
        .handle
        .human_request("r1", "t1", "Please log in", 300_000);

    let (active, prompt, request_id) = harness.next_handoff("handoff banner").await;
    assert!(active);
    assert_eq!(prompt.as_deref(), Some("Please log in"));
    assert_eq!(request_id.as_deref(), Some("r1"));
    assert!(matches!(
        harness.next_gateway().await,
        GatewayMessage::YieldRequest {
            reason: YieldReason::RequestHuman,
            ..
        }
    ));
    assert_eq!(
        harness.handle.snapshot().handoff,
        Some(PendingHandoff {
            request_id: "r1".to_owned(),
            prompt: "Please log in".to_owned(),
        })
    );

    harness.handle.answer("r1", true);
    // Accepting re-sends the same yield request: the first one may have raced
    // the MCP while it was still finishing a call.
    assert!(matches!(
        harness.next_gateway().await,
        GatewayMessage::YieldRequest {
            reason: YieldReason::RequestHuman,
            ..
        }
    ));
    harness.handle.yield_ok("t1");
    assert!(harness.next_hello("interactive hello").await);
    assert!(matches!(
        harness.next_nav("navigation state").await.0.as_str(),
        "t1"
    ));

    harness.handle.answer("r1", false);
    match harness.next_gateway().await {
        GatewayMessage::HumanResult {
            session_id,
            request_id,
            status,
            url,
            title,
        } => {
            assert_eq!(session_id, "s1");
            assert_eq!(request_id, "r1");
            assert_eq!(status, HumanStatus::Done);
            assert_eq!(url, "https://example.test/");
            assert_eq!(title, "Example");
        }
        other => panic!("unexpected gateway message: {other:?}"),
    }
    assert!(matches!(
        harness.next_gateway().await,
        GatewayMessage::Reclaimed { ok: true, .. }
    ));
    assert_eq!(
        harness.handle.snapshot().state,
        OwnershipState::AgentDriving
    );
}

#[tokio::test]
async fn the_driver_closes_the_controller_on_reclaim() {
    let mut harness = Harness::new().await;
    harness.handle.takeover();
    assert!(matches!(
        harness.next_gateway().await,
        GatewayMessage::YieldRequest { .. }
    ));
    harness.handle.yield_ok("t1");
    assert!(harness.next_hello("interactive hello").await);

    harness.handle.reclaim("t1");
    match harness.next_gateway().await {
        GatewayMessage::Reclaimed { page_id, ok, .. } => {
            assert_eq!(page_id, "t1");
            assert!(ok);
        }
        other => panic!("unexpected gateway message: {other:?}"),
    }
    assert!(!harness.next_hello("read-only hello").await);
    let snapshot = harness.handle.snapshot();
    assert_eq!(snapshot.state, OwnershipState::AgentDriving);
    assert!(!snapshot.has_controller);
    assert_eq!(snapshot.viewport, None);
}

#[tokio::test]
async fn declining_a_handoff_before_taking_it_reports_declined() {
    let mut harness = Harness::new().await;
    harness
        .handle
        .human_request("r1", "t1", "Please log in", 300_000);
    let (active, _, _) = harness.next_handoff("handoff banner").await;
    assert!(active);
    assert!(matches!(
        harness.next_gateway().await,
        GatewayMessage::YieldRequest { .. }
    ));

    harness.handle.answer("r1", false);
    match harness.next_gateway().await {
        GatewayMessage::HumanResult { status, .. } => {
            assert_eq!(status, HumanStatus::Declined);
        }
        other => panic!("unexpected gateway message: {other:?}"),
    }
    assert_eq!(
        harness.handle.snapshot().state,
        OwnershipState::AgentDriving
    );
    assert_eq!(harness.handle.snapshot().handoff, None);
}

#[tokio::test]
async fn a_second_controller_is_never_attached_while_holding() {
    let mut harness = Harness::new().await;
    harness.handle.takeover();
    assert!(matches!(
        harness.next_gateway().await,
        GatewayMessage::YieldRequest { .. }
    ));
    harness.handle.yield_ok("t1");
    assert!(harness.next_hello("interactive hello").await);

    // A retried takeover while holding is a no-op: one controller per session.
    harness.handle.takeover();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(harness.gateway.try_recv().is_err());
    assert_eq!(harness.attached.lock().expect("attach log").len(), 1);
}

#[tokio::test]
async fn a_viewer_alone_never_takes_the_controller() {
    // Guard rail: the driver waits for a takeover or an idle yield; opening a
    // monitor socket is not a request for the writer role.
    let mut harness = Harness::new().await;
    harness.handle.viewer_joined();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!harness.handle.snapshot().interactive());
    assert!(harness.gateway.try_recv().is_err());
    assert!(harness.attached.lock().expect("attach log").is_empty());
}

#[test]
fn a_session_without_a_peer_takes_the_page_without_a_yield() {
    // `--dev` has no MCP that could answer a yield, so waiting out
    // `yield_timeout` would only produce `yield_pending` and a dead monitor.
    let mut machine = Machine::new(
        Settings {
            has_peer: false,
            ..settings()
        },
        "dev",
        "dev",
        AgentState::Idle,
        Some("t1".to_owned()),
    );

    let effects = machine.on(Event::Takeover, Instant::now());
    assert_eq!(machine.state(), OwnershipState::YieldPending);
    assert!(matches!(
        effects.as_slice(),
        [Effect::OpenController { page_id }] if page_id == "t1"
    ));
    // A second click before the controller opens must not invent a peer.
    assert!(machine.on(Event::Takeover, Instant::now()).is_empty());
    assert!(machine.next_deadline().is_none());

    machine.on(
        Event::ControllerOpened {
            page_id: "t1".to_owned(),
            error: None,
        },
        Instant::now(),
    );
    assert_eq!(machine.state(), OwnershipState::HumanHolds);
    assert!(machine.is_interactive());
}

#[test]
fn a_session_without_a_peer_never_arms_the_idle_yield() {
    // The idle timer exists to hand a *driven* page to the human. With no agent
    // there is nobody to ask, so the timer must not fire (and must not spam the
    // viewer with "no agent is connected").
    let now = Instant::now();
    let mut dev = Machine::new(
        Settings {
            has_peer: false,
            ..settings()
        },
        "dev",
        "dev",
        AgentState::Idle,
        Some("t1".to_owned()),
    );
    dev.on(Event::ViewerJoined, now);
    assert!(dev.next_deadline().is_none());

    // Contrast: the same machine with a peer does arm the timer.
    let mut peer = Machine::new(
        settings(),
        "s1",
        "peri-abc",
        AgentState::Idle,
        Some("t1".to_owned()),
    );
    peer.on(Event::ViewerJoined, now);
    assert!(peer.next_deadline().is_some());
}
