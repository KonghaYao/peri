//! `session/update` projection and foreground state publication.

use serde_json::{json, Value};

use super::config;
use super::errors;
use super::state::Foreground;

/// Stable plan identity: one session has one todo list, and every `plan` update
/// replaces it whole (pinned `PlanItems` MUST-replace semantics).
const PLAN_ID: &str = "session-plan";

/// Metadata updates that describe the session rather than live turn activity.
///
/// While a setup request is in flight (`Attach::Attaching`) the host may already
/// push these for the session being attached; parking them keeps the client from
/// losing the command table or config projection it would only learn about
/// post-attach. A resume flushes them before its reply, a new session after it
/// (the session id only becomes routable with that reply).
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
        // v2 has no plan-creation update: the first `plan` of a session is an
        // upsert of the whole list under the constant id.
        "plan" => {
            let entries = object.remove("entries")?;
            let entries = entries.as_array()?.clone();
            let entries = entries
                .into_iter()
                .map(strip_entry_meta)
                .collect::<Vec<_>>();
            object.insert("sessionUpdate".into(), json!("plan_update"));
            object.insert(
                "plan".into(),
                json!({"planId": PLAN_ID, "type": "items", "entries": entries}),
            );
        }
        "tool_call" => {
            object.insert("sessionUpdate".into(), json!("tool_call_update"));
        }
        // v1 command entries carry no kind discriminator; v2 requires one.
        "available_commands_update" => {
            if let Some(commands) = object
                .get_mut("availableCommands")
                .and_then(Value::as_array_mut)
            {
                for command in commands.iter_mut() {
                    *command = command_to_v2(std::mem::take(command));
                }
            }
        }
        // v2 renamed the option identity (`id` → `configId`) and select groups
        // (`group` → `groupId`); every other field keeps its v1 shape.
        "config_option_update" => {
            if let Some(options) = object
                .get_mut("configOptions")
                .and_then(Value::as_array_mut)
            {
                for option in options.iter_mut() {
                    *option = config::option_to_v2(std::mem::take(option));
                }
            }
        }
        "tool_call_update" | "session_info_update" | "usage_update" => {}
        // `current_mode_update` and any v1-only kind have no v2 equivalent: v2
        // replaced the mode surface with configuration options.
        _ => return None,
    }
    Some(vec![update])
}

/// A plan entry keeps its pinned fields; `_meta` (the TUI's `activeForm`) never
/// reaches the v2 wire.
fn strip_entry_meta(mut entry: Value) -> Value {
    if let Some(object) = entry.as_object_mut() {
        object.remove("_meta");
    }
    entry
}

/// v1 commands carry no kind discriminator; v2 requires `type: "command"`, and
/// the command input needs its own `type: "text"` tag. Private `_meta` never
/// reaches the v2 wire, so no `peri.*` command metadata is projected either.
fn command_to_v2(mut command: Value) -> Value {
    let Some(object) = command.as_object_mut() else {
        return command;
    };
    object.remove("_meta");
    object.entry("type").or_insert_with(|| json!("command"));
    if let Some(input) = object.get_mut("input").and_then(Value::as_object_mut) {
        input.remove("_meta");
        input.entry("type").or_insert_with(|| json!("text"));
    }
    command
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
            update["error"] = json!({"code": errors::INTERNAL, "message": "Peri execution failed"});
        }
        Some(stop_reason) => update["stopReason"] = json!(stop_reason),
        None => {}
    }
    update
}

#[cfg(test)]
#[path = "updates_test.rs"]
mod tests;
