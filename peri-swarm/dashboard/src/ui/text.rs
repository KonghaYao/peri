//! Every user-facing English string in the dashboard.
//!
//! Pages format values, but the wording lives here so a future locale layer can
//! replace it in one place. No string in this module ever contains a secret.

use std::time::{Duration, Instant};

use crate::app::{ActionId, DetailTab, FieldId, HealthState, Page, SidebarGroup};
use crate::view_model::{Column, StatusBucket};

mod detail;
pub use detail::*;
mod errors;
pub use errors::*;
mod footer;
pub use footer::*;
mod help;
pub use help::*;
mod observability;
pub use observability::*;

// ---------------------------------------------------------------------------
// Shell
// ---------------------------------------------------------------------------

/// Window title and header label.
pub const APP_TITLE: &str = "peri-swarm dashboard";

/// Header value when no credentials exist yet.
pub const NOT_CONFIGURED: &str = "not configured";

/// Health labels.
pub const HEALTH_UNKNOWN: &str = "unknown";
/// Fully healthy gateway.
pub const HEALTH_HEALTHY: &str = "healthy";
/// No connection attempted (onboarding).
pub const HEALTH_NOT_CONNECTED: &str = "not connected";

/// Health line for the header and Settings.
#[must_use]
pub fn health_label(state: &HealthState) -> String {
    match state {
        HealthState::Unknown => HEALTH_UNKNOWN.to_owned(),
        HealthState::Healthy => HEALTH_HEALTHY.to_owned(),
        HealthState::Degraded(status) => format!("degraded ({status})"),
        HealthState::Unreachable(message) => format!("unreachable: {message}"),
    }
}

/// Last-refresh label, relative to `now`.
#[must_use]
pub fn last_refresh_label(last: Option<Instant>, now: Instant) -> String {
    format!("last refresh: {}", refresh_age(last, now))
}

/// Value-only refresh age for label/value rows.
///
/// Settings already prints its own `Last refresh` label, so the value must not
/// repeat the words.
#[must_use]
pub fn refresh_age(last: Option<Instant>, now: Instant) -> String {
    let Some(last) = last else {
        return "never".to_owned();
    };
    let seconds = now.saturating_duration_since(last).as_secs();
    match seconds {
        0 | 1 => "just now".to_owned(),
        2..=59 => format!("{seconds}s ago"),
        60..=3599 => format!("{}m ago", seconds / 60),
        _ => format!("{}h ago", seconds / 3600),
    }
}

/// Latency label used by Settings.
#[must_use]
pub fn latency_label(latency: Option<Duration>) -> String {
    match latency {
        Some(latency) => format!("{} ms", latency.as_millis()),
        None => DASH.to_owned(),
    }
}

/// Placeholder for an unavailable value.
pub const DASH: &str = "—";

/// Narrow-terminal menu button label (U+2630 TRIGRAM FOR HEAVEN).
pub const MENU_BUTTON: &str = "\u{2630}";

/// Menu overlay title.
pub const MENU_TITLE: &str = "Menu";

/// Menu footer hint.
pub const MENU_HINT: &str = "up/down select · Enter open · Esc close · click a page";

/// Sidebar group labels.
#[must_use]
pub fn group_label(group: SidebarGroup) -> &'static str {
    match group {
        SidebarGroup::Monitor => "Monitor",
        SidebarGroup::Resources => "Resources",
        SidebarGroup::Platform => "Platform",
        SidebarGroup::Bottom => "Bottom",
    }
}

/// Sidebar page labels.
#[must_use]
pub fn page_label(page: Page) -> &'static str {
    match page {
        Page::CoreMetrics => "Core metrics",
        Page::AgentMetrics => "Agent metrics",
        Page::SandboxMetrics => "Sandbox metrics",
        Page::SessionLog => "Session log",
        Page::RequestLog => "Request log",
        Page::Query => "Query",
        Page::Agents => "Agents",
        Page::Artifacts => "Artifacts",
        Page::Vaults => "Vaults",
        Page::Mcp => "MCP",
        Page::Automations => "Automations",
        Page::Nodes => "Nodes",
        Page::Settings => "Settings",
    }
}

/// Action button labels.
#[must_use]
pub fn action_label(action: ActionId) -> &'static str {
    match action {
        ActionId::Submit => "Connect",
        ActionId::TestConnection => "Test connection",
        ActionId::EditCredentials => "Edit credentials",
        ActionId::ConfigureObservability => "Configure observability",
        ActionId::TestObservability => "Test",
        ActionId::SaveObservability => "Save",
        ActionId::CancelObservability => "Cancel",
        ActionId::SignOut => "Sign out",
        ActionId::Quit => "Quit",
        ActionId::ShowGettingStarted => "Show getting started",
        ActionId::SaveCredentials => "Validate and save",
        ActionId::Back => "Back",
        ActionId::ConfirmSignOut => "Sign out",
        ActionId::CancelSignOut => "Cancel",
        ActionId::Retry => "Retry",
        ActionId::OpenSettings => "Settings",
        ActionId::CloseHelp => "Close",
        ActionId::NewSession => "New session",
        ActionId::RefreshSessions => "Refresh",
        ActionId::CreateSubmit => "Create",
        ActionId::AddMetadata => "Add row",
        ActionId::ToggleAdvanced => "Advanced JSON",
        ActionId::RefreshDetail => "Refresh",
        ActionId::LoadEarlierItems => "Load earlier",
        ActionId::JumpToLatest => "Jump to latest",
        ActionId::LoadMoreTurns => "Load more",
        ActionId::SendMessage => "Send",
        ActionId::ApproveFirst => "Approve first",
        ActionId::DenyFirst => "Deny first",
        ActionId::EditSession => "Edit",
        ActionId::DeleteSession => "Delete",
        ActionId::CancelTurn => "Cancel turn",
        ActionId::ConfirmDeleteSession => "Delete session",
        ActionId::CancelDeleteSession => "Keep session",
        ActionId::ConfirmCancelTurn => "Cancel turn",
        ActionId::CancelCancelTurn => "Keep running",
        ActionId::EditSubmit => "Save changes",
        ActionId::RefreshCoreMetrics
        | ActionId::RefreshAgentMetrics
        | ActionId::RefreshRequestLog => "Refresh",
        ActionId::RunQuery => "Run",
    }
}

/// Field labels.
#[must_use]
pub fn field_label(field: FieldId) -> &'static str {
    match field {
        FieldId::OnboardingBaseUrl | FieldId::CredentialsBaseUrl => "Base URL",
        FieldId::OnboardingApiKey | FieldId::CredentialsApiKey => "API key",
        FieldId::ObservabilityBaseUrl => "Base URL",
        FieldId::ObservabilityOrg => "Organization",
        FieldId::ObservabilityUser => "User",
        FieldId::ObservabilityPassword => "Password",
        FieldId::SessionSearch => "Search",
        FieldId::QuerySql => "SQL",
        FieldId::CreateLogicalId => "Logical ID (optional)",
        FieldId::CreateModel => "Model",
        FieldId::CreateCwd => "Working directory (cwd)",
        FieldId::CreateInstructions => "Instructions",
        FieldId::CreateMetadataKey(_) => "Metadata key",
        FieldId::CreateMetadataValue(_) => "Metadata value",
        FieldId::CreateTools => "tools",
        FieldId::CreateEnvironment => "environment",
        FieldId::CreatePromptCache => "prompt_cache_options",
        FieldId::DetailComposer => "Message",
        FieldId::EditModel => "Model",
        FieldId::EditMetadataKey(_) => "Metadata key",
        FieldId::EditMetadataValue(_) => "Metadata value",
        FieldId::EditPromptCache => "prompt_cache_options",
    }
}

/// Scope note for an intentionally empty page.
pub const SCOPE_EMPTY: &str = "Empty page · no gateway data fetched";

/// Scope note for Settings.
pub const SCOPE_SETTINGS: &str = "Local configuration";

/// Scope note for onboarding.
pub const SCOPE_ONBOARDING: &str = "Nothing is saved until validation succeeds";

// ---------------------------------------------------------------------------
// Onboarding
// ---------------------------------------------------------------------------

/// Onboarding page title.
pub const ONBOARDING_TITLE: &str = "Connect to Peri Swarm";

/// Onboarding explanation.
pub const ONBOARDING_INTRO: &str = "Enter the gateway base URL and an API key. Both the health check and an authenticated request must succeed before anything is saved.";

/// Base URL placeholder.
pub const ONBOARDING_BASE_URL_PLACEHOLDER: &str = "http://127.0.0.1:8080";

/// API key placeholder.
pub const ONBOARDING_API_KEY_PLACEHOLDER: &str = "paste the deployment API key";

/// Non-loopback cleartext warning.
pub const CLEARTEXT_WARNING: &str =
    "This address is not loopback. The API key will be sent in cleartext over http.";

/// In-flight validation line.
pub const VALIDATING: &str = "Validating credentials…";

/// Required API key error.
pub const ERR_API_KEY_REQUIRED: &str = "API key is required.";

/// Lost pending-validation state.
pub const ERR_VALIDATION_STATE_LOST: &str = "Validation state was lost. Submit again.";

/// Base URL field error for a rejected URL.
#[must_use]
pub fn base_url_invalid(detail: &str) -> String {
    format!("Base URL is invalid: {detail}")
}

/// Config read error shown on onboarding.
#[must_use]
pub fn config_read_failed(detail: &str) -> String {
    format!("Could not read saved credentials: {detail}")
}

/// Config write error shown on a form.
#[must_use]
pub fn config_save_failed(detail: &str) -> String {
    format!("Could not save credentials: {detail}")
}

/// Health-step validation failure.
#[must_use]
pub fn health_check_failed(detail: &str) -> String {
    format!("Gateway health check failed: {detail}")
}

/// API key rejected by the authenticated check.
pub const ERR_AUTH_REJECTED: &str = "The gateway rejected this API key.";

/// Authenticated check failure that was not a 401.
#[must_use]
pub fn authentication_check_failed(detail: &str) -> String {
    format!("Authentication check failed: {detail}")
}

/// Human description of an API error, without headers or the key.
#[must_use]
pub fn api_error_detail(code: Option<&str>, status: Option<u16>, message: &str) -> String {
    match (code, status) {
        (Some(code), Some(status)) => format!("{message} ({code}, HTTP {status})"),
        (Some(code), None) => format!("{message} ({code})"),
        (None, Some(status)) => format!("{message} (HTTP {status})"),
        (None, None) => message.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Worker failures
// ---------------------------------------------------------------------------

/// No active gateway connection for a revalidation.
pub const NO_ACTIVE_CONNECTION: &str = "No active gateway connection.";

/// Worker client could not be built.
pub const WORKER_CLIENT_UNAVAILABLE: &str = "The background API client is not available.";

/// Worker startup failure.
#[must_use]
pub fn worker_start_failed(detail: &str) -> String {
    format!("Could not start the API worker: {detail}")
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// Settings page title.
pub const SETTINGS_TITLE: &str = "Settings";

/// Health row label.
pub const LABEL_HEALTH: &str = "Health";

/// Contract row label.
pub const LABEL_CONTRACT: &str = "Contract";

/// Last-latency row label.
pub const LABEL_LATENCY: &str = "Last latency";

/// Last-refresh row label.
pub const LABEL_REFRESH: &str = "Last refresh";

/// API key row label.
pub const LABEL_API_KEY: &str = "API key";

/// Base URL row label.
pub const LABEL_BASE_URL: &str = "Base URL";

/// Connection section heading.
pub const SECTION_CONNECTION: &str = "Connection";

/// Actions section heading.
pub const SECTION_ACTIONS: &str = "Actions";

/// API key value when a key is stored.
pub const KEY_SET: &str = "(set)";

/// API key value when no key is stored.
pub const KEY_NOT_SET: &str = "(not set)";

/// Contract value before the header is observed.
pub const CONTRACT_NOT_REPORTED: &str = "not reported yet";

/// Settings key-storage hint.
pub const SETTINGS_KEY_HINT: &str = "The API key is stored locally and is never displayed.";

/// In-flight connection test.
pub const TESTING_CONNECTION: &str = "Testing connection…";

/// Successful connection test.
#[must_use]
pub fn test_ok(elapsed: Duration) -> String {
    format!("Connection OK in {} ms.", elapsed.as_millis())
}

/// Successful re-authentication from the banner.
pub const REAUTHENTICATED: &str = "Re-authenticated. Polling resumed.";

/// Credentials saved by the editor.
pub const CREDENTIALS_SAVED: &str = "Credentials saved.";

/// Contract mismatch warning.
#[must_use]
pub fn contract_mismatch(actual: &str, expected: &str) -> String {
    format!(
        "Gateway contract {actual} does not match the expected version {expected}. Read-only use is allowed; writes may fail."
    )
}

/// Loose config permissions warning.
#[must_use]
pub fn permissions_warning(mode: u32) -> String {
    format!(
        "Config file mode is {mode:04o}; expected 0600. The API key may be readable by other users."
    )
}

/// Sign-out deletion failure.
#[must_use]
pub fn sign_out_failed(detail: &str) -> String {
    format!("Could not delete the saved credentials: {detail}")
}

// ---------------------------------------------------------------------------
// Credentials editor
// ---------------------------------------------------------------------------

/// Credential editor title.
pub const CREDENTIALS_TITLE: &str = "Edit credentials";

/// Credential editor explanation.
pub const CREDENTIALS_INTRO: &str = "The new origin is validated before anything is saved. The current credentials stay in place until validation and save both succeed.";

/// Hint under the key field.
pub const CREDENTIALS_KEY_HINT: &str = "Leave the API key blank to keep the current key.";

// ---------------------------------------------------------------------------
// Observability
// ---------------------------------------------------------------------------

/// Settings section heading for the OpenObserve connection.
pub const SECTION_OBSERVABILITY: &str = "Observability";

/// Observability status row label.
pub const LABEL_OBSERVABILITY_STATUS: &str = "Status";

/// Observability organization row label.
pub const LABEL_OBSERVABILITY_ORG: &str = "Organization";

/// Observability user row label.
pub const LABEL_OBSERVABILITY_USER: &str = "User";

/// Observability status when no connection is stored.
pub const OBSERVABILITY_NOT_CONFIGURED: &str = "not configured";

/// Observability status when a connection is stored.
pub const OBSERVABILITY_CONFIGURED: &str = "configured";

/// Short warning shown on Settings.
pub const OBSERVABILITY_NOT_A_BOUNDARY: &str = "Direct admin connection · not a read-only boundary";

/// Configuration sub-page title.
pub const OBSERVABILITY_TITLE: &str = "Configure observability";

/// Configuration sub-page explanation.
pub const OBSERVABILITY_INTRO: &str = "The dashboard sends SELECT queries directly to OpenObserve with a dedicated admin account. OpenObserve OSS has no viewer role, so that account can write; the client-side SELECT check is not a security boundary. Keep the connection on a private network, use a dedicated account, and rotate the password.";

/// Base URL placeholder.
pub const OBSERVABILITY_BASE_URL_PLACEHOLDER: &str = "http://127.0.0.1:5080";

/// Organization placeholder.
pub const OBSERVABILITY_ORG_PLACEHOLDER: &str = "default";

/// User placeholder.
pub const OBSERVABILITY_USER_PLACEHOLDER: &str = "dashboard account";

/// Password placeholder.
pub const OBSERVABILITY_PASSWORD_PLACEHOLDER: &str = "leave blank to keep the stored password";

/// Hint under the password field.
pub const OBSERVABILITY_PASSWORD_HINT: &str =
    "The password is stored in the 0600 config file and is never displayed.";

/// In-flight OpenObserve probe.
pub const TESTING_OBSERVABILITY: &str = "Testing OpenObserve connection…";

/// Observability settings saved by the editor.
pub const OBSERVABILITY_SAVED: &str = "Observability settings saved.";

/// Successful OpenObserve probe.
#[must_use]
pub fn observability_test_ok(elapsed: Duration, streams: Option<usize>) -> String {
    match streams {
        Some(1) => format!(
            "OpenObserve connection OK in {} ms (1 stream).",
            elapsed.as_millis()
        ),
        Some(streams) => format!(
            "OpenObserve connection OK in {} ms ({streams} streams).",
            elapsed.as_millis()
        ),
        None => format!("OpenObserve connection OK in {} ms.", elapsed.as_millis()),
    }
}

/// Human description of an OpenObserve failure, without credentials or headers.
#[must_use]
pub fn observability_error(error: &crate::observability::ObservabilityError) -> String {
    use crate::observability::ObservabilityError;
    match error {
        ObservabilityError::InvalidConfig(inner) => observability_config_error(inner),
        ObservabilityError::Timeout => "OpenObserve request timed out.".to_owned(),
        ObservabilityError::Unreachable => "Could not reach OpenObserve.".to_owned(),
        ObservabilityError::Http { status, detail } => {
            format!("OpenObserve returned HTTP {status}{detail}.")
        }
        ObservabilityError::Malformed => "OpenObserve returned an unreadable response.".to_owned(),
        ObservabilityError::Client => "Could not build the OpenObserve client.".to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Banner
// ---------------------------------------------------------------------------

/// HTTP 401 status text.
pub const STATUS_401: &str = "401";

/// Banner retry button.
pub const BANNER_RETRY: &str = "Retry";

/// Banner settings button.
pub const BANNER_SETTINGS: &str = "Settings";

/// One-line banner text.
#[must_use]
pub fn banner_text(code: Option<&str>, message: &str) -> String {
    match code {
        Some(code) => format!("{code} · {message}"),
        None => message.to_owned(),
    }
}

/// Unauthorized banner message.
///
/// The action comes first so a narrow banner still tells the operator what to
/// do after the server detail is truncated.
#[must_use]
pub fn unauthorized_banner(message: &str) -> String {
    format!("Re-authenticate from Settings. Authentication failed: {message}.")
}

/// Shown instead of a spinner on gateway-backed pages while a 401 pause is
/// active. OpenObserve pages keep loading independently.
pub const POLLING_PAUSED: &str =
    "Polling paused: the gateway rejected the API key. Re-authenticate from Settings.";

/// Generic gateway failure banner.
#[must_use]
pub fn gateway_error_banner(message: &str) -> String {
    format!("Gateway error: {message}")
}

/// Error-actions overlay title.
pub const ERROR_ACTIONS_TITLE: &str = "Connection problem";

/// Error-actions overlay hint.
pub const ERROR_ACTIONS_HINT: &str = "Enter activates · Esc closes";

// ---------------------------------------------------------------------------
// Sign out
// ---------------------------------------------------------------------------

/// Sign-out overlay title.
pub const SIGNOUT_TITLE: &str = "Sign out";

/// Sign-out overlay message.
pub const SIGNOUT_MESSAGE: &str =
    "Delete the saved credentials on this machine and return to onboarding?";

/// Sign-out overlay hint. Cancel is focused first, so Enter cancels; the hint
/// describes what the keys do instead of claiming that Enter confirms.
pub const SIGNOUT_HINT: &str = "Tab/left-right select · Enter activates · Esc cancels";

// ---------------------------------------------------------------------------
// Empty pages
// ---------------------------------------------------------------------------

/// Empty-page body label.
pub const WHY_EMPTY: &str = "Why this page is empty";

/// Capability label.
pub const WHAT_GATEWAY_SUPPORTS: &str = "What the gateway supports";

/// Never-fabricate notice.
pub const EMPTY_NOTICE: &str =
    "This page is intentionally empty. The dashboard never fabricates data.";

/// Explanation and real capabilities for one empty page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmptyPageText {
    /// Why the page has no data.
    pub reason: &'static str,
    /// What the gateway does expose.
    pub supported: &'static str,
}

/// Copy for one of the 11 unimplemented pages.
#[must_use]
pub fn empty_page(page: Page) -> EmptyPageText {
    match page {
        Page::SandboxMetrics => EmptyPageText {
            reason: "This build collects no resource-level sandbox metrics.",
            supported: "The gateway reports only the sandbox type and id attached to a session; it exposes no CPU, memory, or sandbox resource usage.",
        },
        Page::Query => EmptyPageText {
            reason: "The query page is not implemented in this build.",
            supported: "This dashboard calls no ad-hoc query API. Session data remains available in Session log.",
        },
        Page::Agents => EmptyPageText {
            reason: "This build does not manage agents as a standalone resource.",
            supported: "Agent model and instructions are configured per session and shown in session detail.",
        },
        Page::Artifacts => EmptyPageText {
            reason: "Artifacts are not implemented in this build.",
            supported: "The gateway exposes no artifact store or artifact endpoints.",
        },
        Page::Vaults => EmptyPageText {
            reason: "The gateway used here exposes no vault endpoints.",
            supported: "Vault ids reported on a session are shown in session detail when present.",
        },
        Page::Mcp => EmptyPageText {
            reason: "MCP server management is not implemented in this build.",
            supported: "The gateway exposes no MCP server inventory or configuration endpoints.",
        },
        Page::Automations => EmptyPageText {
            reason: "Automations are not implemented in this build.",
            supported: "The gateway exposes no automation or schedule endpoints.",
        },
        Page::Nodes => EmptyPageText {
            reason: "Multi-node aggregation is not available in this build.",
            supported: "The dashboard connects to one gateway origin; its health and contract status are shown in the header and Settings.",
        },
        Page::CoreMetrics
        | Page::AgentMetrics
        | Page::SessionLog
        | Page::RequestLog
        | Page::Settings => EmptyPageText {
            reason: "This page is implemented.",
            supported: "No empty state is shown.",
        },
    }
}

// ---------------------------------------------------------------------------
// Session log
// ---------------------------------------------------------------------------

/// Session log page title.
pub const SESSION_LOG_TITLE: &str = "Session log";

/// Number-loaded label used by the footer scope note.
#[must_use]
pub fn loaded_label(loaded: usize) -> String {
    format!("{loaded} loaded")
}

/// Scope note for the session log.
#[must_use]
pub fn scope_session_log(loaded: usize, stale: bool) -> String {
    let mut note = format!("Newest activity first · {}", loaded_label(loaded));
    if stale {
        note.push_str(" · stale");
    }
    note
}

/// Status bucket labels.
#[must_use]
pub fn status_bucket_label(bucket: StatusBucket) -> &'static str {
    match bucket {
        StatusBucket::All => "All",
        StatusBucket::InProgress => "In progress",
        StatusBucket::RequiresAction => "Requires action",
        StatusBucket::Failed => "Failed",
        StatusBucket::Idle => "Idle",
    }
}

/// Chip label with its loaded count.
#[must_use]
pub fn chip_label(bucket: StatusBucket, count: usize) -> String {
    format!("{} {count}", status_bucket_label(bucket))
}

/// Table column headers.
#[must_use]
pub fn column_header(column: Column) -> &'static str {
    match column {
        Column::Session => "Session",
        Column::Status => "Status",
        Column::Approvals => "Approvals",
        Column::LastActive => "Last active",
        Column::Metadata => "Metadata",
    }
}

/// Search field placeholder.
pub const SEARCH_PLACEHOLDER: &str = "Search id, model, metadata, environment";

/// Search scope hint shown next to the field when it fits.
pub const SEARCH_SCOPE_HINT: &str = "searches the loaded set";

/// Session log pager: previous page.
pub const PAGE_PREV: &str = "‹ Prev";

/// Session log pager: next page.
pub const PAGE_NEXT: &str = "Next ›";

/// Loading label shown before the first page arrives.
pub const LOADING_SESSIONS: &str = "Loading sessions…";

/// Empty state title.
pub const EMPTY_SESSIONS_TITLE: &str = "No sessions yet";

/// Empty state explanation.
pub const EMPTY_SESSIONS_BODY: &str =
    "The gateway returned no sessions for this origin. Create one to get started.";

/// Shown when the loaded set has no row matching the filter and search.
pub const NO_SESSIONS_MATCH: &str = "No sessions match the current filter and search.";

/// Shown above the table when the metadata column does not fit.
pub const METADATA_HIDDEN: &str = "Metadata is hidden at this width; open a session to see it.";

/// Inline list error prefix.
#[must_use]
pub fn list_error(detail: &str) -> String {
    format!("Could not refresh sessions: {detail}")
}

/// Approvals column summary.
#[must_use]
pub fn approvals_summary(count: usize) -> String {
    match count {
        0 => DASH.to_owned(),
        1 => "1 pending".to_owned(),
        count => format!("{count} pending"),
    }
}

/// Relative time label for a Unix-seconds timestamp.
///
/// `0` (or a negative value) means the gateway never reported the timestamp.
#[must_use]
pub fn relative_time(unix_seconds: i64, now_unix: i64) -> String {
    if unix_seconds <= 0 {
        return DASH.to_owned();
    }
    let delta = now_unix.saturating_sub(unix_seconds);
    if delta <= 1 {
        return "just now".to_owned();
    }
    if delta < 60 {
        return format!("{delta}s ago");
    }
    if delta < 3_600 {
        return format!("{}m ago", delta / 60);
    }
    if delta < 86_400 {
        return format!("{}h ago", delta / 3_600);
    }
    format!("{}d ago", delta / 86_400)
}

// ---------------------------------------------------------------------------
// Create session
// ---------------------------------------------------------------------------

/// Create page title.
pub const CREATE_TITLE: &str = "New session";

/// Create page explanation.
pub const CREATE_INTRO: &str = "Only filled fields are sent. Logical ID and model may stay empty so the gateway applies its defaults. Nothing is sent until you press Create.";

/// Placeholder for the optional logical id.
pub const CREATE_LOGICAL_ID_PLACEHOLDER: &str = "optional, A-Z a-z 0-9 _ - (max 128)";

/// Placeholder for the model.
pub const CREATE_MODEL_PLACEHOLDER: &str = "gateway default when empty";

/// Placeholder for the working directory.
pub const CREATE_CWD_PLACEHOLDER: &str = "/path/to/workdir";

/// Placeholder for the instructions area.
pub const CREATE_INSTRUCTIONS_PLACEHOLDER: &str = "system instructions for the agent";

/// Metadata section heading.
pub const SECTION_METADATA: &str = "Metadata";

/// Shown when no metadata rows exist.
pub const METADATA_EMPTY: &str = "No metadata rows yet.";

/// Placeholder for a metadata key field.
pub const METADATA_KEY_PLACEHOLDER: &str = "key";

/// Placeholder for a metadata value field.
pub const METADATA_VALUE_PLACEHOLDER: &str = "value";

/// Remove-row button label.
pub const REMOVE_ROW: &str = "Remove";

/// Advanced section heading.
pub const SECTION_ADVANCED: &str = "Advanced JSON";

/// Button label that expands the advanced section.
pub const SHOW_ADVANCED: &str = "Show advanced JSON";

/// Button label that collapses the advanced section.
pub const HIDE_ADVANCED: &str = "Hide advanced JSON";

/// Advanced JSON area placeholders.
pub const CREATE_TOOLS_PLACEHOLDER: &str = "[]";
/// Environment placeholder.
pub const CREATE_ENVIRONMENT_PLACEHOLDER: &str = "{}";
/// Prompt cache placeholder.
pub const CREATE_PROMPT_CACHE_PLACEHOLDER: &str = "{\"ttl\":\"30m\"}";

/// Submitting label.
pub const CREATE_SUBMITTING: &str = "Creating session…";

/// Logical id validation failure.
pub const ERR_LOGICAL_ID_INVALID: &str =
    "Logical ID must match ^[A-Za-z0-9_-]{1,128}$ when it is not empty.";

/// Metadata key validation failure.
pub const ERR_METADATA_KEY_REQUIRED: &str = "Metadata keys must not be empty.";

/// Duplicate metadata key failure.
pub const ERR_METADATA_KEY_DUPLICATE: &str = "Metadata keys must be unique.";

/// Advanced JSON parse failure.
#[must_use]
pub fn advanced_json_invalid(field: &str, detail: &str) -> String {
    format!("Advanced JSON for {field} does not parse: {detail}")
}

/// Create request failure.
#[must_use]
pub fn create_failed(detail: &str) -> String {
    format!("Could not create the session: {detail}")
}

/// Identity conflict shown next to the logical id.
#[must_use]
pub fn identity_conflict(detail: &str) -> String {
    format!("A session with this logical id already exists: {detail}")
}

// ---------------------------------------------------------------------------
// Session detail skeleton
// ---------------------------------------------------------------------------

/// Detail page title.
pub const DETAIL_TITLE: &str = "Session detail";

/// Detail tab labels.
#[must_use]
pub fn detail_tab_label(tab: DetailTab) -> &'static str {
    match tab {
        DetailTab::Timeline => "Timeline",
        DetailTab::Turns => "Turns",
        DetailTab::Info => "Info",
    }
}

/// Snapshot line shown until the detail loader exists.
pub const DETAIL_SNAPSHOT_PENDING: &str = "The session snapshot is not loaded in this build stage.";

/// Timeline placeholder body.
pub const DETAIL_TIMELINE_PLACEHOLDER: &str = "Timeline items arrive with the detail stage. The tab is wired so the body can be replaced without changing the shell.";

/// Turns placeholder body.
pub const DETAIL_TURNS_PLACEHOLDER: &str = "The turn list arrives with the detail stage. The tab is wired so the body can be replaced without changing the shell.";

/// Info placeholder body.
pub const DETAIL_INFO_PLACEHOLDER: &str =
    "Session facts (cwd, sandbox, vaults, metadata, instructions) arrive with the detail stage.";

/// Placeholder body for one tab.
#[must_use]
pub fn detail_placeholder(tab: DetailTab) -> &'static str {
    match tab {
        DetailTab::Timeline => DETAIL_TIMELINE_PLACEHOLDER,
        DetailTab::Turns => DETAIL_TURNS_PLACEHOLDER,
        DetailTab::Info => DETAIL_INFO_PLACEHOLDER,
    }
}

/// Footer hints on the session log.
pub const FOOTER_SESSION_LOG: &str =
    "click a row · PgUp/PgDn page · n new session · / search · r refresh · Tab focus · Ctrl-C quit";

/// Footer hints on the metrics pages.
pub const FOOTER_METRICS: &str =
    "1/2/3 window · PgUp/PgDn page · r refresh · Tab focus · h help · q quit · Ctrl-C quit";

/// Footer hints on the metrics pages when the sidebar is behind the menu.
pub const FOOTER_METRICS_NARROW: &str =
    "menu/Tab focus · 1/2/3 window · PgUp/PgDn page · r refresh · h help · q quit · Ctrl-C quit";

/// Footer hints on the Query page.
pub const FOOTER_QUERY: &str =
    "1/2/3 window · Ctrl-Enter run · PgUp/PgDn page · Tab focus · h help · q quit · Ctrl-C quit";

/// Footer hints on the Query page when the sidebar is behind the menu.
pub const FOOTER_QUERY_NARROW: &str =
    "menu/Tab focus · 1/2/3 window · Ctrl-Enter run · PgUp/PgDn page · q quit · Ctrl-C quit";

/// Footer hints on the Query page when the executed SQL carries its own
/// top-level LIMIT/OFFSET.
///
/// OpenObserve ignores the request offset in that case, so server-side paging
/// is off; advertising `PgUp/PgDn page` would promise keys that do nothing.
pub const FOOTER_QUERY_MANUAL_LIMIT: &str =
    "1/2/3 window · Ctrl-Enter run · SQL LIMIT/OFFSET · Tab focus · h help · q quit · Ctrl-C quit";

/// Footer hints on the Query page with a SQL LIMIT/OFFSET when the sidebar is
/// behind the menu.
pub const FOOTER_QUERY_MANUAL_LIMIT_NARROW: &str =
    "menu/Tab focus · 1/2/3 window · Ctrl-Enter run · SQL LIMIT/OFFSET · q quit · Ctrl-C quit";

/// Footer hints on the create form.
pub const FOOTER_CREATE: &str = "Tab next control · click to focus · Esc back · Ctrl-C quit";

/// Footer hints on the detail page.
pub const FOOTER_DETAIL: &str =
    "Tab focus · Enter activate · Ctrl-Enter send · r refresh · Esc back · Ctrl-C quit";

/// Footer hints on the edit form.
pub const FOOTER_EDIT: &str = "Tab next control · click to focus · Esc back · Ctrl-C quit";

/// Scope note on the create form.
pub const SCOPE_CREATE: &str = "Nothing is sent until Create succeeds";

/// Scope note on the edit form.
pub const SCOPE_EDIT: &str = "Changes are sent only when you save";

/// Scope note on the detail page.
#[must_use]
pub fn scope_detail(loaded: usize, stale: bool) -> String {
    let mut note = format!("Session detail · {}", loaded_label(loaded));
    if stale {
        note.push_str(" · stale");
    }
    note
}

/// Compatibility note for callers without a loaded count.
pub const SCOPE_DETAIL: &str = "Session detail";

/// Session detail id line.
#[must_use]
pub fn session_id_label(id: &str) -> String {
    format!("Session {id}")
}

// ---------------------------------------------------------------------------
// Too small
// ---------------------------------------------------------------------------

/// Shown instead of the page when the terminal is too small to lay out.
pub const TERMINAL_TOO_SMALL: &str = "Terminal too small";

/// Minimum size hint below [`TERMINAL_TOO_SMALL`].
pub const TERMINAL_TOO_SMALL_HINT: &str = "Resize to at least 50x12 to continue.";

/// Prefix for a fatal startup error printed after the terminal is restored.
pub const STARTUP_ERROR: &str = "peri-swarm dashboard failed to start";
