//! Wire protocol of the monitor gateway.
//!
//! Three vocabularies coexist, each with its own tag:
//!
//! - [`RegistryMessage`]: MCP → gateway over the UDS registry, tagged by `type`.
//! - [`GatewayMessage`]: gateway → MCP over the same UDS connection, tagged by `type`.
//! - [`ClientMessage`] / [`ServerMessage`]: monitor page ↔ gateway over the
//!   monitor WebSocket, tagged by `t`.
//!
//! Every enum carries an `#[serde(other)] Unknown` variant: an unrecognised tag
//! deserialises successfully and the caller logs and ignores it, so an older
//! gateway keeps working with a newer MCP. A known tag with a malformed payload
//! is still an error; a typo must not silently drop data.
//!
//! The JSON field names are part of the contract (`mcp-packages/peri-browser/README.md`,
//! section 6) and are asserted verbatim in `protocol_test.rs`.

use serde::{Deserialize, Serialize};

/// Agent-side lifecycle reported by the MCP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Driving,
    #[default]
    Idle,
    WaitingForHuman,
}

impl AgentState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Driving => "driving",
            Self::Idle => "idle",
            Self::WaitingForHuman => "waiting_for_human",
        }
    }
}

/// A top-level page owned by one MCP session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageRef {
    pub target_id: String,
    pub url: String,
    pub title: String,
}

/// MCP → gateway registry messages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum RegistryMessage {
    Register {
        session_id: String,
        name: String,
        cdp_url: String,
        pages: Vec<PageRef>,
        state: AgentState,
    },
    UpdatePages {
        session_id: String,
        pages: Vec<PageRef>,
    },
    State {
        session_id: String,
        state: AgentState,
    },
    YieldOk {
        session_id: String,
        page_id: String,
    },
    Reclaim {
        session_id: String,
        page_id: String,
    },
    CommandResult {
        session_id: String,
        request_id: String,
        ok: bool,
        error: Option<String>,
    },
    Unregister {
        session_id: String,
    },
    #[serde(other)]
    Unknown,
}

/// Why the gateway asks the MCP to give up its page controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum YieldReason {
    Takeover,
    Idle,
    RequestHuman,
}

/// Outcome of a human handoff reported back to the MCP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanStatus {
    Done,
    Declined,
    Timeout,
}

/// Page operation requested by a viewer and executed by the MCP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum BrowserCommand {
    Navigate { url: String },
    History { delta: i64 },
    NewTab { url: String },
    CloseTab { page_id: String },
}

/// Gateway → MCP messages on the registry connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum GatewayMessage {
    YieldRequest {
        session_id: String,
        page_id: String,
        reason: YieldReason,
        timeout_ms: u64,
    },
    Command {
        session_id: String,
        request_id: String,
        page_id: String,
        command: BrowserCommand,
    },
    HumanRequest {
        session_id: String,
        request_id: String,
        page_id: String,
        prompt: String,
        timeout_ms: u64,
    },
    HumanResult {
        session_id: String,
        request_id: String,
        status: HumanStatus,
        url: String,
        title: String,
    },
    Reclaimed {
        session_id: String,
        page_id: String,
        ok: bool,
    },
    #[serde(other)]
    Unknown,
}

/// Capabilities advertised in the monitor `hello` frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Caps {
    pub new_tab: bool,
    pub diagnostics: bool,
}

/// One tab in the monitor tab strip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabInfo {
    pub id: String,
    pub url: String,
    pub title: String,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleEntry {
    pub level: String,
    pub text: String,
    /// Milliseconds since the UNIX epoch, as observed by the gateway.
    pub ts: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkEntry {
    pub method: String,
    pub url: String,
    /// `None` while the request is still in flight.
    pub status: Option<u16>,
    /// Milliseconds since the UNIX epoch, as observed by the gateway.
    pub ts: u64,
}

/// Gateway → monitor page frames (text). Screencast JPEGs are sent as binary
/// frames outside this enum, always preceded by a [`ServerMessage::Meta`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ServerMessage {
    Hello {
        session: String,
        name: String,
        agent: AgentState,
        interactive: bool,
        caps: Caps,
    },
    Tabs {
        tabs: Vec<TabInfo>,
    },
    Agent {
        state: AgentState,
    },
    /// Where the official Chrome DevTools can attach to the active page. The
    /// monitor cannot navigate to `devtools://` itself, so it shows the URL.
    Devtools {
        url: String,
        page_id: String,
    },
    Nav {
        page_id: String,
        url: String,
        title: String,
        can_go_back: bool,
        can_go_forward: bool,
    },
    Meta {
        page_id: String,
        viewport_width: u32,
        viewport_height: u32,
        device_width: u32,
        device_height: u32,
        page_scale_factor: f64,
        scroll_offset_x: f64,
        scroll_offset_y: f64,
    },
    Thumb {
        page_id: String,
        data: String,
    },
    Handoff {
        active: bool,
        prompt: Option<String>,
        request_id: Option<String>,
    },
    Console {
        page_id: String,
        entries: Vec<ConsoleEntry>,
    },
    Network {
        page_id: String,
        entries: Vec<NetworkEntry>,
    },
    Error {
        code: String,
        message: String,
    },
    #[serde(other)]
    Unknown,
}

/// Input vocabulary is shared with the CDP layer (`peri-browser`): the monitor
/// wire names and the `Input.*` payloads must stay one definition.
pub use peri_browser::input::{KeyEventKind, MouseButton, MouseEventKind};

/// Monitor page → gateway messages.
///
/// Pointer coordinates are in the device space of the last frame's
/// [`ServerMessage::Meta`] (`deviceWidth`/`deviceHeight`). When the screencast
/// bound clamps a frame, its JPEG pixel size differs from that box.
/// The gateway maps it to CSS pixels with the live `Page.getLayoutMetrics`
/// viewport (`ws::canvas_to_css`), so the client must not pre-scale them to
/// `viewportWidth`/`viewportHeight`.
/// Keyboard modifier bitmask: Alt = 1, Ctrl = 2, Meta = 4, Shift = 8.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ClientMessage {
    Ack,
    /// Ask for the official Chrome DevTools URL. The gateway releases the page
    /// controller first: Kitesurf lets one controller attach at a time.
    Devtools,
    Switch {
        page_id: String,
    },
    Visible {
        visible: bool,
    },
    Mouse {
        #[serde(rename = "type")]
        event: MouseEventKind,
        x: f64,
        y: f64,
        #[serde(default)]
        button: Option<MouseButton>,
        #[serde(default)]
        click_count: Option<u32>,
    },
    Wheel {
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
    },
    Key {
        #[serde(rename = "type")]
        event: KeyEventKind,
        key: String,
        code: String,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        modifiers: u32,
    },
    Text {
        text: String,
    },
    Navigate {
        url: String,
    },
    History {
        delta: i64,
    },
    NewTab {
        url: String,
    },
    CloseTab {
        page_id: String,
    },
    Takeover,
    Release,
    Answer {
        request_id: String,
        accepted: bool,
    },
    #[serde(other)]
    Unknown,
}

impl ClientMessage {
    /// Wire tag of this message, used in logs.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Ack => "ack",
            Self::Switch { .. } => "switch",
            Self::Visible { .. } => "visible",
            Self::Mouse { .. } => "mouse",
            Self::Wheel { .. } => "wheel",
            Self::Key { .. } => "key",
            Self::Text { .. } => "text",
            Self::Navigate { .. } => "navigate",
            Self::History { .. } => "history",
            Self::NewTab { .. } => "newTab",
            Self::CloseTab { .. } => "closeTab",
            Self::Devtools => "devtools",
            Self::Takeover => "takeover",
            Self::Release => "release",
            Self::Answer { .. } => "answer",
            Self::Unknown => "unknown",
        }
    }
}

#[cfg(test)]
#[path = "protocol_test.rs"]
mod tests;
