//! `session/update` projection and foreground state publication.

use serde_json::{json, Value};

use super::state::Foreground;

/// Metadata updates that describe the session rather than live turn activity.
///
/// While a setup request is in flight (`Attach::Attaching`) the host may already
/// push these for the session being attached; holding them until the response is
/// sent keeps the client from losing the command table or config projection it
/// would only learn about post-attach.
pub(super) fn is_attach_held(update: &Value) -> bool {
    matches!(
        update.get("sessionUpdate").and_then(Value::as_str),
        Some(
            "available_commands_update"
                | "config_option_update"
                | "current_mode_update"
                | "session_info_update"
        )
    )
}

// The content conversion upstream already uses canonical message IDs. Never
// guess an ID for a real message or expose private routing metadata.
pub(super) fn project_update(
    mut update: Value,
    resetting_replay_chunk: bool,
) -> Option<Vec<Value>> {
    let kind = update.get("sessionUpdate")?.as_str()?.to_owned();
    let object = update.as_object_mut()?;
    object.remove("_meta");
    match kind.as_str() {
        "agent_message_chunk" | "agent_thought_chunk" | "user_message_chunk" => {
            let id = object.get("messageId")?.as_str()?.to_owned();
            if resetting_replay_chunk {
                let whole = kind.strip_suffix("_chunk")?;
                return Some(vec![
                    json!({"sessionUpdate": whole, "messageId": id, "content": []}),
                    update,
                ]);
            }
        }
        "tool_call" => {
            object.insert("sessionUpdate".into(), json!("tool_call_update"));
        }
        "tool_call_update" | "session_info_update" => {}
        // Legacy plans have no stable plan ID, and several legacy update
        // shapes are incompatible with v2. Do not emit a misleading upsert.
        _ => return None,
    }
    Some(vec![update])
}

/// Project a metadata update that was parked while a session was attaching.
///
/// The live path still keeps the S1 update subset, but an attach-time snapshot is
/// the only one the client may never receive again: dropping it loses the
/// session's command table or configuration state rather than deferring it. The
/// held kinds are therefore projected to their pinned v2 shapes; everything else
/// keeps the live rules (`session_info_update` included).
pub(super) fn project_held(mut update: Value) -> Option<Vec<Value>> {
    let kind = update.get("sessionUpdate")?.as_str()?.to_owned();
    match kind.as_str() {
        "available_commands_update" => {
            if let Some(commands) = update
                .get_mut("availableCommands")
                .and_then(Value::as_array_mut)
            {
                for command in commands.iter_mut() {
                    *command = command_to_v2(std::mem::take(command));
                }
            }
        }
        "config_option_update" => {
            if let Some(options) = update
                .get_mut("configOptions")
                .and_then(Value::as_array_mut)
            {
                for option in options.iter_mut() {
                    *option = config_option_to_v2(std::mem::take(option));
                }
            }
        }
        // v2 replaced the v1 mode surface with configuration options and has no
        // mode update. The pinned `SessionUpdate` reserves unknown kinds for
        // forward compatibility, so the held snapshot passes through unchanged.
        "current_mode_update" => {}
        _ => return project_update(update, false),
    }
    if let Some(object) = update.as_object_mut() {
        object.remove("_meta");
    }
    Some(vec![update])
}

/// v1 commands carry no kind discriminator; v2 requires `type: "command"`.
fn command_to_v2(mut command: Value) -> Value {
    if let Some(object) = command.as_object_mut() {
        object.entry("type").or_insert_with(|| json!("command"));
    }
    command
}

/// v2 renamed the option identity (`id` → `configId`) and select groups
/// (`group` → `groupId`); every other field keeps its v1 shape.
fn config_option_to_v2(mut option: Value) -> Value {
    if let Some(object) = option.as_object_mut() {
        if let Some(id) = object.remove("id") {
            object.entry("configId").or_insert(id);
        }
        if let Some(groups) = object.get_mut("options").and_then(Value::as_array_mut) {
            for group in groups.iter_mut() {
                let Some(group) = group.as_object_mut() else {
                    continue;
                };
                if let Some(id) = group.remove("group") {
                    group.entry("groupId").or_insert(id);
                }
            }
        }
    }
    option
}

/// Foreground state snapshot published as a `state_update`.
pub(super) fn state(foreground: Foreground, stop_reason: Option<&str>) -> Value {
    let mut update = json!({
        "sessionUpdate": "state_update",
        "state": match foreground {
            Foreground::Idle => "idle",
            Foreground::Running => "running",
            Foreground::RequiresAction => "requires_action",
        }
    });
    match stop_reason {
        Some("error") => {
            update["stopReason"] = json!("error");
            update["error"] = json!({"code": -32603, "message": "Peri execution failed"});
        }
        Some(stop_reason) => update["stopReason"] = json!(stop_reason),
        None => {}
    }
    update
}

#[cfg(test)]
#[path = "updates_test.rs"]
mod tests;
