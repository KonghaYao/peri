use super::PeriCaps;
use serde_json::json;

#[test]
fn test_default_all_false() {
    let caps = PeriCaps::default();
    assert!(!caps.token_stats);
    assert!(!caps.skill_names);
    assert!(!caps.replay);
    assert!(!caps.source_agent_id);
    assert!(!caps.context_usage);
    assert!(!caps.agent_event);
    assert!(!caps.agent_event_done);
    assert!(!caps.unstable_event);
    assert!(!caps.prediction);
    assert!(!caps.hitl_pending);
}

#[test]
fn test_from_client_meta_all_true() {
    let meta = json!({
        "peri.tokenStats": true,
        "peri.skillNames": true,
        "peri.replay": true,
        "peri.sourceAgentId": true,
        "peri.contextUsage": true,
        "peri.agentEvent": true,
        "peri.agentEventDone": true,
        "peri.unstableEvent": true,
        "peri.prediction": true,
        "peri.hitlPending": true,
    });
    let caps = PeriCaps::from_client_meta(meta.as_object().unwrap());
    assert!(caps.token_stats);
    assert!(caps.skill_names);
    assert!(caps.replay);
    assert!(caps.source_agent_id);
    assert!(caps.context_usage);
    assert!(caps.agent_event);
    assert!(caps.agent_event_done);
    assert!(caps.unstable_event);
    assert!(caps.prediction);
    assert!(caps.hitl_pending);
}

#[test]
fn test_from_client_meta_partial() {
    let meta = json!({
        "peri.tokenStats": true,
        "peri.replay": true,
    });
    let caps = PeriCaps::from_client_meta(meta.as_object().unwrap());
    assert!(caps.token_stats);
    assert!(caps.replay);
    assert!(!caps.skill_names);
    assert!(!caps.source_agent_id);
    assert!(!caps.context_usage);
}

#[test]
fn test_from_client_meta_empty() {
    let empty = serde_json::Map::new();
    let caps = PeriCaps::from_client_meta(&empty);
    assert_eq!(caps, PeriCaps::default());
}

#[test]
fn test_from_client_meta_unknown_keys_ignored() {
    let meta = json!({
        "peri.tokenStats": true,
        "some.unknown": "ignored",
    });
    let caps = PeriCaps::from_client_meta(meta.as_object().unwrap());
    assert!(caps.token_stats);
    assert!(!caps.replay);
}

#[test]
fn test_to_agent_meta_roundtrip() {
    let caps = PeriCaps {
        token_stats: true,
        skill_names: false,
        replay: true,
        source_agent_id: true,
        context_usage: false,
        ..Default::default()
    };
    let meta = caps.to_agent_meta();
    let caps2 = PeriCaps::from_client_meta(&meta);
    assert_eq!(caps, caps2);
}

#[test]
fn test_all_enabled() {
    let caps = PeriCaps::all_enabled();
    assert!(caps.token_stats);
    assert!(caps.skill_names);
    assert!(caps.replay);
    assert!(caps.source_agent_id);
    assert!(caps.context_usage);
    assert!(caps.agent_event);
    assert!(caps.agent_event_done);
    assert!(caps.unstable_event);
    assert!(caps.prediction);
    assert!(caps.hitl_pending);
}
