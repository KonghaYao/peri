//! Typed browser errors and their stable tool-facing codes.
//!
//! Tool failures are MCP tool errors (`isError`), never process exits. Codes are
//! stable English strings so a host can branch on them; the message and hint
//! stay actionable for the model (README §7).

use std::path::PathBuf;

use peri_browser_cdp::CdpError;

#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    #[error("browser is not connected: no CDP endpoint configured")]
    NotConnected,
    #[error("no active page in this session")]
    NoActivePage,
    #[error("page {0:?} is not owned by this session")]
    PageNotFound(String),
    #[error("page reference {0:?} matches more than one page; use the page id")]
    AmbiguousPage(String),
    #[error("invalid argument {name}: {reason}")]
    InvalidArgument { name: String, reason: String },
    #[error("{0}")]
    GatewayUnavailable(String),
    #[error("a human hand-off is in progress for this session")]
    WaitingForHuman,
    #[error("the page controller has been yielded to the monitor")]
    Yielded,
    #[error("uid {0:?} is not part of the latest snapshot")]
    StaleUid(String),
    #[error("element {0:?} was not found")]
    ElementNotFound(String),
    #[error("element {0:?} has no visible box")]
    NotVisible(String),
    #[error("{0}")]
    Timeout(String),
    #[error("create/close lock {0} was not acquired in time")]
    LockTimeout(PathBuf),
    #[error("browser endpoint rejected {method}: {message}")]
    BrowserControl { method: String, message: String },
    #[error("script evaluation failed: {0}")]
    Script(String),
    #[error("internal error: {0}")]
    Internal(String),
    #[error(transparent)]
    Cdp(#[from] CdpError),
}

impl BrowserError {
    /// Stable machine code returned in the tool error text.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotConnected => "not_connected",
            Self::NoActivePage => "no_active_page",
            Self::PageNotFound(_) => "page_not_found",
            Self::AmbiguousPage(_) => "ambiguous_page",
            Self::InvalidArgument { .. } => "invalid_argument",
            Self::GatewayUnavailable(_) => "gateway_unavailable",
            Self::WaitingForHuman => "waiting_for_human",
            Self::Yielded => "page_busy_by_human",
            Self::StaleUid(_) => "stale_uid",
            Self::ElementNotFound(_) => "element_not_found",
            Self::NotVisible(_) => "element_not_visible",
            Self::Timeout(_) => "timeout",
            Self::LockTimeout(_) => "provision_lock_timeout",
            Self::BrowserControl { .. } => "browser_control",
            Self::Script(_) => "script_error",
            Self::Internal(_) => "internal_error",
            Self::Cdp(error) => cdp_code(error),
        }
    }

    /// Short next step for the model, appended to the error text.
    #[must_use]
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Self::NotConnected => Some(
                "Start Kitesurf and point KITESURF_CDP_URL at its ws:// endpoint (or set PERI_BROWSER_CDP_URL for a direct run), then restart this MCP server.",
            ),
            Self::NoActivePage => Some("Call list_pages or new_page first."),
            Self::PageNotFound(_) => Some("Call list_pages to see the pages this session owns."),
            Self::AmbiguousPage(_) => Some("Pass the page id returned by list_pages."),
            Self::StaleUid(_) => Some("Call take_snapshot again and use a uid from that result."),
            Self::NotVisible(_) => Some("Scroll the element into view or wait for it to render."),
            Self::Yielded => Some(
                "The monitor holds the page controller. Retry after the human releases it, or call yield_page deliberately.",
            ),
            Self::WaitingForHuman => Some("Wait for request_human to return."),
            Self::GatewayUnavailable(_) => Some(
                "Start peri-browser-gateway (or set PERI_BROWSER_GATEWAY_SOCK) and retry.",
            ),
            Self::LockTimeout(_) => Some(
                "Another session is creating or closing a page; retry in a moment.",
            ),
            Self::Cdp(error) => cdp_hint(error),
            Self::ElementNotFound(_) => Some(
                "Call take_snapshot again and pass a uid or selector that exists on the page.",
            ),
            Self::Timeout(_) => Some("Retry after the page settles, or raise the configured timeout."),
            Self::BrowserControl { .. } | Self::InvalidArgument { .. } | Self::Script(_) => None,
            Self::Internal(_) => None,
        }
    }

    /// Text placed in the MCP content block: `code: message` plus a hint.
    #[must_use]
    pub fn render(&self) -> String {
        match self.hint() {
            Some(hint) => format!("{}: {self}\n{hint}", self.code()),
            None => format!("{}: {self}", self.code()),
        }
    }
}

fn cdp_code(error: &CdpError) -> &'static str {
    match error {
        CdpError::Timeout { .. } => "egress_timeout",
        CdpError::Closed { .. } => "cdp_closed",
        CdpError::Connect { .. } => "cdp_connect",
        CdpError::InvalidUrl { .. } => "cdp_invalid_url",
        CdpError::Decode(_) => "cdp_decode",
        CdpError::WebSocket(_) => "cdp_websocket",
        CdpError::Protocol {
            code: -32000,
            message,
        } if message.contains("Execution context") => "context_destroyed",
        CdpError::Protocol { message, .. } | CdpError::Rejected { message, .. } => {
            if message.contains("Another controller owns this page session") {
                "another_controller"
            } else if message.contains("Target belongs to another page session") {
                "target_owned_elsewhere"
            } else if message.contains("This method is not available on a page-session connection")
            {
                "method_not_available"
            } else if message.contains("Control has changed") {
                "control_changed"
            } else if message.contains("Viewer connection cannot control the browser") {
                "viewer_denied"
            } else if message.contains("Target expired") {
                "target_expired"
            } else {
                "cdp_error"
            }
        }
    }
}

fn cdp_hint(error: &CdpError) -> Option<&'static str> {
    match cdp_code(error) {
        "egress_timeout" => Some("The CDP endpoint did not answer in time; retry."),
        "cdp_closed" | "target_expired" => Some(
            "The page target is gone. Call list_pages; create a new page if it is no longer listed.",
        ),
        "cdp_connect" => Some(
            "Check that Kitesurf is running and that KITESURF_CDP_URL points at its CDP endpoint.",
        ),
        "another_controller" => Some(
            "Another writer owns this page. The monitor may be holding it; the agent reclaims it on the next operation, or use yield_page deliberately.",
        ),
        "target_owned_elsewhere" => Some("Select a page this session owns."),
        "method_not_available" => Some(
            "This command is instance-scoped and is not available on a page-session connection.",
        ),
        "context_destroyed" => Some("The page navigated; take a fresh snapshot or retry."),
        _ => None,
    }
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
