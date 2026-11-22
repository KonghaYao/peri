//! MCP tool approval → official `requires_action` (`mcp_approval_request`).
//!
//! Trigger scope is the gateway extension `agent.tools[].require_approval`
//! (`design §5.1`, `spec/issues/2026-11-21-peri-swarm-requires-action-design.md`):
//!
//! - only a declared MCP binding enables approval for that **server**; the
//!   session-wide `PermissionMode::Bypass` is never rewritten to `Default`;
//! - matching is by binding identity (`server_label` + wire tool name), never by
//!   tool name alone, so the same tool name on another server is not widened;
//! - anything that cannot be attributed to a declaration is fail-closed: it is
//!   treated as requiring approval, never silently allowed.
//!
//! The official `request_id` is an opaque public id (`appr_<uuidv7>`); it is
//! **not** the ACP JSON-RPC id, which never leaves the child boundary.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::domain::McpToolSpec;

/// Official item type carried by one `required_actions[]` entry.
pub const MCP_APPROVAL_REQUEST: &str = "mcp_approval_request";

/// Official `required_actions[].request.type`: the pending action is an MCP
/// tool call (`openai@7.60.0`, `SessionRequiredActionResourceMcpApprovalRequest`).
pub const MCP_TOOL_CALL: &str = "mcp_tool_call";

/// Prefix of the opaque public approval id.
pub const APPROVAL_ID_PREFIX: &str = "appr_";

/// One decision accepted for a pending approval.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approve,
    Deny,
    Cancel,
}

impl ApprovalDecision {
    /// Official `decision` spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Deny => "deny",
            Self::Cancel => "cancel",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "approve" => Some(Self::Approve),
            "deny" => Some(Self::Deny),
            "cancel" => Some(Self::Cancel),
            _ => None,
        }
    }

    /// ACP `session/request_permission` response body.
    ///
    /// `deny` and `cancel` both settle only this tool call. `cancel` keeps the
    /// ACP `cancelled` outcome so the host does not report a user rejection for
    /// an answer nobody gave.
    #[must_use]
    pub fn acp_response(self) -> Value {
        match self {
            Self::Approve => {
                json!({ "outcome": { "outcome": "selected", "optionId": "allow_once" } })
            }
            Self::Deny => {
                json!({ "outcome": { "outcome": "selected", "optionId": "reject_once" } })
            }
            Self::Cancel => json!({ "outcome": { "outcome": "cancelled" } }),
        }
    }
}

/// Why a pending approval is no longer answerable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettlementReason {
    /// A decision was accepted.
    Answered,
    /// The host's approval wait expired (`APPROVAL_WAIT`).
    Expired,
    /// The turn that owned the tool call reached its terminal.
    TurnEnded,
    /// The host/transport was lost, so no answer can reach the caller.
    ActorLost,
}

impl SettlementReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Answered => "answered",
            Self::Expired => "expired",
            Self::TurnEnded => "turn_ended",
            Self::ActorLost => "actor_lost",
        }
    }
}

/// One pending approval projected to the official `required_actions` shape.
///
/// `openai@7.60.0` nests the presentation facts under `request` and keeps the
/// opaque ids at the top level:
///
/// ```json
/// {
///   "type": "mcp_approval_request",
///   "request_id": "appr_...",
///   "turn_id": "turn_...",
///   "request": {
///     "type": "mcp_tool_call",
///     "server_label": "server-a",
///     "name": "search",
///     "arguments": {},
///     "message": "..."
///   }
/// }
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct RequiredAction {
    pub request_id: String,
    pub turn_id: String,
    /// Declared server label.
    ///
    /// The official type is `string` (not nullable), so a call that cannot be
    /// attributed to a declared server projects `""`. The empty string means
    /// "the source could not be determined"; `request.message` states that
    /// explicitly instead of pretending the server is named `""`.
    pub server_label: String,
    /// `null` when the host did not provide the wire tool name.
    pub tool: Option<String>,
    /// `null` (`Value::Null`) when the host did not provide arguments.
    pub arguments: Value,
}

impl RequiredAction {
    #[must_use]
    pub fn to_json(&self) -> Value {
        required_action_json(
            &self.request_id,
            &self.turn_id,
            &self.server_label,
            self.tool.as_deref(),
            Some(&self.arguments),
        )
    }
}

/// Project persisted approval facts into the official `required_actions` entry.
///
/// Live projection and replay fold call this same function so a reconnect
/// cannot observe a different shape than the live stream.
///
/// `server_label` is `""` when the source could not be determined: the official
/// field is a non-nullable `string`, so there is no `null` representation. The
/// presentation `message` says so explicitly. An empty `tool` is projected as
/// `null` (the official `name` is nullable), never as `name: ""`.
#[must_use]
pub fn required_action_json(
    request_id: &str,
    turn_id: &str,
    server_label: &str,
    tool: Option<&str>,
    arguments: Option<&Value>,
) -> Value {
    let tool = tool.filter(|tool| !tool.is_empty());
    json!({
        "type": MCP_APPROVAL_REQUEST,
        "request_id": request_id,
        "turn_id": turn_id,
        "request": {
            "type": MCP_TOOL_CALL,
            "server_label": server_label,
            "name": tool,
            "arguments": arguments.cloned().unwrap_or(Value::Null),
            "message": approval_message(server_label, tool),
        },
    })
}

/// Short, readable presentation text. It states the pending action only; it
/// never claims output from the tool that has not run.
fn approval_message(server_label: &str, tool: Option<&str>) -> String {
    match (server_label.is_empty(), tool) {
        (false, Some(tool)) => {
            format!("MCP tool `{tool}` on server `{server_label}` requires approval")
        }
        (false, None) => format!("An MCP tool call on server `{server_label}` requires approval"),
        (true, Some(tool)) => {
            format!("MCP tool `{tool}` requires approval; its server could not be determined")
        }
        (true, None) => {
            "An MCP tool call requires approval; its server could not be determined".to_owned()
        }
    }
}

/// How one declared server treats its tools.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApprovalPolicy {
    /// No tool on this server asks (`require_approval: "never"` or absent).
    Never,
    /// Every tool asks (`require_approval: "always"`).
    Always,
    /// Per-tool lists plus an optional default.
    Scoped {
        always: BTreeSet<String>,
        never: BTreeSet<String>,
        default: Option<bool>,
    },
    /// A value this contract cannot interpret: every tool asks (fail-closed).
    Unrecognized,
}

impl ApprovalPolicy {
    /// Parse one declaration value. Unknown shapes stay `Unrecognized` instead
    /// of collapsing to "no approval".
    #[must_use]
    pub fn parse(value: Option<&Value>) -> Self {
        let Some(value) = value else {
            return Self::Never;
        };
        match value {
            Value::String(text) if text == "always" => Self::Always,
            Value::String(text) if text == "never" => Self::Never,
            Value::Object(object) => {
                let always = tool_names(object.get("always"));
                let never = tool_names(object.get("never"));
                let default = match object.get("default").and_then(Value::as_str) {
                    Some("always") => Some(true),
                    Some("never") => Some(false),
                    // An explicit presence with another spelling is not a fact
                    // this contract may guess; the per-tool rule stays unknown.
                    Some(_) => None,
                    None => None,
                };
                Self::Scoped {
                    always,
                    never,
                    default,
                }
            }
            _ => Self::Unrecognized,
        }
    }

    /// Whether `tool` on this server must wait for an answer.
    ///
    /// A tool the declaration does not name, in an object without a default,
    /// stays unknown and therefore requires approval (fail-closed).
    #[must_use]
    pub fn requires_approval(&self, tool: &str) -> bool {
        match self {
            Self::Never => false,
            Self::Always | Self::Unrecognized => true,
            Self::Scoped {
                always,
                never,
                default,
            } => {
                if always.contains(tool) {
                    return true;
                }
                if never.contains(tool) {
                    return false;
                }
                default.unwrap_or(true)
            }
        }
    }

    /// Whether the host must escalate this server's calls at all. `Never`
    /// servers are never escalated, so their tools keep the session mode.
    #[must_use]
    pub fn escalates(&self) -> bool {
        !matches!(self, Self::Never)
    }
}

/// One declared server and its policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerApproval {
    pub server_label: String,
    pub policy: ApprovalPolicy,
}

/// Declared approvals, in declaration order (stable for diagnostics).
#[must_use]
pub fn declared(tools: &[McpToolSpec]) -> Vec<ServerApproval> {
    tools
        .iter()
        .filter_map(|tool| {
            let policy = ApprovalPolicy::parse(tool.require_approval.as_ref());
            policy.escalates().then(|| ServerApproval {
                server_label: tool.server_label.clone(),
                policy,
            })
        })
        .collect()
}

/// Server names whose external MCP calls must reach the approval channel.
///
/// An empty set keeps `PermissionMode::Bypass` byte-for-byte equivalent to the
/// pre-approval behavior.
#[must_use]
pub fn escalation_servers(tools: &[McpToolSpec]) -> BTreeSet<String> {
    declared(tools)
        .into_iter()
        .map(|approval| approval.server_label)
        .collect()
}

/// Resolve whether one approval request must wait, by binding identity.
///
/// `None` (an origin this session never declared) is fail-closed.
#[must_use]
pub fn requires_answer(declarations: &[ServerApproval], server_label: &str, tool: &str) -> bool {
    match declarations
        .iter()
        .find(|declaration| declaration.server_label == server_label)
    {
        Some(declaration) => declaration.policy.requires_approval(tool),
        // No declaration for this binding: nothing proves the call is exempt.
        None => true,
    }
}

fn tool_names(value: Option<&Value>) -> BTreeSet<String> {
    value
        .and_then(|value| value.get("tool_names"))
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "approval_test.rs"]
mod tests;
