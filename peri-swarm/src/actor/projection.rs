//! ACP notification → Swarm item projection (`design §6.1`).
//!
//! This module is deliberately **pure**: it consumes the JSON wire shape of
//! `session/update`, `peri/agent_event`, and `peri/agent_event_done` and
//! produces transport-neutral projections. It never imports `peri-acp`, so the
//! only module that owns the ACP dependency stays `actor/runtime.rs`.
//!
//! Ordering, persistence, and broadcast belong to [`super::reader::ItemLog`];
//! turn accumulation belongs to [`super::service`]. Projection only answers
//! "what did the host say".

use serde_json::{json, Value};

use crate::domain::{ItemKind, ItemSource};

pub const SESSION_UPDATE: &str = "session/update";
pub const AGENT_EVENT: &str = "peri/agent_event";
pub const AGENT_EVENT_DONE: &str = "peri/agent_event_done";

/// One append-only item derived from an ACP notification.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectedItem {
    pub kind: ItemKind,
    pub source: ItemSource,
    pub payload: Value,
}

/// Everything a session driver needs to know from one notification.
#[derive(Clone, Debug, PartialEq)]
pub enum Projected {
    Item(ProjectedItem),
    TurnStarted {
        request_id: String,
        generation: Option<String>,
        /// `true` for a queued (`user_input_run_started`) run: the host runs a
        /// user input that was handed off mid-turn. It continues the open turn
        /// instead of opening a new one.
        queued: bool,
    },
    QueueChanged {
        generation: String,
    },
    TurnCompleted {
        request_id: Option<String>,
        stop_reason: String,
    },
    ExecutionFailed {
        message: String,
    },
}

/// Project one host notification. Unknown methods and replay notifications
/// produce no output instead of an error, so a newer host cannot break an older
/// gateway connection.
#[must_use]
pub fn project(method: &str, params: &Value) -> Vec<Projected> {
    match method {
        SESSION_UPDATE => project_session_update(params),
        AGENT_EVENT => project_agent_event(params),
        AGENT_EVENT_DONE => project_agent_event_done(params),
        _ => Vec::new(),
    }
}

fn project_session_update(params: &Value) -> Vec<Projected> {
    // `session/load` replays persisted history through the same notification
    // channel. `swarm_items` is the single replayable record (`design §6.1`);
    // projecting replayed updates would duplicate every historical item after a
    // reconnect, so replay frames are ignored here.
    //
    // The host stamps `periReplay` on the *update* `_meta`
    // (`ContentChunk._meta`, `dispatch/session_replay.rs`); the params-level
    // check stays for hosts that mark the notification instead.
    if is_replay(params) || params.get("update").is_some_and(is_replay) {
        return Vec::new();
    }
    let Some(update) = params.get("update") else {
        return Vec::new();
    };
    let Some(kind) = update.get("sessionUpdate").and_then(Value::as_str) else {
        return Vec::new();
    };
    let source = source_from_meta(params);

    match kind {
        "agent_message_chunk" | "agent_thought_chunk" => {
            let Some(text) = update
                .get("content")
                .and_then(|content| content.get("text"))
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            else {
                return Vec::new();
            };
            let Some(message_id) = update.get("messageId").and_then(Value::as_str) else {
                return Vec::new();
            };
            vec![Projected::Item(ProjectedItem {
                kind: if kind == "agent_message_chunk" {
                    ItemKind::Message
                } else {
                    ItemKind::Thought
                },
                source,
                payload: json!({ "messageId": message_id, "text": text }),
            })]
        }
        "tool_call" => {
            let Some(tool_call_id) = update.get("toolCallId").and_then(Value::as_str) else {
                return Vec::new();
            };
            let mut payload = json!({
                "toolCallId": tool_call_id,
                "title": update.get("title").cloned().unwrap_or(Value::Null),
                "status": update.get("status").cloned().unwrap_or(Value::Null),
            });
            // `design §6.1`: exactly one of `input`/`body` describes the call.
            if let Some(input) = update.get("rawInput").filter(|value| !value.is_null()) {
                payload["input"] = input.clone();
            } else if let Some(body) = update.get("content").filter(|value| !value.is_null()) {
                payload["body"] = body.clone();
            }
            if let Some(origin) = update
                .get("_meta")
                .and_then(|meta| meta.get("peri"))
                .and_then(|peri| peri.get("originMessageId"))
            {
                payload["originMessageId"] = origin.clone();
            }
            vec![Projected::Item(ProjectedItem {
                kind: ItemKind::ToolCall,
                source,
                payload,
            })]
        }
        "tool_call_update" => {
            let Some(tool_call_id) = update.get("toolCallId").and_then(Value::as_str) else {
                return Vec::new();
            };
            let terminal = update
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|status| status == "completed" || status == "failed");
            let mut payload = json!({
                "toolCallId": tool_call_id,
                "status": update.get("status").cloned().unwrap_or(Value::Null),
            });
            if let Some(title) = update.get("title").filter(|value| !value.is_null()) {
                payload["title"] = title.clone();
            }
            if let Some(output) = update.get("rawOutput").filter(|value| !value.is_null()) {
                payload["output"] = output.clone();
            }
            vec![Projected::Item(ProjectedItem {
                kind: if terminal || payload.get("output").is_some() {
                    ItemKind::ToolResult
                } else {
                    ItemKind::ToolUpdate
                },
                source,
                payload,
            })]
        }
        // The gateway persists the user message at acceptance, so the host's
        // `user_message_chunk` echo would duplicate it. Command/config/usage
        // updates are not transcript items (`design §6.1` mapping table).
        "user_message_chunk"
        | "usage_update"
        | "plan"
        | "available_commands_update"
        | "current_mode_update"
        | "config_option_update"
        | "session_info_update" => Vec::new(),
        _ => Vec::new(),
    }
}

fn project_agent_event(params: &Value) -> Vec<Projected> {
    let Some(raw) = params.get("event_json").and_then(Value::as_str) else {
        return Vec::new();
    };
    let Ok(event) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let Some(event_type) = event.get("type").and_then(Value::as_str) else {
        return Vec::new();
    };
    let value = event.get("value").cloned().unwrap_or(Value::Null);

    match event_type {
        "execution_started" | "user_input_run_started" => {
            let Some(request_id) = value.get("request_id").and_then(Value::as_str) else {
                return Vec::new();
            };
            let generation = value
                .get("generation")
                .and_then(Value::as_str)
                .map(str::to_owned);
            vec![Projected::TurnStarted {
                request_id: request_id.to_owned(),
                generation,
                queued: event_type == "user_input_run_started",
            }]
        }
        "user_input_queue_changed" => value
            .get("snapshot")
            .and_then(|snapshot| snapshot.get("generation"))
            .and_then(Value::as_str)
            .map(|generation| {
                vec![Projected::QueueChanged {
                    generation: generation.to_owned(),
                }]
            })
            .unwrap_or_default(),
        "agent_execution_failed" => value
            .get("message")
            .and_then(Value::as_str)
            .map(|message| {
                vec![Projected::ExecutionFailed {
                    message: message.to_owned(),
                }]
            })
            .unwrap_or_default(),
        // A child agent completing is not the root turn completing. It is
        // projected as a receipt owned by `child:<instanceId>` so clients can
        // fold child output separately from the root turn lifecycle.
        "subagent_stopped" => {
            child_receipt(value.get("instance_id").and_then(Value::as_str), &event)
        }
        "background_task_completed" => {
            child_receipt(value.get("child_thread_id").and_then(Value::as_str), &event)
        }
        "user_input_delivered" => vec![Projected::Item(ProjectedItem {
            kind: ItemKind::Receipt,
            source: ItemSource::root(),
            payload: json!({ "event": event, "_raw": true }),
        })],
        // `state_snapshot` / `turn_committed` carry whole transcripts; they are
        // not transcript items and would duplicate persisted messages. Other
        // TUI-only events have no Swarm item contract yet.
        _ => Vec::new(),
    }
}

fn child_receipt(instance_id: Option<&str>, event: &Value) -> Vec<Projected> {
    let Some(instance_id) = instance_id.filter(|id| !id.is_empty()) else {
        return Vec::new();
    };
    vec![Projected::Item(ProjectedItem {
        kind: ItemKind::Receipt,
        source: ItemSource {
            agent_id: format!("child:{instance_id}"),
            task_id: None,
        },
        payload: json!({ "event": event, "_raw": true }),
    })]
}

fn project_agent_event_done(params: &Value) -> Vec<Projected> {
    let Some(stop_reason) = params.get("stopReason").and_then(Value::as_str) else {
        let request_id = params
            .get("requestId")
            .and_then(Value::as_str)
            .unwrap_or("");
        // Dropping this silently strands the turn: nothing else can close it.
        tracing::warn!(
            request_id,
            "peri/agent_event_done without stopReason; turn has no terminal"
        );
        return Vec::new();
    };
    vec![Projected::TurnCompleted {
        request_id: params
            .get("requestId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        stop_reason: stop_reason.to_owned(),
    }]
}

/// `_meta.periReplay == true` on a notification or on its update payload.
fn is_replay(value: &Value) -> bool {
    value
        .get("_meta")
        .and_then(|meta| meta.get("periReplay"))
        .and_then(Value::as_bool)
        == Some(true)
}

fn source_from_meta(params: &Value) -> ItemSource {
    match params
        .get("_meta")
        .and_then(|meta| meta.get("peri"))
        .and_then(|peri| peri.get("sourceAgentId"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        Some(child) => ItemSource {
            agent_id: format!("child:{child}"),
            task_id: None,
        },
        None => ItemSource::root(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(value: Value) -> Value {
        json!({ "sessionId": "s1", "update": value })
    }

    #[test]
    fn message_chunks_share_the_message_id_but_get_distinct_payloads() {
        let first = project(
            SESSION_UPDATE,
            &update(json!({
                "sessionUpdate": "agent_message_chunk",
                "messageId": "msg-1",
                "content": { "type": "text", "text": "hel" },
            })),
        );
        let second = project(
            SESSION_UPDATE,
            &update(json!({
                "sessionUpdate": "agent_message_chunk",
                "messageId": "msg-1",
                "content": { "type": "text", "text": "lo" },
            })),
        );
        let Projected::Item(first) = &first[0] else {
            panic!("message chunk must project to an item");
        };
        let Projected::Item(second) = &second[0] else {
            panic!("message chunk must project to an item");
        };
        assert_eq!(first.kind, ItemKind::Message);
        assert_eq!(first.payload["messageId"], "msg-1");
        assert_eq!(first.payload["text"], "hel");
        assert_eq!(second.payload["messageId"], "msg-1");
        assert_eq!(second.payload["text"], "lo");
        assert_eq!(first.source, ItemSource::root());
    }

    #[test]
    fn child_source_is_taken_from_meta() {
        let projected = project(
            SESSION_UPDATE,
            &json!({
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "messageId": "msg-2",
                    "content": { "type": "text", "text": "child" },
                },
                "_meta": { "peri": { "sourceAgentId": "agent-child" } },
            }),
        );
        let Projected::Item(item) = &projected[0] else {
            panic!("expected item");
        };
        assert_eq!(item.source.agent_id, "child:agent-child");
    }

    #[test]
    fn replay_notifications_are_ignored() {
        let projected = project(
            SESSION_UPDATE,
            &json!({
                "sessionId": "s1",
                "update": { "sessionUpdate": "agent_message_chunk", "content": { "text": "x" } },
                "_meta": { "periReplay": true },
            }),
        );
        assert!(projected.is_empty());
    }

    #[test]
    fn update_level_replay_marker_is_ignored() {
        // The host stamps replayed chunks on the update payload, not on the
        // notification params (`dispatch/session_replay.rs::replay_meta`).
        let projected = project(
            SESSION_UPDATE,
            &json!({
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "messageId": "msg-1",
                    "content": { "text": "old" },
                    "_meta": { "periReplay": true, "periLoadGeneration": "gen-1" },
                },
            }),
        );
        assert!(projected.is_empty());
    }

    #[test]
    fn tool_calls_keep_a_stable_tool_call_id() {
        let start = project(
            SESSION_UPDATE,
            &update(json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call-1",
                "title": "Read",
                "status": "pending",
                "rawInput": { "path": "/tmp/a" },
            })),
        );
        let Projected::Item(start) = &start[0] else {
            panic!("expected item");
        };
        assert_eq!(start.kind, ItemKind::ToolCall);
        assert_eq!(start.payload["toolCallId"], "call-1");
        assert_eq!(start.payload["input"]["path"], "/tmp/a");
        assert!(start.payload.get("body").is_none());

        let done = project(
            SESSION_UPDATE,
            &update(json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-1",
                "status": "completed",
                "rawOutput": "ok",
            })),
        );
        let Projected::Item(done) = &done[0] else {
            panic!("expected item");
        };
        assert_eq!(done.kind, ItemKind::ToolResult);
        assert_eq!(done.payload["toolCallId"], "call-1");
        assert_eq!(done.payload["output"], "ok");
    }

    #[test]
    fn tool_progress_is_a_tool_update() {
        let projected = project(
            SESSION_UPDATE,
            &update(json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-1",
                "status": "in_progress",
            })),
        );
        let Projected::Item(item) = &projected[0] else {
            panic!("expected item");
        };
        assert_eq!(item.kind, ItemKind::ToolUpdate);
    }

    #[test]
    fn execution_started_projects_a_turn_start() {
        let event = json!({
            "type": "execution_started",
            "value": { "generation": "gen-1", "request_id": "req-1" },
        });
        let projected = project(
            AGENT_EVENT,
            &json!({ "sessionId": "s1", "event_json": event.to_string() }),
        );
        assert_eq!(
            projected,
            vec![Projected::TurnStarted {
                request_id: "req-1".into(),
                generation: Some("gen-1".into()),
                queued: false,
            }]
        );
    }

    #[test]
    fn queued_run_start_is_marked_as_a_continuation() {
        let event = json!({
            "type": "user_input_run_started",
            "value": { "generation": "gen-1", "request_id": "req-2" },
        });
        let projected = project(
            AGENT_EVENT,
            &json!({ "sessionId": "s1", "event_json": event.to_string() }),
        );
        assert_eq!(
            projected,
            vec![Projected::TurnStarted {
                request_id: "req-2".into(),
                generation: Some("gen-1".into()),
                queued: true,
            }]
        );
    }

    #[test]
    fn child_completion_is_a_receipt_not_a_root_turn_end() {
        let event = json!({
            "type": "subagent_stopped",
            "value": {
                "agent_name": "explorer",
                "result": "done",
                "is_error": false,
                "instance_id": "child-1",
            },
        });
        let projected = project(
            AGENT_EVENT,
            &json!({ "sessionId": "s1", "event_json": event.to_string() }),
        );
        let Projected::Item(item) = &projected[0] else {
            panic!("expected item");
        };
        assert_eq!(item.kind, ItemKind::Receipt);
        assert_eq!(item.source.agent_id, "child:child-1");
        assert_eq!(item.payload["_raw"], true);
        assert!(projected
            .iter()
            .all(|event| !matches!(event, Projected::TurnCompleted { .. })));
    }

    #[test]
    fn agent_event_done_is_the_root_turn_terminal() {
        let projected = project(
            AGENT_EVENT_DONE,
            &json!({ "sessionId": "s1", "stopReason": "end_turn", "requestId": "req-1" }),
        );
        assert_eq!(
            projected,
            vec![Projected::TurnCompleted {
                request_id: Some("req-1".into()),
                stop_reason: "end_turn".into(),
            }]
        );
    }

    #[test]
    fn malformed_frames_are_ignored() {
        assert!(project(AGENT_EVENT, &json!({ "event_json": "not-json" })).is_empty());
        assert!(project(AGENT_EVENT_DONE, &json!({})).is_empty());
        assert!(project("session/update", &json!({})).is_empty());
        assert!(project("unknown/method", &json!({})).is_empty());
    }
}
