//! Every user-facing English string in the dashboard.
//!
//! Pages format values, but the wording lives here so a future locale layer can
//! replace it in one place. No string in this module ever contains a secret.

use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use serde_json::Value;

use crate::app::{ActionId, DetailTab, FieldId, HealthState, Page, SidebarGroup};
use crate::metrics::TimeWindow;
use crate::query::SqlError;
use crate::view_model::{Column, StatusBucket};

mod detail;
pub use detail::*;
mod errors;
pub use errors::*;

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
    let Some(last) = last else {
        return "last refresh: never".to_owned();
    };
    let seconds = now.saturating_duration_since(last).as_secs();
    match seconds {
        0 | 1 => "last refresh: just now".to_owned(),
        2..=59 => format!("last refresh: {seconds}s ago"),
        60..=3599 => format!("last refresh: {}m ago", seconds / 60),
        _ => format!("last refresh: {}h ago", seconds / 3600),
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

// ---------------------------------------------------------------------------
// Footer hints
// ---------------------------------------------------------------------------

/// Footer hints on wide main pages.
pub const FOOTER_MAIN: &str =
    "Tab focus · up/down page · r refresh · n new session · h help · q quit · Ctrl-C quit";

/// Footer hints on narrow main pages where the sidebar is behind the menu button.
pub const FOOTER_MAIN_NARROW: &str =
    "menu/Tab focus · r refresh · n new session · h help · q quit · Ctrl-C quit";

/// Footer hints while a text field owns focus.
pub const FOOTER_FIELD: &str = "Tab next control · Enter submit · Esc leave field · Ctrl-C quit";

/// Footer hints on Settings.
pub const FOOTER_SETTINGS: &str =
    "Tab focus · Enter activate · Esc back · h help · q quit · Ctrl-C quit";

/// Footer hints while a modal is open.
pub const FOOTER_OVERLAY: &str = "Esc close · up/down move · Enter select · mouse: click a choice";

/// Footer hints on the too-small screen.
pub const FOOTER_TOO_SMALL: &str = "Ctrl-C quit";

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
#[must_use]
pub fn unauthorized_banner(message: &str) -> String {
    format!("Authentication failed: {message}. Re-authenticate from Settings.")
}

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

/// Sign-out overlay hint.
pub const SIGNOUT_HINT: &str = "Enter confirms · Esc cancels";

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

// ---------------------------------------------------------------------------
// Help overlay
// ---------------------------------------------------------------------------

/// Help overlay title.
pub const HELP_TITLE: &str = "Getting started";

/// Help overlay footer.
pub const HELP_HINT: &str = "Esc closes this overlay · click outside to dismiss";

/// One help section.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HelpSection {
    /// Section heading.
    pub title: &'static str,
    /// Body lines.
    pub lines: &'static [&'static str],
}

/// Help content.
pub const HELP_SECTIONS: [HelpSection; 5] = [    HelpSection {
        title: "Credentials",
        lines: &[
            "On first run, enter the gateway base URL and an API key.",
            "Connect runs GET /healthz and an authenticated session list before saving anything.",
            "The key is stored in ~/.config/peri-swarm-dashboard/config.toml with mode 0600 and is never displayed.",
        ],
    },
    HelpSection {
        title: "Opening a session",
        lines: &[
            "The Session log lists the most recently active sessions first.",
            "Rows are paged: use the paginator under the table or PgUp/PgDn to change page.",
            "Click a row, or focus the page and press Enter, to open that session.",
        ],
    },
    HelpSection {
        title: "Sending input and approving",
        lines: &[
            "Open a session and type in the composer at the bottom of the page.",
            "Ctrl-Enter sends the message. When a turn requests approval, approve or deny it from the session header.",
        ],
    },
    HelpSection {
        title: "Sign out",
        lines: &[
            "Settings → Sign out deletes the saved credentials and returns to onboarding.",
            "Signing out does not cancel a running turn on the gateway.",
        ],
    },
    HelpSection {
        title: "Mouse and keyboard",
        lines: &[
            "Click sidebar items, buttons, and fields. The wheel scrolls the sidebar or the page under the pointer.",
            "Tab and Shift-Tab move focus; arrows navigate; Enter activates; Esc goes back.",
            "Ctrl-C quits from anywhere. q quits only when no text field owns focus.",
        ],
    },
];

// ---------------------------------------------------------------------------
// Metrics pages
// ---------------------------------------------------------------------------

/// Core-metrics page title.
pub const CORE_METRICS_TITLE: &str = "Core metrics";

/// Agent-metrics page title.
pub const AGENT_METRICS_TITLE: &str = "Agent metrics";

/// Request-log page title.
pub const REQUEST_LOG_TITLE: &str = "Request log";

/// Short window label used by the toolbar.
#[must_use]
pub fn window_label(window: TimeWindow) -> &'static str {
    match window {
        TimeWindow::FifteenMinutes => "15m",
        TimeWindow::OneHour => "1h",
        TimeWindow::TwentyFourHours => "24h",
    }
}

/// Scope note for Core metrics.
#[must_use]
pub fn core_metrics_scope(window: TimeWindow, stale: bool) -> String {
    let mut note = format!(
        "Window {} · source OpenObserve metrics",
        window_label(window)
    );
    if stale {
        note.push_str(" · stale");
    }
    note
}

/// Scope note for Agent metrics.
#[must_use]
pub fn agent_metrics_scope(
    window: TimeWindow,
    loaded: usize,
    truncated: bool,
    stale: bool,
) -> String {
    let mut note = format!(
        "Window {} · source gateway agent metrics · {}",
        window_label(window),
        sessions_label(loaded)
    );
    if truncated {
        note.push_str(" · truncated");
    }
    if stale {
        note.push_str(" · stale");
    }
    note
}

/// Scope note for the request log.
#[must_use]
pub fn request_log_scope(window: TimeWindow, page: usize, stale: bool) -> String {
    let mut note = format!(
        "Window {} · source OpenObserve logs · page {}",
        window_label(window),
        page.saturating_add(1)
    );
    if stale {
        note.push_str(" · stale");
    }
    note
}

/// Requests-per-minute value.
#[must_use]
pub fn requests_per_minute_value(value: Option<f64>) -> String {
    match value.filter(|value| value.is_finite()) {
        Some(value) => format!("{value:.1}"),
        None => DASH.to_owned(),
    }
}

/// Error-rate value as a percentage.
#[must_use]
pub fn error_rate_value(value: Option<f64>) -> String {
    match value.filter(|value| value.is_finite()) {
        Some(value) => format!("{:.1}%", value * 100.0),
        None => DASH.to_owned(),
    }
}

/// p95 latency value in milliseconds.
#[must_use]
pub fn p95_value(seconds: Option<f64>) -> String {
    match seconds.filter(|value| value.is_finite()) {
        Some(seconds) => format!("{:.0} ms", seconds * 1000.0),
        None => DASH.to_owned(),
    }
}

/// Integer total with an em dash for an unreported value.
#[must_use]
pub fn total_value(value: Option<u64>) -> String {
    value.map_or_else(|| DASH.to_owned(), |value| value.to_string())
}

/// Actual sample range covered by a Core-metrics snapshot.
#[must_use]
pub fn data_range(start_us: Option<u64>, end_us: Option<u64>) -> String {
    match (start_us, end_us) {
        (Some(start), Some(end)) => format!("{} – {}", clock(start), clock(end)),
        _ => DASH.to_owned(),
    }
}

/// Core-metrics labels.
pub const LABEL_REQUESTS_PER_MINUTE: &str = "Requests / min";
/// 5xx error-rate label.
pub const LABEL_ERROR_RATE: &str = "Error rate (5xx)";
/// p95 latency label.
pub const LABEL_P95_LATENCY: &str = "p95 latency";
/// Total request label.
pub const LABEL_TOTAL_REQUESTS: &str = "Total requests";
/// Data-range label.
pub const LABEL_DATA_RANGE: &str = "Data range";

/// OpenObserve is not configured.
pub const OBSERVABILITY_NOT_CONFIGURED_MESSAGE: &str = "Observability is not configured.";

/// How to configure OpenObserve.
pub const OBSERVABILITY_NOT_CONFIGURED_HINT: &str =
    "Add an OpenObserve connection in Settings to load this page.";

/// Initial Core-metrics load.
pub const LOADING_CORE_METRICS: &str = "Loading core metrics…";

/// Initial Agent-metrics load.
pub const LOADING_AGENT_METRICS: &str = "Loading agent metrics…";

/// Initial request-log load.
pub const LOADING_REQUEST_LOG: &str = "Loading requests…";

/// No metric samples in the window.
pub const EMPTY_CORE_METRICS: &str = "No metric data in this window.";

/// Empty Core-metrics explanation.
pub const EMPTY_CORE_METRICS_HINT: &str =
    "OpenObserve reported no samples for the selected window.";

/// No usage in the window.
pub const EMPTY_AGENT_METRICS: &str = "No usage reported in this window.";

/// Empty Agent-metrics explanation.
pub const EMPTY_AGENT_METRICS_HINT: &str =
    "Only sessions with at least one usage-carrying turn appear here.";

/// No access-log rows in the window.
pub const EMPTY_REQUEST_LOG: &str = "No requests in this window.";

/// Empty request-log explanation.
pub const EMPTY_REQUEST_LOG_HINT: &str = "The access log has no entries for the selected window.";

/// Core-metrics query failure.
#[must_use]
pub fn core_metrics_error(detail: &str) -> String {
    format!("Could not load core metrics: {detail}")
}

/// Agent-metrics query failure.
#[must_use]
pub fn agent_metrics_error(detail: &str) -> String {
    format!("Could not load agent metrics: {detail}")
}

/// Request-log query failure.
#[must_use]
pub fn request_log_error(detail: &str) -> String {
    format!("Could not load requests: {detail}")
}

/// Visible marker when a failed refresh kept the previous data on screen.
pub const STALE_DATA: &str = "Showing the last successful data; the latest refresh failed.";

/// Agent table headers.
pub const AGENT_COL_SESSION: &str = "Session";
/// Model column.
pub const AGENT_COL_MODEL: &str = "Model";
/// Input token column.
pub const AGENT_COL_INPUT: &str = "Input";
/// Output token column.
pub const AGENT_COL_OUTPUT: &str = "Output";
/// Combined cache token column.
pub const AGENT_COL_CACHE: &str = "Cache";
/// Total token column.
pub const AGENT_COL_TOTAL: &str = "Total";
/// Turn count column.
pub const AGENT_COL_TURNS: &str = "Turns";
/// Last activity column.
pub const AGENT_COL_LAST_ACTIVE: &str = "Last active";

/// Exact truncation notice required by the gateway contract.
pub const AGENT_METRICS_TRUNCATED: &str = "Window truncated; totals cover the newest 500 sessions.";

/// `1 session` / `N sessions`.
#[must_use]
pub fn sessions_label(count: usize) -> String {
    match count {
        1 => "1 session".to_owned(),
        count => format!("{count} sessions"),
    }
}

/// Totals line built from the gateway `totals` object.
#[must_use]
pub fn agent_totals_line(totals: &crate::api::AgentMetricsTotals, sessions: usize) -> String {
    format!(
        "Totals · {} · input {} · output {} · cache {} · total {} · turns {} · calls {} · model {}",
        sessions_label(sessions),
        token_value(totals.input_tokens),
        token_value(totals.output_tokens),
        cache_value(totals.cache_read_tokens, totals.cache_creation_tokens),
        token_value(totals.total_tokens),
        totals.turns,
        totals.calls,
        model_value(totals.model.as_deref()),
    )
}

/// Token count with an em dash for an unreported value.
#[must_use]
pub fn token_value(value: Option<u64>) -> String {
    value.map_or_else(|| DASH.to_owned(), |value| value.to_string())
}

/// Combined cache tokens: read + creation. Both missing renders an em dash.
#[must_use]
pub fn cache_value(read: Option<u64>, creation: Option<u64>) -> String {
    match (read, creation) {
        (None, None) => DASH.to_owned(),
        (Some(read), Some(creation)) => read.saturating_add(creation).to_string(),
        (Some(value), None) | (None, Some(value)) => value.to_string(),
    }
}

/// Model name or an em dash.
#[must_use]
pub fn model_value(model: Option<&str>) -> String {
    model
        .filter(|model| !model.is_empty())
        .map_or_else(|| DASH.to_owned(), str::to_owned)
}

/// Relative last-activity label for an RFC3339 gateway timestamp.
#[must_use]
pub fn agent_last_active(value: &str, now_unix: i64) -> String {
    if value.is_empty() {
        return DASH.to_owned();
    }
    match DateTime::parse_from_rfc3339(value) {
        Ok(timestamp) => relative_time(timestamp.timestamp(), now_unix),
        // The value is gateway-provided display data, not a secret; show it
        // unchanged when this client cannot parse it.
        Err(_) => value.to_owned(),
    }
}

/// Request-log table headers.
pub const REQUEST_COL_TIME: &str = "Time";
/// HTTP method column.
pub const REQUEST_COL_METHOD: &str = "Method";
/// Route column.
pub const REQUEST_COL_ROUTE: &str = "Route";
/// Status column.
pub const REQUEST_COL_STATUS: &str = "Status";
/// Duration column.
pub const REQUEST_COL_DURATION: &str = "Duration";
/// Session column.
pub const REQUEST_COL_SESSION: &str = "Session";

/// Request timestamp in local time.
#[must_use]
pub fn request_time(timestamp_us: Option<u64>) -> String {
    timestamp_us.map_or_else(|| DASH.to_owned(), clock)
}

/// HTTP status or an em dash.
#[must_use]
pub fn request_status(status: Option<u16>) -> String {
    status.map_or_else(|| DASH.to_owned(), |status| status.to_string())
}

/// Request duration in milliseconds.
#[must_use]
pub fn request_duration(duration_ms: Option<u64>) -> String {
    duration_ms.map_or_else(|| DASH.to_owned(), |duration| format!("{duration} ms"))
}

/// Optional request-log text (method, route, session id).
#[must_use]
pub fn request_optional(value: Option<&str>) -> String {
    value
        .filter(|value| !value.is_empty())
        .map_or_else(|| DASH.to_owned(), str::to_owned)
}

/// `Page 1 / 3` for client-side paging.
#[must_use]
pub fn page_indicator(page: usize, page_count: usize) -> String {
    format!("Page {} / {}", page.saturating_add(1), page_count.max(1))
}

/// `Page 1` for server-side paging without a known total.
#[must_use]
pub fn request_page_indicator(page: usize) -> String {
    format!("Page {}", page.saturating_add(1))
}

/// Local `mm-dd hh:mm:ss` label for a Unix-microsecond timestamp.
fn clock(timestamp_us: u64) -> String {
    let Ok(timestamp) = i64::try_from(timestamp_us) else {
        return DASH.to_owned();
    };
    DateTime::from_timestamp_micros(timestamp).map_or_else(
        || DASH.to_owned(),
        |timestamp| {
            timestamp
                .with_timezone(&Local)
                .format("%m-%d %H:%M:%S")
                .to_string()
        },
    )
}

// ---------------------------------------------------------------------------
// Query page
// ---------------------------------------------------------------------------

/// Query page title.
pub const QUERY_TITLE: &str = "Query";

/// SQL editor label.
pub const QUERY_SQL_LABEL: &str = "SQL";

/// Empty-editor placeholder.
pub const QUERY_SQL_PLACEHOLDER: &str = "SELECT * FROM \"peri_swarm_logs\" LIMIT 20";

/// Editor hint, shown under the text area.
pub const QUERY_HINT: &str = "Read-only SELECT · Ctrl-Enter runs the query";

/// Loading label while a query is running.
pub const QUERY_RUNNING: &str = "Running…";

/// A completed query matched no rows.
pub const QUERY_EMPTY: &str = "No rows returned.";

/// Empty-result explanation.
pub const QUERY_EMPTY_HINT: &str = "The statement completed and matched no rows in this window.";

/// Longest cell value shown in the result table, before the ellipsis.
pub const QUERY_CELL_MAX_CHARS: usize = 96;

/// Scope note for the Query page.
#[must_use]
pub fn query_scope(window: TimeWindow, page: usize) -> String {
    format!(
        "Window {} · source OpenObserve logs and traces · page {} · manual run",
        window_label(window),
        page.saturating_add(1)
    )
}

/// Query failure.
#[must_use]
pub fn query_error(detail: &str) -> String {
    format!("Could not run the query: {detail}")
}

/// Local SQL validation message. The statement is never echoed.
#[must_use]
pub fn sql_error(error: SqlError) -> &'static str {
    match error {
        SqlError::Empty => "Enter a SELECT statement.",
        SqlError::TooLarge => "Query is too large (limit 8 KiB).",
        SqlError::NotSelect => "Query must be a single SELECT statement.",
        SqlError::Semicolon => "Only a single trailing semicolon is allowed.",
        SqlError::Nul => "Query must not contain NUL characters.",
    }
}

/// Notice shown when the result has more columns than fit on screen.
#[must_use]
pub fn query_columns_truncated(shown: usize, total: usize) -> String {
    format!("Showing {shown} of {total} columns.")
}

/// One result cell.
///
/// `null` and a missing key render as an em dash; nested values become compact
/// JSON; control characters become spaces so one cell cannot break the table
/// row; very long values are cut at [`QUERY_CELL_MAX_CHARS`] with an ellipsis.
#[must_use]
pub fn query_cell(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return DASH.to_owned();
    };
    let text = match value {
        Value::Null => return DASH.to_owned(),
        Value::String(text) => text.clone(),
        Value::Bool(_) | Value::Number(_) => value.to_string(),
        Value::Array(_) | Value::Object(_) => {
            serde_json::to_string(value).unwrap_or_else(|_| DASH.to_owned())
        }
    };
    let cleaned: String = text
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    if cleaned.chars().count() <= QUERY_CELL_MAX_CHARS {
        return cleaned;
    }
    let mut truncated: String = cleaned.chars().take(QUERY_CELL_MAX_CHARS).collect();
    truncated.push('…');
    truncated
}
