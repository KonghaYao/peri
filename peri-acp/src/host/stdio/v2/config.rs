//! The ACP v2 configuration surface: option projection and
//! `session/set_config_option`.
//!
//! v1 and v2 `SessionConfigOption` differ only in the identity field (`id` →
//! `configId`) and select groups (`group` → `groupId`). The list itself is always
//! built by `dispatch::config_update::make_config_options` in the host: the
//! adapter renames fields and never invents an option or a value.

use serde_json::{json, Value};

use crate::transport::types::RequestId;

use super::errors;
use super::state::{Connection, Reply, Routed};

/// Rename one v1 option into its v2 shape.
pub(super) fn option_to_v2(mut option: Value) -> Value {
    let Some(object) = option.as_object_mut() else {
        return option;
    };
    object.remove("_meta");
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
    option
}

/// Project the `configOptions` of a v1 response/notification in place.
pub(super) fn project_options(object: &mut serde_json::Map<String, Value>) {
    object.remove("_meta");
    if let Some(options) = object
        .get_mut("configOptions")
        .and_then(Value::as_array_mut)
    {
        for option in options.iter_mut() {
            *option = option_to_v2(std::mem::take(option));
        }
    }
}

/// The projected option list carried by a v1 reply, if any.
pub(super) fn options_from(reply: &Value) -> Vec<Value> {
    reply
        .get("configOptions")
        .and_then(Value::as_array)
        .map(|options| options.iter().cloned().map(option_to_v2).collect())
        .unwrap_or_default()
}

/// `session/set_config_option`.
///
/// The pinned contract is `{configId, type:"id", value}` → v1 `{configId, value}`
/// plus a `-32602` for an unknown option or value that leaves the session
/// configuration untouched. Peri advertises no boolean option, so a `boolean`
/// value type is rejected rather than silently coerced.
pub(super) fn set_config_option(
    connection: &mut Connection,
    id: RequestId,
    params: &Value,
) -> Routed {
    let Some(session_id) = params.get("sessionId").and_then(Value::as_str) else {
        return Routed::Answered(Err(errors::invalid_params("Missing sessionId")));
    };
    let Some(slot) = connection.slot(session_id) else {
        return Routed::Answered(Err(errors::session_not_found(session_id)));
    };
    let Some(config_id) = params.get("configId").and_then(Value::as_str) else {
        return Routed::Answered(Err(errors::invalid_params("Missing configId")));
    };
    let value = match params.get("type").and_then(Value::as_str) {
        Some("id") => match params.get("value").and_then(Value::as_str) {
            Some(value) => value,
            None => {
                return Routed::Answered(Err(errors::invalid_params(
                    "Missing session configuration value",
                )))
            }
        },
        Some("boolean") => {
            return Routed::Answered(Err(errors::invalid_params(
                "Peri ACP v2 Preview advertises no boolean session configuration option",
            )))
        }
        _ => {
            return Routed::Answered(Err(errors::invalid_params(
                "Unsupported session configuration value type",
            )))
        }
    };
    // The option list the host last reported decides what is settable. Rejecting
    // here is what keeps an unknown id or an out-of-range value from reaching the
    // v1 handler, which treats an unknown id as a no-op and maps an unknown mode
    // to the default mode.
    if let Some(option) = slot
        .config
        .iter()
        .find(|option| option.get("configId").and_then(Value::as_str) == Some(config_id))
    {
        if !option_accepts(option, value) {
            return Routed::Answered(Err(errors::invalid_params(format!(
                "Invalid value for session configuration option {config_id}"
            ))));
        }
    } else if !slot.config.is_empty() {
        return Routed::Answered(Err(errors::invalid_params(format!(
            "Unknown session configuration option {config_id}"
        ))));
    }
    connection.replies.insert(id.clone(), Reply::ConfigOptions);
    Routed::Forward {
        id,
        method: "session/set_config_option".into(),
        params: json!({"sessionId": session_id, "configId": config_id, "value": value}),
    }
}

/// Whether a select option lists `value` (ungrouped values or a group's values).
///
/// Options the preview does not understand are forwarded untouched: the pinned
/// rule for an unknown type is to ignore it, not to reject it.
fn option_accepts(option: &Value, value: &str) -> bool {
    if option.get("type").and_then(Value::as_str) != Some("select") {
        return true;
    }
    let Some(options) = option.get("options").and_then(Value::as_array) else {
        return true;
    };
    options.iter().any(|entry| {
        entry.get("value").and_then(Value::as_str) == Some(value)
            || entry
                .get("options")
                .and_then(Value::as_array)
                .is_some_and(|group| {
                    group
                        .iter()
                        .any(|inner| inner.get("value").and_then(Value::as_str) == Some(value))
                })
    })
}

/// The request id that asks the host for a session's current option list.
///
/// `session/resume` has no v1 `configOptions` in its response, so the adapter
/// fetches the same list `session/new` returns before answering the client.
pub(super) fn fetch_params(session_id: &str) -> Value {
    json!({ "sessionId": session_id })
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
