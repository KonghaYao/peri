//! Session-detail and edit-page wording.
//!
//! Kept in a submodule so the central text module stays inside the repository
//! size budget; every string is still reachable as `text::…` through the
//! re-export in [`super`].

// ---------------------------------------------------------------------------
// Session detail data
// ---------------------------------------------------------------------------

/// First-load label.
pub const DETAIL_LOADING: &str = "Loading session…";

/// Snapshot marker after a failed refresh.
pub const DETAIL_STALE: &str =
    "Showing the last good snapshot. Refreshes are failing; press r to retry.";

/// Placeholder when no snapshot ever arrived.
pub const DETAIL_NOT_LOADED: &str = "(session not loaded)";

/// Approvals section heading.
pub const SECTION_APPROVALS: &str = "Pending approvals";

/// Approve button label.
pub const APPROVE: &str = "Approve";

/// Deny button label.
pub const DENY: &str = "Deny";

/// An approval answer is on the wire.
pub const APPROVAL_IN_FLIGHT: &str = "Sending approval answer…";

/// A required action with no usable label.
pub const ACTION_UNKNOWN: &str = "(unknown)";

/// Approval section heading line.
#[must_use]
pub fn approval_heading(server_label: &str, tool: Option<&str>) -> String {
    let server = if server_label.is_empty() {
        ACTION_UNKNOWN
    } else {
        server_label
    };
    match tool {
        Some(tool) if !tool.is_empty() => format!("{server} · {tool}"),
        _ => server.to_owned(),
    }
}

/// A required action this build cannot answer.
#[must_use]
pub fn unsupported_action(kind: &str) -> String {
    format!("Action type '{kind}' is read-only in this dashboard; resolve it in Peri.")
}

/// Approval answer failure.
#[must_use]
pub fn approval_failed(detail: &str) -> String {
    format!("Could not submit the approval answer: {detail}")
}

/// Snapshot refresh failure (kept beside the stale snapshot).
#[must_use]
pub fn detail_error(detail: &str) -> String {
    format!("Could not refresh the session: {detail}")
}

/// Timeline page failure.
#[must_use]
pub fn timeline_error(detail: &str) -> String {
    format!("Could not load earlier items: {detail}")
}

/// Timeline first page is loading.
pub const TIMELINE_LOADING: &str = "Loading timeline…";

/// Timeline has no items.
pub const TIMELINE_EMPTY: &str = "No timeline items yet.";

/// Older page in flight.
pub const TIMELINE_LOADING_OLDER: &str = "Loading earlier items…";

/// The 500-item cap was reached.
pub const TIMELINE_LIMIT_REACHED: &str = "500 items loaded; the timeline is capped.";

/// New items arrived while the reader was scrolled up.
pub const TIMELINE_UNSEEN: &str = "New items arrived while you were reading";

/// Turn group heading prefix.
#[must_use]
pub fn turn_heading(turn_id: &str) -> String {
    format!("Turn {turn_id}")
}

/// A turn section with no id yet.
pub const TURN_UNASSIGNED: &str = "Turn (unassigned)";

/// User message label.
pub const ROLE_USER: &str = "You";

/// Assistant message label.
pub const ROLE_ASSISTANT: &str = "Assistant";

/// Reasoning summary label.
pub const ROLE_REASONING: &str = "Reasoning";

/// Tool call heading.
#[must_use]
pub fn tool_call_heading(name: Option<&str>) -> String {
    match name {
        Some(name) if !name.is_empty() => format!("Tool call · {name}"),
        _ => "Tool call".to_owned(),
    }
}

/// Tool output heading.
#[must_use]
pub fn tool_output_heading(call_id: Option<&str>) -> String {
    match call_id {
        Some(call_id) if !call_id.is_empty() => format!("Tool output · {call_id}"),
        _ => "Tool output".to_owned(),
    }
}

/// Heading for an official item this build does not specialize.
#[must_use]
pub fn item_heading(label: &str) -> String {
    let label = if label.is_empty() {
        ACTION_UNKNOWN
    } else {
        label
    };
    format!("Item · {label}")
}

/// Collapsed block marker.
pub const COLLAPSED: &str = "▸";

/// Expanded block marker.
pub const EXPANDED: &str = "▾";

/// Ellipsis appended to clipped text.
pub const ELLIPSIS: &str = "…";

/// Turns table headers.
pub const TURN_COL_ID: &str = "Turn id";
/// Turns status column.
pub const TURN_COL_STATUS: &str = "Status";
/// Turns start column.
pub const TURN_COL_STARTED: &str = "Started";
/// Turns completion column.
pub const TURN_COL_COMPLETED: &str = "Completed";
/// Turns error column.
pub const TURN_COL_ERROR: &str = "Error";

/// Turns page is loading.
pub const TURNS_LOADING: &str = "Loading turns…";

/// Session has no turns.
pub const TURNS_EMPTY: &str = "No turns yet.";

/// Turns page failure.
#[must_use]
pub fn turns_error(detail: &str) -> String {
    format!("Could not load turns: {detail}")
}

/// Value shown when a field is absent.
pub const VALUE_NONE: &str = "(none)";

/// Value shown when a field was never reported.
pub const VALUE_NOT_SET: &str = "(not set)";

/// Info labels.
pub const LABEL_SESSION_ID: &str = "Session id";
/// Logical id label.
pub const LABEL_LOGICAL_ID: &str = "Logical id";
/// Root session id label.
pub const LABEL_ROOT_SESSION: &str = "Root session id";
/// Working-directory label.
pub const LABEL_CWD: &str = "Working directory";
/// Model label.
pub const LABEL_MODEL: &str = "Model";
/// Environment label.
pub const LABEL_ENVIRONMENT: &str = "Environment";
/// Durable state label.
pub const LABEL_STATE: &str = "State";
/// Official status label.
pub const LABEL_STATUS: &str = "Status";
/// Creation time label.
pub const LABEL_CREATED: &str = "Created";
/// Last activity label.
pub const LABEL_LAST_ACTIVE: &str = "Last active";
/// Sandbox label.
pub const LABEL_SANDBOX: &str = "Sandbox";
/// Vault ids label.
pub const LABEL_VAULT_IDS: &str = "Vault ids";
/// Metadata label.
pub const LABEL_METADATA: &str = "Metadata";
/// Instructions label.
pub const LABEL_INSTRUCTIONS: &str = "Instructions";
/// Usage label.
pub const LABEL_USAGE: &str = "Usage";
/// Error label.
pub const LABEL_ERROR: &str = "Error";
/// Prompt cache label.
pub const LABEL_PROMPT_CACHE: &str = "Prompt cache";

/// Environment summary (`kind` plus optional id).
#[must_use]
pub fn environment_value(kind: &str, id: Option<&str>) -> String {
    match id {
        Some(id) if !id.is_empty() => format!("{kind} · {id}"),
        _ => kind.to_owned(),
    }
}

/// Composer placeholder.
pub const COMPOSER_PLACEHOLDER: &str = "Type a message… Ctrl-Enter sends";

/// Composer hint while approvals block input.
pub const COMPOSER_BLOCKED_APPROVAL: &str =
    "Resolve the pending approval above before sending a message.";

/// Composer in-flight hint.
pub const COMPOSER_SENDING: &str = "Sending message…";

/// Empty message error.
pub const ERR_MESSAGE_EMPTY: &str = "Type a message before sending.";

/// Send failure.
#[must_use]
pub fn send_failed(detail: &str) -> String {
    format!("Could not send the message: {detail}")
}

/// Delete 409 explanation.
#[must_use]
pub fn delete_conflict(detail: &str) -> String {
    format!("A turn is running. Cancel it before deleting this session. {detail}")
}

/// Delete failure.
#[must_use]
pub fn delete_failed(detail: &str) -> String {
    format!("Could not delete the session: {detail}")
}

/// 502 explanation.
#[must_use]
pub fn gateway_host_failure(detail: &str) -> String {
    format!("The gateway could not reach its host: {detail}")
}

/// Update failure.
#[must_use]
pub fn update_failed(detail: &str) -> String {
    format!("Could not save the session: {detail}")
}

/// Update 409 explanation.
#[must_use]
pub fn update_conflict(detail: &str) -> String {
    format!("The session is running; cancel the turn before editing. {detail}")
}

/// Cancel-turn failure.
#[must_use]
pub fn cancel_failed(detail: &str) -> String {
    format!("Could not cancel the turn: {detail}")
}

// ---------------------------------------------------------------------------
// Session edit
// ---------------------------------------------------------------------------

/// Edit page title.
pub const EDIT_TITLE: &str = "Edit session";

/// Edit page explanation.
pub const EDIT_INTRO: &str = "Model, metadata and prompt cache can be changed. Instructions are read-only because the gateway update endpoint does not accept them.";

/// Read-only instructions explanation.
pub const EDIT_INSTRUCTIONS_READONLY: &str =
    "Read-only: the gateway's update endpoint does not accept instructions.";

/// Placeholder for the required model.
pub const EDIT_MODEL_PLACEHOLDER: &str = "required";

/// Advanced prompt-cache placeholder.
pub const EDIT_PROMPT_CACHE_PLACEHOLDER: &str = "{\"ttl\":\"30m\"} · blank keeps, null clears";

/// Required-model error.
pub const ERR_MODEL_REQUIRED: &str = "Model is required.";

/// Edit submission in flight.
pub const EDIT_SUBMITTING: &str = "Saving changes…";

/// Confirm delete dialog title.
pub const DELETE_TITLE: &str = "Delete session";

/// Confirm delete dialog message.
pub const DELETE_MESSAGE: &str =
    "Deleting this session cannot be undone. Its turns and items are removed from the gateway.";

/// Confirm delete dialog hint.
pub const DELETE_HINT: &str = "Enter confirms · Esc cancels";

/// Cancel-turn dialog title.
pub const CANCEL_TURN_TITLE: &str = "Cancel running turn";

/// Cancel-turn dialog message.
pub const CANCEL_TURN_MESSAGE: &str =
    "The running turn will be stopped. The session and its history are kept.";

/// Cancel-turn dialog hint.
pub const CANCEL_TURN_HINT: &str = "Enter confirms · Esc cancels";
