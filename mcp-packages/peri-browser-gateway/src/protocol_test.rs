use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::json;

use super::*;

fn assert_roundtrip<T>(value: T)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let encoded = serde_json::to_value(&value).expect("serialize");
    let decoded: T = serde_json::from_value(encoded).expect("deserialize");
    assert_eq!(decoded, value);
}

fn page() -> PageRef {
    PageRef {
        target_id: "t1".to_owned(),
        url: "https://example.test/".to_owned(),
        title: "Example".to_owned(),
    }
}

#[test]
fn register_matches_the_wire_contract() {
    let message = RegistryMessage::Register {
        session_id: "s1".to_owned(),
        name: "peri-abc".to_owned(),
        cdp_url: "ws://127.0.0.1:8827".to_owned(),
        pages: vec![page()],
        state: AgentState::Idle,
    };

    assert_eq!(
        serde_json::to_value(&message).unwrap(),
        json!({
            "type": "register",
            "sessionId": "s1",
            "name": "peri-abc",
            "cdpUrl": "ws://127.0.0.1:8827",
            "pages": [{"targetId": "t1", "url": "https://example.test/", "title": "Example"}],
            "state": "idle"
        })
    );
    assert_roundtrip(message);
}

#[test]
fn registry_updates_match_the_wire_contract() {
    assert_eq!(
        serde_json::to_value(RegistryMessage::UpdatePages {
            session_id: "s1".to_owned(),
            pages: vec![page()],
        })
        .unwrap(),
        json!({"type": "update_pages", "sessionId": "s1", "pages": [
            {"targetId": "t1", "url": "https://example.test/", "title": "Example"}
        ]})
    );

    assert_eq!(
        serde_json::to_value(RegistryMessage::State {
            session_id: "s1".to_owned(),
            state: AgentState::WaitingForHuman,
        })
        .unwrap(),
        json!({"type": "state", "sessionId": "s1", "state": "waiting_for_human"})
    );

    assert_eq!(
        serde_json::to_value(RegistryMessage::YieldOk {
            session_id: "s1".to_owned(),
            page_id: "t1".to_owned(),
        })
        .unwrap(),
        json!({"type": "yield_ok", "sessionId": "s1", "pageId": "t1"})
    );

    assert_eq!(
        serde_json::to_value(RegistryMessage::Reclaim {
            session_id: "s1".to_owned(),
            page_id: "t1".to_owned(),
        })
        .unwrap(),
        json!({"type": "reclaim", "sessionId": "s1", "pageId": "t1"})
    );

    assert_eq!(
        serde_json::to_value(RegistryMessage::CommandResult {
            session_id: "s1".to_owned(),
            request_id: "r1".to_owned(),
            ok: false,
            error: Some("boom".to_owned()),
        })
        .unwrap(),
        json!({
            "type": "command_result",
            "sessionId": "s1",
            "requestId": "r1",
            "ok": false,
            "error": "boom"
        })
    );

    assert_eq!(
        serde_json::to_value(RegistryMessage::Unregister {
            session_id: "s1".to_owned(),
        })
        .unwrap(),
        json!({"type": "unregister", "sessionId": "s1"})
    );
}

#[test]
fn gateway_messages_match_the_wire_contract() {
    let yield_request = GatewayMessage::YieldRequest {
        session_id: "s1".to_owned(),
        page_id: "t1".to_owned(),
        reason: YieldReason::Takeover,
        timeout_ms: 60_000,
    };
    assert_eq!(
        serde_json::to_value(&yield_request).unwrap(),
        json!({
            "type": "yield_request",
            "sessionId": "s1",
            "pageId": "t1",
            "reason": "takeover",
            "timeoutMs": 60000
        })
    );
    assert_roundtrip(yield_request);

    let command = GatewayMessage::Command {
        session_id: "s1".to_owned(),
        request_id: "r1".to_owned(),
        page_id: "t1".to_owned(),
        command: BrowserCommand::Navigate {
            url: "https://example.test/".to_owned(),
        },
    };
    assert_eq!(
        serde_json::to_value(&command).unwrap(),
        json!({
            "type": "command",
            "sessionId": "s1",
            "requestId": "r1",
            "pageId": "t1",
            "command": {"kind": "navigate", "url": "https://example.test/"}
        })
    );
    assert_roundtrip(command);

    let human_request = GatewayMessage::HumanRequest {
        session_id: "s1".to_owned(),
        request_id: "r2".to_owned(),
        page_id: "t1".to_owned(),
        prompt: "Please log in".to_owned(),
        timeout_ms: 300_000,
    };
    assert_eq!(
        serde_json::to_value(&human_request).unwrap(),
        json!({
            "type": "human_request",
            "sessionId": "s1",
            "requestId": "r2",
            "pageId": "t1",
            "prompt": "Please log in",
            "timeoutMs": 300000
        })
    );

    let human_result = GatewayMessage::HumanResult {
        session_id: "s1".to_owned(),
        request_id: "r2".to_owned(),
        status: HumanStatus::Declined,
        url: "https://example.test/".to_owned(),
        title: "Example".to_owned(),
    };
    assert_eq!(
        serde_json::to_value(&human_result).unwrap(),
        json!({
            "type": "human_result",
            "sessionId": "s1",
            "requestId": "r2",
            "status": "declined",
            "url": "https://example.test/",
            "title": "Example"
        })
    );

    let reclaimed = GatewayMessage::Reclaimed {
        session_id: "s1".to_owned(),
        page_id: "t1".to_owned(),
        ok: true,
    };
    assert_eq!(
        serde_json::to_value(&reclaimed).unwrap(),
        json!({"type": "reclaimed", "sessionId": "s1", "pageId": "t1", "ok": true})
    );
}

#[test]
fn browser_commands_match_the_wire_contract() {
    assert_eq!(
        serde_json::to_value(BrowserCommand::History { delta: -1 }).unwrap(),
        json!({"kind": "history", "delta": -1})
    );
    assert_eq!(
        serde_json::to_value(BrowserCommand::NewTab {
            url: "about:blank".to_owned()
        })
        .unwrap(),
        json!({"kind": "new_tab", "url": "about:blank"})
    );
    assert_eq!(
        serde_json::to_value(BrowserCommand::CloseTab {
            page_id: "t1".to_owned()
        })
        .unwrap(),
        json!({"kind": "close_tab", "pageId": "t1"})
    );

    for command in [
        BrowserCommand::History { delta: 2 },
        BrowserCommand::NewTab {
            url: "about:blank".to_owned(),
        },
        BrowserCommand::CloseTab {
            page_id: "t1".to_owned(),
        },
    ] {
        assert_roundtrip(command);
    }
}

#[test]
fn server_messages_match_the_wire_contract() {
    let hello = ServerMessage::Hello {
        session: "s1".to_owned(),
        name: "peri-abc".to_owned(),
        agent: AgentState::Idle,
        interactive: true,
        caps: Caps {
            new_tab: true,
            diagnostics: true,
        },
    };
    assert_eq!(
        serde_json::to_value(&hello).unwrap(),
        json!({
            "t": "hello",
            "session": "s1",
            "name": "peri-abc",
            "agent": "idle",
            "interactive": true,
            "caps": {"newTab": true, "diagnostics": true}
        })
    );

    let tabs = ServerMessage::Tabs {
        tabs: vec![TabInfo {
            id: "t1".to_owned(),
            url: "https://example.test/".to_owned(),
            title: "Example".to_owned(),
            active: true,
        }],
    };
    assert_eq!(
        serde_json::to_value(&tabs).unwrap(),
        json!({
            "t": "tabs",
            "tabs": [{
                "id": "t1",
                "url": "https://example.test/",
                "title": "Example",
                "active": true
            }]
        })
    );

    assert_eq!(
        serde_json::to_value(ServerMessage::Agent {
            state: AgentState::Driving
        })
        .unwrap(),
        json!({"t": "agent", "state": "driving"})
    );

    let nav = ServerMessage::Nav {
        page_id: "t1".to_owned(),
        url: "https://example.test/".to_owned(),
        title: "Example".to_owned(),
        can_go_back: true,
        can_go_forward: false,
    };
    assert_eq!(
        serde_json::to_value(&nav).unwrap(),
        json!({
            "t": "nav",
            "pageId": "t1",
            "url": "https://example.test/",
            "title": "Example",
            "canGoBack": true,
            "canGoForward": false
        })
    );

    let meta = ServerMessage::Meta {
        page_id: "t1".to_owned(),
        viewport_width: 1280,
        viewport_height: 720,
        device_width: 1280,
        device_height: 720,
        page_scale_factor: 1.0,
        scroll_offset_x: 0.0,
        scroll_offset_y: 0.0,
    };
    assert_eq!(
        serde_json::to_value(&meta).unwrap(),
        json!({
            "t": "meta",
            "pageId": "t1",
            "viewportWidth": 1280,
            "viewportHeight": 720,
            "deviceWidth": 1280,
            "deviceHeight": 720,
            "pageScaleFactor": 1.0,
            "scrollOffsetX": 0.0,
            "scrollOffsetY": 0.0
        })
    );

    assert_eq!(
        serde_json::to_value(ServerMessage::Thumb {
            page_id: "t1".to_owned(),
            data: "data:image/jpeg;base64,AA==".to_owned(),
        })
        .unwrap(),
        json!({"t": "thumb", "pageId": "t1", "data": "data:image/jpeg;base64,AA=="})
    );

    assert_eq!(
        serde_json::to_value(ServerMessage::Handoff {
            active: true,
            prompt: Some("Please log in".to_owned()),
            request_id: Some("r1".to_owned()),
        })
        .unwrap(),
        json!({
            "t": "handoff",
            "active": true,
            "prompt": "Please log in",
            "requestId": "r1"
        })
    );

    let console = ServerMessage::Console {
        page_id: "t1".to_owned(),
        entries: vec![ConsoleEntry {
            level: "log".to_owned(),
            text: "hello".to_owned(),
            ts: 0,
        }],
    };
    assert_eq!(
        serde_json::to_value(&console).unwrap(),
        json!({
            "t": "console",
            "pageId": "t1",
            "entries": [{"level": "log", "text": "hello", "ts": 0}]
        })
    );

    let network = ServerMessage::Network {
        page_id: "t1".to_owned(),
        entries: vec![NetworkEntry {
            method: "GET".to_owned(),
            url: "https://example.test/".to_owned(),
            status: Some(200),
            ts: 0,
        }],
    };
    assert_eq!(
        serde_json::to_value(&network).unwrap(),
        json!({
            "t": "network",
            "pageId": "t1",
            "entries": [{
                "method": "GET",
                "url": "https://example.test/",
                "status": 200,
                "ts": 0
            }]
        })
    );

    assert_eq!(
        serde_json::to_value(ServerMessage::Error {
            code: "read_only".to_owned(),
            message: "agent is driving".to_owned(),
        })
        .unwrap(),
        json!({"t": "error", "code": "read_only", "message": "agent is driving"})
    );
}

#[test]
fn client_messages_match_the_wire_contract() {
    assert_eq!(
        serde_json::from_str::<ClientMessage>(r#"{"t":"ack"}"#).unwrap(),
        ClientMessage::Ack
    );
    assert_eq!(
        serde_json::from_str::<ClientMessage>(r#"{"t":"switch","pageId":"t1"}"#).unwrap(),
        ClientMessage::Switch {
            page_id: "t1".to_owned()
        }
    );
    assert_eq!(
        serde_json::from_str::<ClientMessage>(r#"{"t":"visible","visible":false}"#).unwrap(),
        ClientMessage::Visible { visible: false }
    );
    assert_eq!(
        serde_json::from_str::<ClientMessage>(
            r#"{"t":"mouse","type":"down","x":0,"y":0,"button":"left","clickCount":1}"#
        )
        .unwrap(),
        ClientMessage::Mouse {
            event: MouseEventKind::Down,
            x: 0.0,
            y: 0.0,
            button: Some(MouseButton::Left),
            click_count: Some(1),
        }
    );
    assert_eq!(
        serde_json::from_str::<ClientMessage>(
            r#"{"t":"wheel","x":0,"y":0,"deltaX":0,"deltaY":120}"#
        )
        .unwrap(),
        ClientMessage::Wheel {
            x: 0.0,
            y: 0.0,
            delta_x: 0.0,
            delta_y: 120.0,
        }
    );
    assert_eq!(
        serde_json::from_str::<ClientMessage>(
            r#"{"t":"key","type":"down","key":"a","code":"KeyA","text":"a","modifiers":0}"#
        )
        .unwrap(),
        ClientMessage::Key {
            event: KeyEventKind::Down,
            key: "a".to_owned(),
            code: "KeyA".to_owned(),
            text: Some("a".to_owned()),
            modifiers: 0,
        }
    );
    assert_eq!(
        serde_json::from_str::<ClientMessage>(r#"{"t":"text","text":"hello"}"#).unwrap(),
        ClientMessage::Text {
            text: "hello".to_owned()
        }
    );
    assert_eq!(
        serde_json::from_str::<ClientMessage>(r#"{"t":"navigate","url":"about:blank"}"#).unwrap(),
        ClientMessage::Navigate {
            url: "about:blank".to_owned()
        }
    );
    assert_eq!(
        serde_json::from_str::<ClientMessage>(r#"{"t":"history","delta":-1}"#).unwrap(),
        ClientMessage::History { delta: -1 }
    );
    assert_eq!(
        serde_json::from_str::<ClientMessage>(r#"{"t":"newTab","url":"about:blank"}"#).unwrap(),
        ClientMessage::NewTab {
            url: "about:blank".to_owned()
        }
    );
    assert_eq!(
        serde_json::from_str::<ClientMessage>(r#"{"t":"closeTab","pageId":"t1"}"#).unwrap(),
        ClientMessage::CloseTab {
            page_id: "t1".to_owned()
        }
    );
    assert_eq!(
        serde_json::from_str::<ClientMessage>(r#"{"t":"takeover"}"#).unwrap(),
        ClientMessage::Takeover
    );
    assert_eq!(
        serde_json::from_str::<ClientMessage>(r#"{"t":"release"}"#).unwrap(),
        ClientMessage::Release
    );
    assert_eq!(
        serde_json::from_str::<ClientMessage>(r#"{"t":"answer","requestId":"r1","accepted":true}"#)
            .unwrap(),
        ClientMessage::Answer {
            request_id: "r1".to_owned(),
            accepted: true,
        }
    );
}

#[test]
fn unknown_tags_deserialize_as_unknown() {
    assert_eq!(
        serde_json::from_str::<RegistryMessage>(r#"{"type":"future","sessionId":"s1"}"#).unwrap(),
        RegistryMessage::Unknown
    );
    assert_eq!(
        serde_json::from_str::<GatewayMessage>(r#"{"type":"future","sessionId":"s1"}"#).unwrap(),
        GatewayMessage::Unknown
    );
    assert_eq!(
        serde_json::from_str::<ClientMessage>(r#"{"t":"future","x":1}"#).unwrap(),
        ClientMessage::Unknown
    );
    assert_eq!(
        serde_json::from_str::<ServerMessage>(r#"{"t":"future","x":1}"#).unwrap(),
        ServerMessage::Unknown
    );
}

#[test]
fn missing_fields_on_a_known_tag_are_an_error() {
    assert!(
        serde_json::from_str::<RegistryMessage>(r#"{"type":"register","sessionId":"s1"}"#).is_err()
    );
    assert!(serde_json::from_str::<ClientMessage>(r#"{"t":"switch"}"#).is_err());
    assert!(serde_json::from_str::<RegistryMessage>(r#"{"sessionId":"s1"}"#).is_err());
}

#[test]
fn agent_state_strings_are_stable() {
    for (state, text) in [
        (AgentState::Driving, "driving"),
        (AgentState::Idle, "idle"),
        (AgentState::WaitingForHuman, "waiting_for_human"),
    ] {
        assert_eq!(state.as_str(), text);
        assert_eq!(serde_json::to_value(state).unwrap(), json!(text));
    }
}
