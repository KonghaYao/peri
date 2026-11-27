use serde_json::json;

use super::{
    session_created_event_id, session_failed_event_id, session_idle_event_id,
    session_in_progress_event_id, turn_completed_event_id, turn_created_event_id,
    turn_error_event_id, turn_failed_event_id, turn_id_for_item, turn_in_progress_event_id,
    turn_item_added_event_id, turn_item_done_event_id, turn_item_part_added_event_id,
    turn_item_part_done_event_id, turn_item_text_done_event_id, turn_output_text_delta_event_id,
    turn_output_text_delta_replay_event_id, unix_from_rfc3339, OfficialEvent, OfficialMessageItem,
    OfficialSession, OfficialTurn, TurnStatus,
};
use crate::domain::{SessionState, SessionStatus};

#[test]
fn turn_id_is_derived_from_the_persisted_user_item() {
    let item_id = "itim_input_0192f0c5-0000-7000-8000-00000000abcd";
    let turn_id = turn_id_for_item(item_id);
    assert_eq!(turn_id, "turn_input_0192f0c5-0000-7000-8000-00000000abcd");
    // Stable: the same persisted row always derives the same id.
    assert_eq!(turn_id_for_item(item_id), turn_id);
    // A row without the `itim_` prefix is still namespaced, never empty.
    assert_eq!(turn_id_for_item("abc"), "turn_abc");
}

#[test]
fn event_ids_are_derived_from_turn_identity_and_item_seq() {
    assert_eq!(turn_created_event_id("turn_a"), "evt_turn_a_created");
    assert_eq!(turn_item_added_event_id(7), "evt_item_7_added");
    assert_eq!(turn_completed_event_id("turn_a"), "evt_turn_a_completed");
    assert_eq!(turn_failed_event_id("turn_a"), "evt_turn_a_failed");
    assert_eq!(session_idle_event_id("turn_a"), "evt_turn_a_idle");
    assert_eq!(
        session_failed_event_id("turn_a"),
        "evt_turn_a_session_failed"
    );

    let turn = OfficialTurn::new("agent-1", "turn_a", TurnStatus::InProgress, 100, None);
    let created = OfficialEvent::turn_created("agent-1", "turn_a", turn);
    assert_eq!(created.event_id(), "evt_turn_a_created");
    assert_eq!(created.event_type(), "agent.session.turn.created");

    let message = OfficialMessageItem::user("itim_input_x", "turn_a", "hi");
    let added = OfficialEvent::turn_item_added(3, "agent-1", "turn_a", message);
    assert_eq!(added.event_id(), "evt_item_3_added");
    assert_eq!(added.event_type(), "agent.session.turn.item.added");
}

#[test]
fn official_events_serialize_with_the_sdk_type_tag() {
    let turn = OfficialTurn::new("agent-1", "turn_a", TurnStatus::Completed, 100, Some(101));
    let completed = OfficialEvent::turn_completed("agent-1", "turn_a", turn);
    let value = serde_json::to_value(&completed).expect("serialize");
    assert_eq!(value["type"], json!("agent.session.turn.completed"));
    assert_eq!(value["event_id"], json!("evt_turn_a_completed"));
    assert_eq!(value["session_id"], json!("agent-1"));
    assert_eq!(value["turn_id"], json!("turn_a"));
    assert_eq!(value["turn"]["status"], json!("completed"));
    assert_eq!(value["turn"]["object"], json!("agent.session.turn"));
    assert_eq!(value["usage"], json!(null));

    let session = OfficialSession::new(
        "agent-1",
        SessionStatus::Idle,
        100,
        101,
        None,
        Some("model-a".to_owned()),
        None,
    );
    let idle = OfficialEvent::session_idle("turn_a", session);
    let value = serde_json::to_value(&idle).expect("serialize");
    assert_eq!(value["type"], json!("agent.session.idle"));
    assert_eq!(value["event_id"], json!("evt_turn_a_idle"));
    assert_eq!(value["session"]["status"], json!("idle"));
    assert_eq!(value["session"]["agent"]["model"], json!("model-a"));
    assert_eq!(value["session"]["agent"]["instructions"], json!(null));
    assert_eq!(value["session"]["metadata"], json!({}));
    assert_eq!(value["session"]["required_actions"], json!([]));
    assert_eq!(value["session"]["vault_ids"], json!([]));

    // Round-trip: the gateway hop decodes the same enum from SSE data.
    let decoded: OfficialEvent = serde_json::from_value(value).expect("deserialize");
    assert_eq!(decoded, idle);
}

#[test]
fn message_items_carry_the_official_content_blocks() {
    let user = OfficialMessageItem::user("itim_input_x", "turn_a", "hello");
    let value = serde_json::to_value(&user).expect("serialize");
    assert_eq!(value["type"], json!("message"));
    assert_eq!(value["role"], json!("user"));
    assert_eq!(value["status"], json!("completed"));
    assert_eq!(value["turn_id"], json!("turn_a"));
    assert_eq!(value["content"][0]["type"], json!("input_text"));
    assert_eq!(value["content"][0]["text"], json!("hello"));

    let assistant = OfficialMessageItem::assistant("itim_msg_x", "turn_a", "hi");
    let value = serde_json::to_value(&assistant).expect("serialize");
    assert_eq!(value["role"], json!("assistant"));
    assert_eq!(value["content"][0]["type"], json!("output_text"));
}

#[test]
fn completion_event_ids_derive_from_the_persisted_facts() {
    assert_eq!(turn_item_done_event_id(7), "evt_item_7_done");
    assert_eq!(
        turn_item_part_added_event_id("itim_msg_x"),
        "evt_item_itim_msg_x_part_added"
    );
    assert_eq!(turn_item_part_done_event_id(7), "evt_item_7_part_done");
    assert_eq!(turn_item_text_done_event_id(7), "evt_item_7_text_done");
    assert_eq!(
        turn_output_text_delta_event_id("itim_msg_x", 2),
        "evt_item_itim_msg_x_delta_2"
    );
    assert_eq!(
        turn_output_text_delta_replay_event_id(7),
        "evt_item_7_text_delta"
    );
    assert_eq!(
        turn_in_progress_event_id("turn_a"),
        "evt_turn_a_in_progress"
    );
    assert_eq!(
        session_in_progress_event_id("turn_a"),
        "evt_turn_a_session_in_progress"
    );
    assert_eq!(
        session_created_event_id("agent-1"),
        "evt_agent-1_session_created"
    );
    assert_eq!(turn_error_event_id("turn_a"), "evt_turn_a_error");

    let turn = OfficialTurn::new("agent-1", "turn_a", TurnStatus::InProgress, 100, None);
    let created = OfficialEvent::session_in_progress(
        "turn_a",
        OfficialSession::new(
            "agent-1",
            SessionStatus::InProgress,
            100,
            100,
            None,
            None,
            None,
        ),
    );
    assert_eq!(created.event_id(), "evt_turn_a_session_in_progress");
    let progress = OfficialEvent::turn_in_progress("agent-1", "turn_a", turn);
    assert_eq!(progress.event_id(), "evt_turn_a_in_progress");
    assert_eq!(progress.event_type(), "agent.session.turn.in_progress");
}

#[test]
fn assistant_added_ids_are_item_keyed_and_live_delta_ids_differ_from_replay() {
    let message = OfficialMessageItem::assistant("itim_msg_x", "turn_a", "hel");
    let added =
        OfficialEvent::turn_assistant_item_added("itim_msg_x", "agent-1", "turn_a", message);
    assert_eq!(added.event_id(), "evt_item_itim_msg_x_added");
    assert_eq!(added.event_type(), "agent.session.turn.item.added");

    let part = OfficialEvent::turn_content_part_added("itim_msg_x", "agent-1", "turn_a", 0, "hel");
    assert_eq!(part.event_id(), "evt_item_itim_msg_x_part_added");

    let live =
        OfficialEvent::turn_output_text_delta_live("itim_msg_x", 2, "agent-1", "turn_a", 0, "lo");
    let replay =
        OfficialEvent::turn_output_text_delta_replay(7, "agent-1", "turn_a", "itim_msg_x", 0, "lo");
    assert_eq!(live.event_type(), "agent.session.turn.output_text.delta");
    assert_eq!(live.event_id(), "evt_item_itim_msg_x_delta_2");
    assert_eq!(replay.event_id(), "evt_item_7_text_delta");
    assert_ne!(
        live.event_id(),
        replay.event_id(),
        "chunks are not persisted, so live and replay delta ids cannot be shared"
    );
    let value = serde_json::to_value(&live).expect("serialize");
    assert_eq!(
        value,
        json!({
            "type": "agent.session.turn.output_text.delta",
            "event_id": "evt_item_itim_msg_x_delta_2",
            "session_id": "agent-1",
            "turn_id": "turn_a",
            "item_id": "itim_msg_x",
            "content_index": 0,
            "output_index": 0,
            "delta": "lo",
        })
    );
}

#[test]
fn completion_frames_match_the_local_sdk_shape() {
    let message =
        OfficialMessageItem::assistant("itim_msg_x", "turn_a", "hi").with_phase(Some("commentary"));
    let done = OfficialEvent::turn_item_done(3, "agent-1", "turn_a", 1, message);
    let value = serde_json::to_value(&done).expect("serialize");
    assert_eq!(value["type"], json!("agent.session.turn.item.done"));
    assert_eq!(value["event_id"], json!("evt_item_3_done"));
    assert_eq!(value["output_index"], json!(1));
    assert_eq!(value["item"]["id"], json!("itim_msg_x"));
    assert_eq!(value["item"]["type"], json!("message"));
    assert_eq!(value["item"]["role"], json!("assistant"));
    assert_eq!(value["item"]["status"], json!("completed"));
    assert_eq!(value["item"]["phase"], json!("commentary"));
    assert_eq!(value["item"]["content"][0]["type"], json!("output_text"));
    assert_eq!(value["item"]["content"][0]["text"], json!("hi"));
    assert_eq!(value["item"]["turn_id"], json!("turn_a"));

    let added = OfficialEvent::turn_content_part_added("itim_msg_x", "agent-1", "turn_a", 1, "hi");
    let value = serde_json::to_value(&added).expect("serialize");
    assert_eq!(
        value["type"],
        json!("agent.session.turn.content_part.added")
    );
    assert_eq!(value["event_id"], json!("evt_item_itim_msg_x_part_added"));
    assert_eq!(value["content_index"], json!(0));
    assert_eq!(value["output_index"], json!(1));
    assert_eq!(
        value["part"],
        json!({ "type": "output_text", "text": "hi" })
    );

    let text = OfficialEvent::turn_output_text_done(3, "agent-1", "turn_a", "itim_msg_x", 1, "hi");
    let value = serde_json::to_value(&text).expect("serialize");
    assert_eq!(value["type"], json!("agent.session.turn.output_text.done"));
    assert_eq!(value["item_id"], json!("itim_msg_x"));
    assert_eq!(value["content_index"], json!(0));
    assert_eq!(value["output_index"], json!(1));
    assert_eq!(value["text"], json!("hi"));
    assert_eq!(value["session_id"], json!("agent-1"));
    assert_eq!(value["turn_id"], json!("turn_a"));

    let error = OfficialEvent::error("turn_a", "agent-1", "boom");
    let value = serde_json::to_value(&error).expect("serialize");
    assert_eq!(value["type"], json!("error"));
    assert_eq!(value["event_id"], json!("evt_turn_a_error"));
    assert_eq!(value["session_id"], json!("agent-1"));
    assert_eq!(
        value["error"],
        json!({
            "code": "internal_error",
            "message": "boom",
            "param": null,
            "type": "internal_error",
        })
    );

    let created = OfficialEvent::session_created(OfficialSession::new(
        "agent-1",
        SessionStatus::Idle,
        100,
        101,
        None,
        None,
        None,
    ));
    let value = serde_json::to_value(&created).expect("serialize");
    assert_eq!(value["type"], json!("agent.session.created"));
    assert_eq!(value["event_id"], json!("evt_agent-1_session_created"));
    assert_eq!(value["session"]["id"], json!("agent-1"));
    assert_eq!(value["session"]["object"], json!("agent.session"));
}

#[test]
fn session_status_projects_the_durable_state_and_open_turn() {
    assert_eq!(
        SessionStatus::project(SessionState::Idle, false, false),
        SessionStatus::Idle
    );
    assert_eq!(
        SessionStatus::project(SessionState::Idle, true, false),
        SessionStatus::InProgress
    );
    assert_eq!(
        SessionStatus::project(SessionState::Running, false, false),
        SessionStatus::InProgress
    );
    assert_eq!(
        SessionStatus::project(SessionState::Failed, false, false),
        SessionStatus::Failed
    );
    // A later turn recovers the session.
    assert_eq!(
        SessionStatus::project(SessionState::Failed, true, false),
        SessionStatus::InProgress
    );
    // States without an official spelling project as idle.
    assert_eq!(
        SessionStatus::project(SessionState::Creating, false, false),
        SessionStatus::Idle
    );
    assert_eq!(
        SessionStatus::project(SessionState::Stopped, false, false),
        SessionStatus::Idle
    );
    assert_eq!(SessionStatus::Idle.as_str(), "idle");
    assert_eq!(SessionStatus::InProgress.as_str(), "in_progress");
    assert_eq!(SessionStatus::Failed.as_str(), "failed");
}

#[test]
fn persisted_rfc3339_timestamps_convert_to_unix_seconds() {
    assert_eq!(unix_from_rfc3339("1970-01-01T00:00:00Z"), 0);
    assert_eq!(unix_from_rfc3339("2026-01-01T00:00:00Z"), 1_767_225_600);
    // Malformed input is visibly not an observation, never "now".
    assert_eq!(unix_from_rfc3339("not-a-timestamp"), 0);
    assert_eq!(unix_from_rfc3339(""), 0);
}

#[test]
fn official_session_carries_persisted_metadata_and_prompt_cache_options() {
    let session = OfficialSession::new(
        "agent-1",
        SessionStatus::Idle,
        100,
        101,
        None,
        Some("model-a".to_owned()),
        None,
    )
    .with_metadata([("team".to_owned(), "alpha".to_owned())].into())
    .with_prompt_cache_options(Some(crate::domain::PromptCacheOptions::new(
        crate::domain::PromptCacheTtl::TwelveHours,
    )));
    let value = serde_json::to_value(&session).expect("serialize");
    assert_eq!(value["metadata"], json!({ "team": "alpha" }));
    assert_eq!(value["prompt_cache_options"], json!({ "ttl": "12h" }));

    // No configured value: the optional field is absent, never `null`.
    let session = OfficialSession::new("agent-1", SessionStatus::Idle, 0, 0, None, None, None);
    let value = serde_json::to_value(&session).expect("serialize");
    assert!(value.get("prompt_cache_options").is_none());
}

#[test]
fn official_sessions_carry_the_recorded_environment() {
    let session = OfficialSession::new("agent-1", SessionStatus::Idle, 0, 0, None, None, None)
        .with_environment(crate::domain::EnvironmentRef {
            kind: crate::domain::EnvironmentKind::OpenAiHosted,
            id: Some("env-1".to_owned()),
        });
    let value = serde_json::to_value(&session).expect("serialize");
    assert_eq!(
        value["environment"],
        json!({ "type": "openai_hosted", "id": "env-1" })
    );

    // Unknown environment stays officially `none`; no id is fabricated.
    let session = OfficialSession::new("agent-1", SessionStatus::Idle, 0, 0, None, None, None)
        .with_environment(crate::domain::EnvironmentRef::none());
    let value = serde_json::to_value(&session).expect("serialize");
    assert_eq!(value["environment"], json!({ "type": "none" }));
}

#[test]
fn a_requires_action_snapshot_is_an_official_session_event() {
    let session = OfficialSession::new(
        "agent-1",
        SessionStatus::RequiresAction,
        1,
        2,
        None,
        None,
        None,
    )
    .with_required_actions(vec![json!({
        "type": "mcp_approval_request",
        "request_id": "appr_1",
        "turn_id": "turn_1",
        "request": {
            "type": "mcp_tool_call",
            "server_label": "server-a",
            "name": "search",
            "arguments": {},
            "message": "MCP tool `search` on server `server-a` requires approval",
        },
    })]);
    let event = OfficialEvent::requires_action(9, session);
    assert_eq!(event.event_type(), "agent.session.requires_action");
    assert_eq!(event.event_id(), "evt_item_9_requires_action");
    let value = serde_json::to_value(&event).expect("serialize");
    assert_eq!(value["type"], "agent.session.requires_action");
    assert_eq!(value["session"]["object"], "agent.session");
    assert_eq!(value["session"]["status"], "requires_action");
    assert_eq!(value["session"]["created_at"], 1);
    assert_eq!(value["session"]["last_active_at"], 2);
    assert_eq!(value["session"]["environment"], json!({ "type": "none" }));
    assert_eq!(value["session"]["metadata"], json!({}));
    assert_eq!(value["session"]["vault_ids"], json!([]));
    assert!(value["session"]["usage"].is_null());
    assert_eq!(
        value["session"]["required_actions"][0]["request_id"],
        "appr_1"
    );
    assert_eq!(
        value["session"]["required_actions"][0]["request"]["type"],
        "mcp_tool_call"
    );
}
