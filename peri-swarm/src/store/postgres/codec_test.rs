//! Encoding tests for the PostgreSQL state store. No database is contacted.

use std::collections::BTreeMap;

use serde_json::json;

use super::{
    decode, decode_mcp_json, decode_metadata, decode_prompt_cache, encode, mcp_json, metadata_json,
    parse_input_state, parse_item_kind, parse_session_state, parse_turn_status,
    patch_session_params, prompt_cache_json, seq_field, seq_param,
};
use crate::domain::{
    EnvironmentRef, InputState, ItemKind, McpHeader, McpToolSpec, PromptCacheOptions,
    PromptCacheTtl, SessionState, TurnStatus,
};
use crate::store::{StoreError, SwarmSessionPatch};

#[test]
fn session_states_round_trip_and_unknown_text_is_unreadable() {
    for state in [
        SessionState::Creating,
        SessionState::Idle,
        SessionState::Running,
        SessionState::Recovering,
        SessionState::Stopped,
        SessionState::Failed,
    ] {
        assert_eq!(
            parse_session_state(state.as_str()).expect("known state"),
            state
        );
    }
    assert!(matches!(
        parse_session_state("paused"),
        Err(StoreError::Unavailable(detail)) if detail.contains("state=\"paused\"")
    ));
}

#[test]
fn item_kinds_round_trip_and_unknown_text_is_unreadable() {
    for kind in [
        ItemKind::Message,
        ItemKind::Thought,
        ItemKind::ToolCall,
        ItemKind::ToolUpdate,
        ItemKind::ToolResult,
        ItemKind::Receipt,
        ItemKind::Approval,
    ] {
        assert_eq!(parse_item_kind(kind.as_str()).expect("known kind"), kind);
    }
    assert!(matches!(
        parse_item_kind("final_answer"),
        Err(StoreError::Unavailable(detail)) if detail.contains("kind=\"final_answer\"")
    ));
}

#[test]
fn input_states_round_trip_and_unknown_text_is_unreadable() {
    for state in [
        InputState::Accepted,
        InputState::HandedOff,
        InputState::Failed,
    ] {
        assert_eq!(
            parse_input_state(state.as_str()).expect("known state"),
            state
        );
    }
    assert!(parse_input_state("queued").is_err());
}

#[test]
fn turn_statuses_round_trip_and_unknown_text_is_unreadable() {
    for status in [
        TurnStatus::Queued,
        TurnStatus::InProgress,
        TurnStatus::Waiting,
        TurnStatus::Completed,
        TurnStatus::Failed,
        TurnStatus::Cancelled,
    ] {
        assert_eq!(
            parse_turn_status(status.as_str()).expect("known status"),
            status
        );
    }
    assert!(parse_turn_status("done").is_err());
}

#[test]
fn seq_never_wraps_the_signed_range() {
    assert_eq!(seq_param(0, "seq").expect("zero"), 0);
    assert_eq!(
        seq_param(i64::MAX as u64, "seq").expect("i64 max"),
        i64::MAX
    );
    let error = seq_param(i64::MAX as u64 + 1, "seq").expect_err("out of range");
    assert!(error.to_string().contains("64-bit range"), "{error}");
    assert_eq!(seq_field(7, "seq").expect("positive"), 7);
    let negative = seq_field(-1, "seq").expect_err("negative");
    assert!(negative.to_string().contains("negative"), "{negative}");
}

#[test]
fn no_declared_mcp_tools_is_null_and_a_declaration_round_trips() {
    assert_eq!(mcp_json(&[]).expect("empty declaration"), None);
    let mut tool = McpToolSpec::new("deepwiki", "https://mcp.example.test/mcp");
    tool.headers = vec![McpHeader {
        name: "Authorization".to_owned(),
        value: "Bearer caller-supplied".to_owned(),
    }];
    tool.require_approval = Some(json!("never"));
    let text = mcp_json(std::slice::from_ref(&tool))
        .expect("declaration")
        .expect("non-empty declaration");
    assert_eq!(
        decode_mcp_json(Some(&text)).expect("round trip"),
        vec![tool]
    );
    assert!(decode_mcp_json(None)
        .expect("absent declaration")
        .is_empty());
}

#[test]
fn an_empty_metadata_map_is_null_and_values_round_trip() {
    assert_eq!(metadata_json(&BTreeMap::new()).expect("empty"), None);
    let mut metadata = BTreeMap::new();
    metadata.insert("tenant".to_owned(), "acme".to_owned());
    let text = metadata_json(&metadata)
        .expect("metadata")
        .expect("non-empty");
    assert_eq!(decode_metadata(Some(&text)).expect("round trip"), metadata);
    assert!(decode_metadata(None).expect("absent metadata").is_empty());
}

#[test]
fn prompt_cache_options_round_trip() {
    assert_eq!(prompt_cache_json(None).expect("absent"), None);
    for options in [
        PromptCacheOptions::new(PromptCacheTtl::ThirtyMinutes),
        PromptCacheOptions::new(PromptCacheTtl::TwelveHours),
    ] {
        let text = prompt_cache_json(Some(options))
            .expect("encode")
            .expect("some");
        assert_eq!(
            decode_prompt_cache(Some(&text)).expect("round trip"),
            Some(options)
        );
    }
    assert_eq!(decode_prompt_cache(None).expect("absent"), None);
}

#[test]
fn json_helpers_report_their_field() {
    let encoded = encode(&json!({"a": 1}), "item payload").expect("encode");
    assert_eq!(encoded, "{\"a\":1}");
    let decoded: serde_json::Value = decode(&encoded, "item payload").expect("decode");
    assert_eq!(decoded, json!({"a": 1}));
    let error = decode::<serde_json::Value>("{", "item payload").expect_err("bad json");
    assert!(error.to_string().contains("item payload"), "{error}");
}

#[test]
fn an_environment_ref_round_trips_through_its_json_column() {
    let environment = EnvironmentRef::none();
    let text = encode(&environment, "environment").expect("encode");
    let decoded: EnvironmentRef = decode(&text, "environment").expect("decode");
    assert_eq!(decoded, environment);
}

#[test]
fn a_patch_keeps_each_absent_field() {
    let params = patch_session_params(&SwarmSessionPatch {
        logical_id: "logical-1".to_owned(),
        model: None,
        mcp_tools: None,
        metadata: None,
        prompt_cache_options: None,
        last_activity_at: None,
    })
    .expect("params");
    assert_eq!(params.logical_id, "logical-1");
    assert!(!params.set_model);
    assert!(!params.set_mcp);
    assert!(!params.set_metadata);
    assert!(!params.set_prompt_cache);
    assert!(!params.set_last_activity);
}

#[test]
fn a_patch_with_an_empty_metadata_map_clears_the_column() {
    let params = patch_session_params(&SwarmSessionPatch {
        logical_id: "logical-1".to_owned(),
        model: Some("stub-model".to_owned()),
        mcp_tools: Some(Vec::new()),
        metadata: Some(BTreeMap::new()),
        prompt_cache_options: Some(None),
        last_activity_at: Some("2026-01-01T00:00:00Z".to_owned()),
    })
    .expect("params");
    assert!(params.set_model);
    assert_eq!(params.model.as_deref(), Some("stub-model"));
    assert!(params.set_mcp);
    assert_eq!(params.mcp, None);
    assert!(params.set_metadata);
    assert_eq!(params.metadata, None);
    assert!(params.set_prompt_cache);
    assert_eq!(params.prompt_cache, None);
    assert!(params.set_last_activity);
    assert_eq!(
        params.last_activity_at.as_deref(),
        Some("2026-01-01T00:00:00Z")
    );
}
