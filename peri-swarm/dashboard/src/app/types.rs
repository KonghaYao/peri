//! Application data types.
//!
//! Kept separate from [`super::App`] so the state machine file stays small.
//! [`Effect`] is deliberately not `Debug`: it may carry an API key.

use crate::api::{ApprovalDecision, CreateSessionSpec, IdempotencyKey, UpdateSessionSpec};
use crate::metrics::TimeWindow;
use crate::ui::text;
use crate::view_model::StatusBucket;
use crate::worker::ValidationPurpose;

/// Which top-level screen is active.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Screen {
    /// First run or after sign out.
    Onboarding,
    /// The sidebar shell with one page.
    Main,
    /// Session detail page with Timeline / Turns / Info tabs.
    SessionDetail,
    /// Placeholder route for the create-session form.
    SessionCreate,
    /// Edit form for an existing session (model, metadata, prompt cache).
    SessionEdit,
    /// Settings sub-page for editing credentials.
    SettingsCredentials,
    /// Settings sub-page for the OpenObserve connection.
    SettingsObservability,
}

/// Every sidebar page, in sidebar order.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Page {
    /// Core metrics.
    CoreMetrics,
    /// Agent metrics.
    AgentMetrics,
    /// Sandbox metrics.
    SandboxMetrics,
    /// Session log (default page).
    SessionLog,
    /// Request log.
    RequestLog,
    /// Ad-hoc read-only query.
    Query,
    /// Agents.
    Agents,
    /// Artifacts.
    Artifacts,
    /// Vaults.
    Vaults,
    /// MCP servers.
    Mcp,
    /// Automations.
    Automations,
    /// Nodes.
    Nodes,
    /// Settings.
    Settings,
}

impl Page {
    /// All pages in sidebar order.
    pub const ALL: [Page; 13] = [
        Page::CoreMetrics,
        Page::AgentMetrics,
        Page::SandboxMetrics,
        Page::SessionLog,
        Page::RequestLog,
        Page::Query,
        Page::Agents,
        Page::Artifacts,
        Page::Vaults,
        Page::Mcp,
        Page::Automations,
        Page::Nodes,
        Page::Settings,
    ];

    /// Sidebar group this page belongs to.
    #[must_use]
    pub fn group(self) -> SidebarGroup {
        match self {
            Page::CoreMetrics
            | Page::AgentMetrics
            | Page::SandboxMetrics
            | Page::SessionLog
            | Page::RequestLog
            | Page::Query => SidebarGroup::Monitor,
            Page::Agents | Page::Artifacts | Page::Vaults | Page::Mcp => SidebarGroup::Resources,
            Page::Automations | Page::Nodes | Page::Settings => SidebarGroup::Platform,
        }
    }

    /// Short English label from the centralized text module.
    #[must_use]
    pub fn label(self) -> &'static str {
        text::page_label(self)
    }

    /// Whether this build implements the page.
    #[must_use]
    pub fn is_implemented(self) -> bool {
        matches!(
            self,
            Page::CoreMetrics
                | Page::AgentMetrics
                | Page::SessionLog
                | Page::RequestLog
                | Page::Settings
        )
    }
}

/// Sidebar group headings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SidebarGroup {
    /// Monitor.
    Monitor,
    /// Resources.
    Resources,
    /// Platform.
    Platform,
    /// Pinned bottom actions.
    Bottom,
}

impl SidebarGroup {
    /// English heading.
    #[must_use]
    pub fn label(self) -> &'static str {
        text::group_label(self)
    }
}

/// One sidebar entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NavItem {
    /// Group heading this item appears under.
    pub group: SidebarGroup,
    /// Destination page.
    pub page: Page,
}

/// All 13 sidebar entries.
pub const NAV_ENTRIES: [NavItem; 13] = [
    NavItem {
        group: SidebarGroup::Monitor,
        page: Page::CoreMetrics,
    },
    NavItem {
        group: SidebarGroup::Monitor,
        page: Page::AgentMetrics,
    },
    NavItem {
        group: SidebarGroup::Monitor,
        page: Page::SandboxMetrics,
    },
    NavItem {
        group: SidebarGroup::Monitor,
        page: Page::SessionLog,
    },
    NavItem {
        group: SidebarGroup::Monitor,
        page: Page::RequestLog,
    },
    NavItem {
        group: SidebarGroup::Monitor,
        page: Page::Query,
    },
    NavItem {
        group: SidebarGroup::Resources,
        page: Page::Agents,
    },
    NavItem {
        group: SidebarGroup::Resources,
        page: Page::Artifacts,
    },
    NavItem {
        group: SidebarGroup::Resources,
        page: Page::Vaults,
    },
    NavItem {
        group: SidebarGroup::Resources,
        page: Page::Mcp,
    },
    NavItem {
        group: SidebarGroup::Platform,
        page: Page::Automations,
    },
    NavItem {
        group: SidebarGroup::Platform,
        page: Page::Nodes,
    },
    NavItem {
        group: SidebarGroup::Platform,
        page: Page::Settings,
    },
];

/// Flattened sidebar row count: three group headings plus 13 entries.
pub const NAV_ROW_COUNT: usize = 16;

/// Flat sidebar row index for a page, including group headings.
#[must_use]
pub fn page_row_index(page: Page) -> usize {
    let mut row = 0usize;
    let mut group: Option<SidebarGroup> = None;
    for item in NAV_ENTRIES {
        if group != Some(item.group) {
            group = Some(item.group);
            row += 1;
        }
        if item.page == page {
            return row;
        }
        row += 1;
    }
    0
}

/// Next page in sidebar order, wrapping.
#[must_use]
pub fn next_page(page: Page) -> Page {
    match Page::ALL.iter().position(|candidate| *candidate == page) {
        Some(index) => Page::ALL[peri_tui_components::list_nav::cycle_next(index, Page::ALL.len())],
        None => Page::SessionLog,
    }
}

/// Previous page in sidebar order, wrapping.
#[must_use]
pub fn previous_page(page: Page) -> Page {
    match Page::ALL.iter().position(|candidate| *candidate == page) {
        Some(index) => {
            Page::ALL[peri_tui_components::list_nav::cycle_previous(index, Page::ALL.len())]
        }
        None => Page::ALL[Page::ALL.len() - 1],
    }
}

/// Editable field identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldId {
    /// Onboarding base URL.
    OnboardingBaseUrl,
    /// Onboarding API key.
    OnboardingApiKey,
    /// Settings credential-edit base URL.
    CredentialsBaseUrl,
    /// Settings credential-edit API key.
    CredentialsApiKey,
    /// Observability editor base URL.
    ObservabilityBaseUrl,
    /// Observability editor organization.
    ObservabilityOrg,
    /// Observability editor user.
    ObservabilityUser,
    /// Observability editor password.
    ObservabilityPassword,
    /// Session log search box.
    SessionSearch,
    /// Create form: optional logical id.
    CreateLogicalId,
    /// Create form: model.
    CreateModel,
    /// Create form: working directory.
    CreateCwd,
    /// Create form: instructions (multiline).
    CreateInstructions,
    /// Create form: metadata key of row `index`.
    CreateMetadataKey(usize),
    /// Create form: metadata value of row `index`.
    CreateMetadataValue(usize),
    /// Create form: advanced tools JSON (multiline).
    CreateTools,
    /// Create form: advanced environment JSON (multiline).
    CreateEnvironment,
    /// Create form: advanced prompt-cache JSON (multiline).
    CreatePromptCache,
    /// Session detail: message composer (multiline).
    DetailComposer,
    /// Edit form: model.
    EditModel,
    /// Edit form: metadata key of row `index`.
    EditMetadataKey(usize),
    /// Edit form: metadata value of row `index`.
    EditMetadataValue(usize),
    /// Edit form: advanced prompt-cache JSON (multiline).
    EditPromptCache,
}

/// Caret placement from a click inside a multiline text area.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextAreaClick {
    /// Row relative to the first inner cell of the area.
    pub row: u16,
    /// Column relative to the first inner cell of the area.
    pub column: u16,
    /// Inner width in cells.
    pub width: u16,
    /// Inner height in cells.
    pub height: u16,
}

/// Caret placement from a click inside a field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FieldClick {
    /// Column relative to the first inner cell.
    pub column: u16,
    /// Inner width in cells.
    pub width: u16,
}

/// Focusable action identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionId {
    /// Validate and save onboarding credentials.
    Submit,
    /// Settings: test the saved connection.
    TestConnection,
    /// Settings: open the credential editor.
    EditCredentials,
    /// Settings: open the OpenObserve connection editor.
    ConfigureObservability,
    /// Observability editor: probe the entered connection.
    TestObservability,
    /// Observability editor: persist the entered connection.
    SaveObservability,
    /// Observability editor: leave without saving.
    CancelObservability,
    /// Open the sign-out confirmation.
    SignOut,
    /// Quit the dashboard.
    Quit,
    /// Sidebar bottom: open the getting-started overlay.
    ShowGettingStarted,
    /// Credential editor: validate and save.
    SaveCredentials,
    /// Leave the current page.
    Back,
    /// Confirm sign out (destructive).
    ConfirmSignOut,
    /// Cancel sign out.
    CancelSignOut,
    /// Error banner: retry the connection.
    Retry,
    /// Error banner: open Settings.
    OpenSettings,
    /// Help overlay: close.
    CloseHelp,
    /// Session log: open the create form.
    NewSession,
    /// Session log: refetch the current limit.
    RefreshSessions,
    /// Create form: submit through the worker.
    CreateSubmit,
    /// Create form: append an empty metadata row.
    AddMetadata,
    /// Create form: expand or collapse the advanced JSON section.
    ToggleAdvanced,
    /// Detail: refetch the session snapshot and its tabs.
    RefreshDetail,
    /// Timeline: load older items with the page cursor.
    LoadEarlierItems,
    /// Timeline: return to the newest items.
    JumpToLatest,
    /// Turns: append the next page.
    LoadMoreTurns,
    /// Composer: submit the message.
    SendMessage,
    /// Approvals: approve the first pending request (keyboard fallback).
    ApproveFirst,
    /// Approvals: deny the first pending request (keyboard fallback).
    DenyFirst,
    /// Detail: open the edit form.
    EditSession,
    /// Detail: open the delete confirmation.
    DeleteSession,
    /// Detail: open the cancel-turn confirmation.
    CancelTurn,
    /// Delete confirmation: confirm.
    ConfirmDeleteSession,
    /// Delete confirmation: dismiss.
    CancelDeleteSession,
    /// Cancel-turn confirmation: confirm.
    ConfirmCancelTurn,
    /// Cancel-turn confirmation: dismiss.
    CancelCancelTurn,
    /// Edit form: submit through the worker.
    EditSubmit,
    /// Core metrics: refetch the selected window.
    RefreshCoreMetrics,
    /// Agent metrics: refetch the selected window.
    RefreshAgentMetrics,
    /// Request log: refetch the current page.
    RefreshRequestLog,
}

/// Tabs of the session detail page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DetailTab {
    /// Item timeline.
    Timeline,
    /// Turn list.
    Turns,
    /// Session facts.
    Info,
}

impl DetailTab {
    /// Every tab, in bar order.
    pub const ALL: [DetailTab; 3] = [DetailTab::Timeline, DetailTab::Turns, DetailTab::Info];

    /// English tab label.
    #[must_use]
    pub fn label(self) -> &'static str {
        text::detail_tab_label(self)
    }
}

/// Focus target for the keyboard.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Focus {
    /// Sidebar navigation.
    Sidebar,
    /// Page content.
    Content,
    /// One text field.
    Field(FieldId),
    /// One action button.
    Action(ActionId),
}

/// Modal overlays. A modal captures every click and key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Overlay {
    /// Getting-started help.
    Help,
    /// Destructive sign-out confirmation.
    ConfirmSignOut,
    /// Narrow-terminal navigation menu.
    NarrowMenu,
    /// Error-banner action chooser.
    ErrorActions,
    /// Destructive delete-session confirmation.
    ConfirmDeleteSession,
    /// Destructive cancel-turn confirmation.
    ConfirmCancelTurn,
}

/// Semantic UI action produced by mouse hit-testing or keyboard routing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Intent {
    /// Switch page from the sidebar.
    SelectPage(Page),
    /// Switch page from the narrow menu.
    SelectMenuPage(Page),
    /// Toggle the narrow menu overlay.
    ToggleMenu,
    /// Open the help overlay.
    OpenHelp,
    /// Open the sign-out confirmation.
    OpenSignOut,
    /// Open the error banner action chooser.
    OpenErrorActions,
    /// Close any overlay.
    CloseOverlay,
    /// Retry the connection shown by the banner.
    Retry,
    /// Navigate to Settings.
    OpenSettings,
    /// Focus a single-line field, optionally placing the caret.
    FocusField {
        /// Field to focus.
        field: FieldId,
        /// Click position, when the focus came from a mouse click.
        click: Option<FieldClick>,
    },
    /// Focus a multiline field; the caret moves to the end of the text.
    FocusTextArea(FieldId),
    /// Focus a multiline field and place the caret from a click.
    FocusTextAreaAt {
        /// Field to focus.
        field: FieldId,
        /// Click position inside the text area.
        click: TextAreaClick,
    },
    /// Focus the sidebar.
    FocusSidebar,
    /// Focus the page content.
    FocusContent,
    /// Focus an action without activating it.
    FocusAction(ActionId),
    /// Focus and activate an action.
    Activate(ActionId),
    /// Scroll the sidebar by a wheel delta.
    ScrollSidebar(i32),
    /// Scroll the page content by a wheel delta.
    ScrollContent(i32),
    /// Confirm the sign-out dialog.
    ConfirmSignOut,
    /// Dismiss the sign-out dialog.
    CancelSignOut,
    /// Leave the current page.
    Back,
    /// Quit.
    Quit,
    /// Session log: apply a status bucket chip.
    SetFilter(StatusBucket),
    /// Session log: open one session by id.
    OpenSession(String),
    /// Session log: open the create form.
    NewSession,
    /// Session log: refetch the loaded limit.
    RefreshSessions,
    /// Session log: show the previous display page.
    SessionPagePrev,
    /// Session log: show the next display page, fetching more rows if needed.
    SessionPageNext,
    /// Session log: jump to a loaded display page (0-based).
    GoToSessionPage(usize),
    /// Metrics pages: select a time window.
    SetMetricsWindow(TimeWindow),
    /// Metrics pages: show the previous display page.
    MetricsPagePrev,
    /// Metrics pages: show the next display page.
    MetricsPageNext,
    /// Session detail: select a tab.
    SelectDetailTab(DetailTab),
    /// Create form: append a metadata row.
    AddMetadataRow,
    /// Create form: remove metadata row `index`.
    RemoveMetadataRow(usize),
    /// Create form: toggle the advanced JSON section.
    ToggleAdvanced,
    /// Create form: submit.
    SubmitCreate,
    /// Detail: toggle one expandable timeline block.
    ToggleTimelineBlock(String),
    /// Detail: load older timeline items with the page cursor.
    LoadEarlierItems,
    /// Detail: jump back to the newest timeline items.
    JumpToLatest,
    /// Detail: append the next turns page.
    LoadMoreTurns,
    /// Detail: refetch the snapshot now.
    RefreshDetail,
    /// Detail: submit the composer text.
    SendMessage,
    /// Detail: answer one pending approval.
    AnswerApproval {
        /// Opaque approval request id.
        request_id: String,
        /// Approve or deny.
        decision: ApprovalDecision,
    },
    /// Detail: open the edit form.
    OpenEditSession,
    /// Edit form: submit.
    SubmitEdit,
    /// Detail: open the delete confirmation.
    OpenDeleteSession,
    /// Detail: open the cancel-turn confirmation.
    OpenCancelTurn,
    /// Confirm the delete dialog.
    ConfirmDeleteSession,
    /// Dismiss the delete dialog.
    CancelDeleteSession,
    /// Confirm the cancel-turn dialog.
    ConfirmCancelTurn,
    /// Dismiss the cancel-turn dialog.
    CancelCancelTurn,
}

/// Result of a health poll.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HealthState {
    /// No poll has completed yet.
    Unknown,
    /// `/healthz` returned `ok`.
    Healthy,
    /// `/healthz` answered but reported another status.
    Degraded(String),
    /// The gateway could not be reached.
    Unreachable(String),
}

/// Banner tone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BannerTone {
    /// Red error banner.
    Error,
    /// Yellow warning banner.
    Warning,
}

/// Which buttons the banner offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BannerActions {
    /// No buttons.
    None,
    /// Retry and Settings.
    RetryAndSettings,
    /// Settings only.
    Settings,
}

/// One banner view for the shell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BannerView {
    /// Error or warning.
    pub tone: BannerTone,
    /// Gateway code or HTTP status, when known.
    pub code: Option<String>,
    /// Safe message; never contains the API key.
    pub message: String,
    /// Available actions.
    pub actions: BannerActions,
    /// Whether this banner suspends authenticated polling.
    pub unauthorized: bool,
}

/// Settings action result line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingsStatus {
    /// Success message.
    Ok(String),
    /// Failure message.
    Error(String),
}

/// Side effects the runtime must perform.
///
/// Deliberately not `Debug`: validation and adoption effects carry the API key.
pub enum Effect {
    /// Probe a candidate OpenObserve connection.
    TestObservability {
        /// UI epoch.
        epoch: u64,
        /// Candidate origin.
        base_url: String,
        /// Candidate organization.
        org: String,
        /// Candidate user.
        user: String,
        /// Candidate password. Never printed.
        password: String,
    },
    /// Validate candidate credentials.
    Validate {
        /// UI epoch.
        epoch: u64,
        /// Why the check runs.
        purpose: ValidationPurpose,
        /// Candidate origin.
        base_url: String,
        /// Candidate key.
        api_key: String,
    },
    /// Revalidate the active connection.
    Revalidate {
        /// UI epoch.
        epoch: u64,
        /// Why the check runs.
        purpose: ValidationPurpose,
    },
    /// Adopt validated credentials in the API worker.
    Adopt {
        /// Validated origin.
        base_url: String,
        /// Validated key.
        api_key: String,
    },
    /// Adopt a newly saved OpenObserve connection in the API worker.
    AdoptObservability {
        /// Validated origin.
        base_url: String,
        /// Validated organization.
        org: String,
        /// Validated user.
        user: String,
        /// Validated password. Never printed.
        password: String,
    },
    /// Run a health poll.
    Health {
        /// UI epoch.
        epoch: u64,
    },
    /// Fetch the session-log list for the current limit.
    ListSessions {
        /// UI epoch.
        epoch: u64,
        /// Poll generation; responses from older generations are discarded.
        generation: u64,
        /// Page size.
        limit: u32,
    },
    /// Query Core metrics for one window.
    LoadCoreMetrics {
        /// UI epoch.
        epoch: u64,
        /// Page poll generation.
        generation: u64,
        /// Window to query.
        window: TimeWindow,
    },
    /// Query Agent metrics for one window.
    LoadAgentMetrics {
        /// UI epoch.
        epoch: u64,
        /// Page poll generation.
        generation: u64,
        /// Window to query.
        window: TimeWindow,
    },
    /// Query one request-log page.
    LoadRequestLog {
        /// UI epoch.
        epoch: u64,
        /// Page poll generation.
        generation: u64,
        /// Window to query.
        window: TimeWindow,
        /// Page, 0-based.
        page: usize,
    },
    /// Create a session from the validated create form.
    CreateSession {
        /// UI epoch.
        epoch: u64,
        /// Validated request body fields.
        spec: CreateSessionSpec,
        /// Fresh submission key.
        key: IdempotencyKey,
    },
    /// Fetch one session snapshot for the detail page.
    GetSession {
        /// UI epoch.
        epoch: u64,
        /// Detail poll generation.
        generation: u64,
        /// Session id.
        session_id: String,
    },
    /// Fetch one items page for the detail timeline.
    ListItems {
        /// UI epoch.
        epoch: u64,
        /// Detail poll generation.
        generation: u64,
        /// Session id.
        session_id: String,
        /// Page size.
        limit: u32,
        /// Opaque page cursor; `None` fetches the newest page.
        after: Option<String>,
    },
    /// Fetch one turns page for the detail page.
    ListTurns {
        /// UI epoch.
        epoch: u64,
        /// Detail poll generation.
        generation: u64,
        /// Session id.
        session_id: String,
        /// Page size.
        limit: u32,
        /// Opaque page cursor; `None` fetches the newest page.
        after: Option<String>,
    },
    /// Update an existing session from the edit form.
    UpdateSession {
        /// UI epoch.
        epoch: u64,
        /// Session id.
        session_id: String,
        /// Validated update fields.
        spec: UpdateSessionSpec,
        /// Submission key.
        key: IdempotencyKey,
    },
    /// Delete one session.
    DeleteSession {
        /// UI epoch.
        epoch: u64,
        /// Session id.
        session_id: String,
        /// Submission key.
        key: IdempotencyKey,
    },
    /// Submit a user message through the official events endpoint.
    SendInput {
        /// UI epoch.
        epoch: u64,
        /// Session id.
        session_id: String,
        /// Message text.
        text: String,
        /// Submission key; reused for a retry of the same text.
        key: IdempotencyKey,
    },
    /// Cancel the running turn.
    CancelTurn {
        /// UI epoch.
        epoch: u64,
        /// Session id.
        session_id: String,
        /// Submission key.
        key: IdempotencyKey,
    },
    /// Answer one pending MCP approval.
    AnswerApproval {
        /// UI epoch.
        epoch: u64,
        /// Session id.
        session_id: String,
        /// Opaque approval request id.
        request_id: String,
        /// Approve or deny.
        decision: ApprovalDecision,
        /// Submission key; reused for a retry of the same answer.
        key: IdempotencyKey,
    },
    /// Drop the worker client after sign out.
    DropClient,
    /// Leave the event loop.
    Quit,
}
