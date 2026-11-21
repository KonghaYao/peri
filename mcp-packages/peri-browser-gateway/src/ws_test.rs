use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::channel::mpsc;
use futures::FutureExt;
use futures::StreamExt;
use tokio::sync::mpsc as tokio_mpsc;

use super::*;
use crate::protocol::{AgentState, BrowserCommand, PageRef, ServerMessage, YieldReason};
use crate::registry::RegistryHandle;
use crate::session::SessionStore;
use crate::stream::{Frame, FrameMeta};

fn page(id: &str) -> PageRef {
    PageRef {
        target_id: id.to_owned(),
        url: format!("https://example.test/{id}"),
        title: format!("Page {id}"),
    }
}

struct Fixture {
    session: Session,
    state: AppState,
    sink: mpsc::UnboundedSender<Message>,
    out: mpsc::UnboundedReceiver<Message>,
    gateway: tokio_mpsc::Receiver<GatewayMessage>,
}

impl Fixture {
    async fn new(pages: Vec<PageRef>) -> Self {
        let registry = Arc::new(RegistryHandle::new());
        // The ownership driver must share the registry handle the test reads
        // from, otherwise yield/reclaim traffic would go to a different queue.
        let sessions = Arc::new(SessionStore::for_tests_with(
            b"secret".to_vec(),
            registry.clone(),
        ));
        let session = sessions
            .register(
                "s1",
                "peri-abc",
                "ws://127.0.0.1:8827",
                pages,
                AgentState::Idle,
            )
            .await;
        let (gateway_tx, gateway_rx) = tokio_mpsc::channel(8);
        registry.bind("s1", gateway_tx).await;
        let state = AppState { sessions, registry };
        let (sink, out) = mpsc::unbounded();
        Self {
            session,
            state,
            sink,
            out,
            gateway: gateway_rx,
        }
    }

    async fn send(&mut self, text: &str) {
        handle_text(&mut self.sink, &self.session, &self.state, text).await;
    }

    fn next_frame(&mut self) -> Option<Message> {
        self.out.next().now_or_never().flatten()
    }

    async fn next_gateway(&mut self) -> GatewayMessage {
        tokio::time::timeout(Duration::from_secs(1), self.gateway.recv())
            .await
            .expect("gateway message timed out")
            .expect("gateway channel closed")
    }
}

#[tokio::test]
async fn ack_visible_and_unknown_messages_are_silent() {
    let mut fixture = Fixture::new(vec![page("t1")]).await;
    fixture.send(r#"{"t":"ack"}"#).await;
    fixture.send(r#"{"t":"visible","visible":false}"#).await;
    fixture.send(r#"{"t":"from_a_newer_monitor"}"#).await;
    fixture.send("not json").await;

    assert!(fixture.next_frame().is_none());
    assert!(fixture.gateway.try_recv().is_err());
}

#[tokio::test]
async fn switch_updates_the_active_tab() {
    let mut fixture = Fixture::new(vec![page("t1"), page("t2")]).await;
    // The tab strip is session-wide, so the switch is published to every
    // viewer through the session broadcast rather than to the caller alone.
    let mut viewer = fixture.session.subscribe();
    fixture.send(r#"{"t":"switch","pageId":"t2"}"#).await;

    let stored = fixture
        .state
        .sessions
        .get("s1")
        .await
        .expect("session exists");
    assert_eq!(stored.active_page.as_deref(), Some("t2"));

    assert!(fixture.next_frame().is_none());
    let frame = tokio::time::timeout(Duration::from_secs(1), viewer.recv())
        .await
        .expect("tabs frame timed out")
        .expect("tabs frame");
    match frame {
        ServerMessage::Tabs { tabs } => {
            assert_eq!(tabs.len(), 2);
            assert!(!tabs[0].active);
            assert!(tabs[1].active);
        }
        other => panic!("unexpected frame: {other:?}"),
    }
}

#[tokio::test]
async fn switch_to_a_foreign_page_is_ignored() {
    let mut fixture = Fixture::new(vec![page("t1")]).await;
    fixture.send(r#"{"t":"switch","pageId":"elsewhere"}"#).await;
    assert!(fixture.next_frame().is_none());
}

#[tokio::test]
async fn an_accepted_switch_reports_the_page_to_replay() {
    let mut fixture = Fixture::new(vec![page("t1"), page("t2")]).await;

    let action = handle_text(
        &mut fixture.sink,
        &fixture.session,
        &fixture.state,
        r#"{"t":"switch","pageId":"t2"}"#,
    )
    .await;
    assert_eq!(action.switched.as_deref(), Some("t2"));
    assert!(action.visible.is_none());

    let action = handle_text(
        &mut fixture.sink,
        &fixture.session,
        &fixture.state,
        r#"{"t":"switch","pageId":"elsewhere"}"#,
    )
    .await;
    assert!(action.switched.is_none(), "foreign page is not replayed");
}

#[tokio::test]
async fn takeover_requests_a_yield_for_the_active_page() {
    let mut fixture = Fixture::new(vec![page("t1"), page("t2")]).await;
    fixture.send(r#"{"t":"takeover"}"#).await;

    let message = fixture.next_gateway().await;
    assert_eq!(
        message,
        GatewayMessage::YieldRequest {
            session_id: "s1".to_owned(),
            page_id: "t1".to_owned(),
            reason: YieldReason::Takeover,
            timeout_ms: crate::config::DEFAULT_YIELD_TIMEOUT_MS,
        }
    );
}

#[tokio::test]
async fn takeover_without_pages_sends_nothing() {
    let mut fixture = Fixture::new(vec![]).await;
    fixture.send(r#"{"t":"takeover"}"#).await;
    assert!(fixture.gateway.try_recv().is_err());
}

#[tokio::test]
async fn read_only_input_is_rejected_without_reaching_the_mcp() {
    let mut fixture = Fixture::new(vec![page("t1")]).await;
    fixture
        .send(r#"{"t":"mouse","type":"down","x":10,"y":20,"button":"left","clickCount":1}"#)
        .await;
    fixture
        .send(r#"{"t":"key","type":"down","key":"a","code":"KeyA","text":"a","modifiers":0}"#)
        .await;
    fixture
        .send(r#"{"t":"wheel","x":0,"y":0,"deltaX":0,"deltaY":120}"#)
        .await;
    fixture.send(r#"{"t":"text","text":"hello"}"#).await;

    let mut rejected = 0;
    while let Some(frame) = fixture.next_frame() {
        let text = match frame {
            Message::Text(text) => text,
            other => panic!("expected a text error frame, got {other:?}"),
        };
        match serde_json::from_str::<ServerMessage>(text.as_str()).unwrap() {
            ServerMessage::Error { code, .. } => {
                assert_eq!(code, "read_only");
                rejected += 1;
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert_eq!(rejected, 4);
    assert!(fixture.gateway.try_recv().is_err());
}

#[tokio::test]
async fn release_and_answer_without_ownership_report_errors() {
    let mut fixture = Fixture::new(vec![page("t1")]).await;
    fixture.send(r#"{"t":"release"}"#).await;
    fixture
        .send(r#"{"t":"answer","requestId":"r1","accepted":true}"#)
        .await;

    let mut codes = Vec::new();
    while let Some(frame) = fixture.next_frame() {
        let Message::Text(text) = frame else {
            panic!("expected text");
        };
        match serde_json::from_str::<ServerMessage>(text.as_str()).unwrap() {
            ServerMessage::Error { code, .. } => codes.push(code),
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert_eq!(codes, ["read_only", "no_handoff"]);
}

#[tokio::test]
async fn canvas_coordinates_map_to_css_pixels() {
    use crate::stream::FrameMeta;

    let meta = FrameMeta {
        page_id: "t1".to_owned(),
        viewport_width: 640,
        viewport_height: 480,
        device_width: 1280,
        device_height: 960,
        page_scale_factor: 2.0,
        scroll_offset_x: 0.0,
        scroll_offset_y: 0.0,
    };

    // 1280x960 image of a 640x480 CSS viewport: one canvas pixel is half a
    // CSS pixel.
    assert_eq!(
        canvas_to_css(100.0, 200.0, Some(&meta), None),
        (50.0, 100.0)
    );

    // Live layout metrics win over the frame metadata: the window may have
    // been resized since the last frame.
    assert_eq!(
        canvas_to_css(
            1280.0,
            960.0,
            Some(&meta),
            Some(crate::cdp::Viewport {
                width: 320,
                height: 240
            })
        ),
        (319.0, 239.0)
    );

    // A stale canvas cannot aim outside the viewport.
    assert_eq!(
        canvas_to_css(5000.0, -20.0, Some(&meta), None),
        (639.0, 0.0)
    );

    // Without a frame there is nothing to scale against.
    assert_eq!(canvas_to_css(10.0, 20.0, None, None), (10.0, 20.0));
    // A zero-sized layout viewport is ignored, not used as a scale of zero.
    assert_eq!(
        canvas_to_css(
            10.0,
            20.0,
            Some(&meta),
            Some(crate::cdp::Viewport {
                width: 0,
                height: 0
            })
        ),
        (5.0, 10.0)
    );
}

#[tokio::test]
async fn page_commands_are_forwarded_to_the_mcp() {
    let mut fixture = Fixture::new(vec![page("t1"), page("t2")]).await;
    fixture
        .send(r#"{"t":"navigate","url":"https://example.test/next"}"#)
        .await;
    fixture.send(r#"{"t":"history","delta":-1}"#).await;
    fixture.send(r#"{"t":"newTab","url":"about:blank"}"#).await;
    fixture.send(r#"{"t":"closeTab","pageId":"t2"}"#).await;

    let expected = [
        BrowserCommand::Navigate {
            url: "https://example.test/next".to_owned(),
        },
        BrowserCommand::History { delta: -1 },
        BrowserCommand::NewTab {
            url: "about:blank".to_owned(),
        },
        BrowserCommand::CloseTab {
            page_id: "t2".to_owned(),
        },
    ];
    for command in expected {
        match fixture.next_gateway().await {
            GatewayMessage::Command {
                session_id,
                request_id,
                page_id,
                command: actual,
            } => {
                assert_eq!(session_id, "s1");
                assert_eq!(page_id, "t1");
                assert!(!request_id.is_empty());
                assert_eq!(actual, command);
            }
            other => panic!("unexpected gateway message: {other:?}"),
        }
    }
    assert!(fixture.next_frame().is_none());
}

#[tokio::test]
async fn takeover_without_an_mcp_broadcasts_unavailable() {
    let sessions = Arc::new(SessionStore::for_tests(b"secret".to_vec()));
    let session = sessions
        .register(
            "s1",
            "peri-abc",
            "ws://127.0.0.1:8827",
            vec![page("t1")],
            AgentState::Idle,
        )
        .await;
    // The failure is discovered by the driver (registry delivery), so the
    // viewer learns it through the session broadcast the socket loop forwards.
    let mut viewer = session.subscribe();
    let state = AppState {
        sessions,
        registry: Arc::new(RegistryHandle::new()),
    };
    let (mut sink, mut out) = mpsc::unbounded();

    handle_text(&mut sink, &session, &state, r#"{"t":"takeover"}"#).await;
    assert!(out.next().now_or_never().flatten().is_none());

    let frame = tokio::time::timeout(Duration::from_secs(1), viewer.recv())
        .await
        .expect("error frame timed out")
        .expect("error frame");
    match frame {
        ServerMessage::Error { code, message } => {
            assert_eq!(code, "unavailable");
            assert!(message.contains("no agent"), "{message}");
        }
        other => panic!("unexpected frame: {other:?}"),
    }
}

#[tokio::test]
async fn a_frame_is_sent_as_meta_then_binary() {
    let frame = Frame {
        jpeg: Bytes::from_static(b"jpeg"),
        meta: FrameMeta {
            page_id: "t1".to_owned(),
            viewport_width: 1280,
            viewport_height: 720,
            device_width: 800,
            device_height: 600,
            page_scale_factor: 1.0,
            scroll_offset_x: 0.0,
            scroll_offset_y: 0.0,
        },
    };
    let (mut sink, mut out) = mpsc::unbounded();
    assert!(send_frame(&mut sink, &frame).await);

    let Message::Text(meta) = out.next().now_or_never().flatten().expect("meta") else {
        panic!("expected text meta");
    };
    let value: serde_json::Value = serde_json::from_str(meta.as_str()).unwrap();
    assert_eq!(value["t"], "meta");
    assert_eq!(value["pageId"], "t1");
    assert_eq!(value["viewportWidth"], 1280);

    let Message::Binary(jpeg) = out.next().now_or_never().flatten().expect("binary") else {
        panic!("expected binary frame");
    };
    assert_eq!(jpeg.as_ref(), b"jpeg");
}
