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
