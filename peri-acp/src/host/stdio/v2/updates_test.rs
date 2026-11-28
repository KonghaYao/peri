use super::{is_attach_held, project_update, state};
use crate::host::stdio::v2::state::Foreground;
use serde_json::json;

#[test]
fn replay_resets_existing_message_before_appending_and_preserves_canonical_id() {
    let projected = project_update(
        json!({
            "sessionUpdate": "agent_message_chunk", "messageId": "canonical",
            "content": {"type": "text", "text": "hello"}, "_meta": {"private": true}
        }),
        true,
    )
    .unwrap();
    assert_eq!(
        projected[0],
        json!({"sessionUpdate":"agent_message","messageId":"canonical","content":[]})
    );
    assert_eq!(projected[1]["messageId"], projected[0]["messageId"]);
    assert!(projected[1].get("_meta").is_none());
}

#[test]
fn attach_hold_covers_only_session_metadata_updates() {
    for kind in [
        "available_commands_update",
        "config_option_update",
        "current_mode_update",
        "session_info_update",
    ] {
        assert!(
            is_attach_held(&json!({"sessionUpdate": kind})),
            "{kind} must be parked until the setup response"
        );
    }
    for kind in ["state_update", "agent_message_chunk", "tool_call_update"] {
        assert!(
            !is_attach_held(&json!({"sessionUpdate": kind})),
            "{kind} is live activity and must not be parked"
        );
    }
}

#[test]
fn state_updates_carry_the_pinned_state_and_stop_reason() {
    assert_eq!(
        state(Foreground::RequiresAction, None),
        json!({"sessionUpdate":"state_update","state":"requires_action"})
    );
    assert_eq!(
        state(Foreground::Idle, Some("end_turn")),
        json!({"sessionUpdate":"state_update","state":"idle","stopReason":"end_turn"})
    );
    let failed = state(Foreground::Idle, Some("error"));
    assert_eq!(failed["stopReason"], "error");
    assert_eq!(failed["error"]["code"], -32603);
    assert_eq!(
        state(Foreground::Running, None),
        json!({"sessionUpdate":"state_update","state":"running"})
    );
}

#[test]
fn a_plan_becomes_a_whole_list_upsert_under_one_stable_id() {
    let projected = project_update(
        json!({
            "sessionUpdate": "plan",
            "entries": [
                {"content": "first", "priority": "high", "status": "pending",
                 "_meta": {"activeForm": "doing first"}},
                {"content": "second", "priority": "low", "status": "completed"}
            ]
        }),
        false,
    )
    .expect("a plan is projected");
    assert_eq!(
        projected,
        vec![json!({
            "sessionUpdate": "plan_update",
            "plan": {
                "planId": "session-plan",
                "type": "items",
                "entries": [
                    {"content": "first", "priority": "high", "status": "pending"},
                    {"content": "second", "priority": "low", "status": "completed"}
                ]
            }
        })]
    );
    // A malformed plan carries no entries and is not published as an empty one.
    assert!(project_update(json!({"sessionUpdate": "plan"}), false).is_none());
}

#[test]
fn commands_gain_the_v2_kind_and_input_discriminators() {
    let projected = project_update(
        json!({
            "sessionUpdate": "available_commands_update",
            "_meta": {"skillNames": ["x"]},
            "availableCommands": [
                {"name": "compact", "description": "shrink", "_meta": {"periKind": "core"}},
                {"name": "verify", "description": "run", "input": {"hint": "what to verify"}}
            ]
        }),
        false,
    )
    .expect("commands are projected");
    assert_eq!(
        projected,
        vec![json!({
            "sessionUpdate": "available_commands_update",
            "availableCommands": [
                {"type": "command", "name": "compact", "description": "shrink"},
                {"type": "command", "name": "verify", "description": "run",
                 "input": {"type": "text", "hint": "what to verify"}}
            ]
        })]
    );
}

#[test]
fn usage_and_session_info_pass_through_without_private_metadata() {
    assert_eq!(
        project_update(
            json!({"sessionUpdate": "usage_update", "used": 0, "size": 200000,
                   "_meta": {"inputTokens": 0}}),
            false
        )
        .expect("usage is projected"),
        vec![json!({"sessionUpdate": "usage_update", "used": 0, "size": 200000})]
    );
    assert_eq!(
        project_update(
            json!({"sessionUpdate": "session_info_update", "title": "restored",
                   "updatedAt": "2026-01-01T00:00:00Z", "_meta": {"peri": true}}),
            false
        )
        .expect("session info is projected"),
        vec![
            json!({"sessionUpdate": "session_info_update", "title": "restored",
                    "updatedAt": "2026-01-01T00:00:00Z"})
        ]
    );
}

#[test]
fn config_option_updates_rename_identity() {
    assert_eq!(
        project_update(
            json!({
                "sessionUpdate": "config_option_update",
                "_meta": {"private": true},
                "configOptions": [{"id": "mode", "name": "Session Mode", "type": "select",
                                   "currentValue": "default", "category": "mode",
                                   "options": [{"value": "default", "name": "Default"}]}]
            }),
            false
        )
        .expect("config options are projected"),
        vec![json!({
            "sessionUpdate": "config_option_update",
            "configOptions": [{"configId": "mode", "name": "Session Mode", "type": "select",
                               "currentValue": "default", "category": "mode",
                               "options": [{"value": "default", "name": "Default"}]}]
        })]
    );
}

#[test]
fn v1_only_updates_are_dropped_rather_than_rewritten() {
    for update in [
        json!({"sessionUpdate": "current_mode_update", "currentModeId": "default"}),
        json!({"sessionUpdate": "models_update", "models": []}),
        json!({"sessionUpdate": "_peri/private"}),
    ] {
        assert!(project_update(update.clone(), false).is_none(), "{update}");
    }
}
