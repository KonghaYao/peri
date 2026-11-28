//! Messages exchanged between the UI thread and the background worker.
//!
//! Split out of [`super`] so the worker module stays inside the repository size
//! budget; every type is still reachable as `worker::…` through the re-export.
//!
//! Commands carry the UI epoch; every result echoes it so responses that belong
//! to a signed-out or replaced connection can be discarded. Commands are not
//! `Debug`: they may contain the API key and must never be printed.

use std::time::{Duration, Instant};

use crate::api::{
    AgentMetrics, ApiError, ApprovalDecision, ApprovalReceipt, CreateSessionSpec, CreatedSession,
    Health, IdempotencyKey, InputReceipt, ItemsPage, Session, SessionPage, Turns,
    UpdateSessionSpec,
};
use crate::metrics::{CoreMetricsSnapshot, RequestLogPage, TimeWindow};
use crate::observability::{ObservabilityError, SearchPage};

/// Why credentials are being validated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationPurpose {
    /// First-run onboarding.
    Onboarding,
    /// Settings → edit credentials.
    Credentials,
    /// Settings → test connection; never saves.
    TestConnection,
    /// Error banner → retry after a 401 or transport failure.
    BannerRetry,
}

/// Which validation step failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationStage {
    /// `GET /healthz` failed.
    Health,
    /// The authenticated session-list call failed.
    Authentication,
}

/// A failed credential validation with the step that failed.
#[derive(Clone, Debug)]
pub struct ValidationFailure {
    /// Step that failed.
    pub stage: ValidationStage,
    /// Structured gateway error.
    pub error: ApiError,
}

/// Result of one health poll.
#[derive(Clone, Debug)]
pub struct HealthReport {
    /// UI epoch this poll belongs to.
    pub epoch: u64,
    /// Health result.
    pub result: Result<Health, ApiError>,
    /// Round-trip time of a successful poll.
    pub latency: Option<Duration>,
    /// When the poll completed.
    pub checked_at: Instant,
    /// Last contract header observed by the client.
    pub contract: Option<String>,
}

/// Which session mutation finished.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MutationKind {
    /// `POST /v1/agents/sessions/{id}`.
    Update,
    /// `DELETE /v1/agents/sessions/{id}`.
    Delete,
    /// `POST /v1/agents/sessions/{id}/events` with a message.
    SendInput,
    /// `POST /v1/agents/sessions/{id}/events` with cancel.
    CancelTurn,
    /// `POST /v1/agents/sessions/{id}/events` with an approval answer.
    Approval,
}

/// Typed payload of a finished session mutation.
#[derive(Clone, Debug)]
pub enum MutationData {
    /// The updated session snapshot.
    Updated(Box<Session>),
    /// The delete was accepted.
    Deleted,
    /// The message or cancel event was accepted.
    Input(Box<InputReceipt>),
    /// The approval answer was accepted.
    Approval(Box<ApprovalReceipt>),
}

/// A successful OpenObserve connection probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservabilityCheck {
    /// Round-trip time of the probe.
    pub elapsed: Duration,
    /// Stream count when OpenObserve reported it.
    pub streams: Option<usize>,
}

/// Result of a page request backed by OpenObserve.
///
/// A missing `[observability]` section is a normal state, not an error: the
/// page explains how to configure it and offers a Settings action.
#[derive(Clone, Debug)]
pub enum ObservabilityOutcome<T> {
    /// The query succeeded.
    Ready(T),
    /// No OpenObserve connection is configured.
    NotConfigured,
    /// The query failed; the error is safe to display.
    Failed(ObservabilityError),
}

/// Messages from the worker to the UI.
#[derive(Clone, Debug)]
pub enum WorkerEvent {
    /// One OpenObserve connection test finished.
    ObservabilityTested {
        /// UI epoch at request time.
        epoch: u64,
        /// Result of `GET /api/{org}/streams`.
        result: Result<ObservabilityCheck, ObservabilityError>,
    },
    /// One Core-metrics query finished.
    CoreMetricsLoaded {
        /// UI epoch at request time.
        epoch: u64,
        /// Page poll generation.
        generation: u64,
        /// Window that was requested.
        window: TimeWindow,
        /// OpenObserve result.
        result: ObservabilityOutcome<CoreMetricsSnapshot>,
    },
    /// One Agent-metrics request finished.
    AgentMetricsLoaded {
        /// UI epoch at request time.
        epoch: u64,
        /// Page poll generation.
        generation: u64,
        /// Window that was requested.
        window: TimeWindow,
        /// Result of `GET /v1/agents/metrics`.
        result: Result<AgentMetrics, ApiError>,
    },
    /// One request-log page finished.
    RequestLogLoaded {
        /// UI epoch at request time.
        epoch: u64,
        /// Page poll generation.
        generation: u64,
        /// Window that was requested.
        window: TimeWindow,
        /// Page that was requested, 0-based.
        page: usize,
        /// OpenObserve result.
        result: ObservabilityOutcome<RequestLogPage>,
    },
    /// One ad-hoc SQL query finished.
    QueryLoaded {
        /// UI epoch at request time.
        epoch: u64,
        /// Query generation.
        generation: u64,
        /// OpenObserve result.
        result: ObservabilityOutcome<SearchPage>,
    },
    /// One validation finished.
    ValidationFinished {
        /// UI epoch at request time.
        epoch: u64,
        /// What was being validated.
        purpose: ValidationPurpose,
        /// Elapsed time on success.
        result: Result<Duration, ValidationFailure>,
    },
    /// One health poll finished.
    HealthFinished(HealthReport),
    /// One session-list request finished.
    SessionsLoaded {
        /// UI epoch at request time.
        epoch: u64,
        /// Poll generation; stale responses are discarded by the UI.
        generation: u64,
        /// Page size that was requested.
        limit: u32,
        /// Result of `GET /v1/agents/sessions`.
        result: Result<SessionPage, ApiError>,
    },
    /// One create request finished.
    SessionCreated {
        /// UI epoch at request time.
        epoch: u64,
        /// Result of `POST /v1/agents/sessions`.
        result: Result<Box<CreatedSession>, ApiError>,
    },
    /// One detail snapshot request finished.
    SessionLoaded {
        /// UI epoch at request time.
        epoch: u64,
        /// Detail poll generation; stale responses are discarded by the UI.
        generation: u64,
        /// Session id that was requested.
        session_id: String,
        /// Result of `GET /v1/agents/sessions/{id}`.
        result: Result<Box<Session>, ApiError>,
    },
    /// One timeline page finished.
    ItemsLoaded {
        /// UI epoch at request time.
        epoch: u64,
        /// Detail poll generation.
        generation: u64,
        /// Session id that was requested.
        session_id: String,
        /// Page size that was requested.
        limit: u32,
        /// Cursor that was requested; `None` for the newest page.
        after: Option<String>,
        /// Result of `GET /v1/agents/sessions/{id}/items`.
        result: Result<Box<ItemsPage>, ApiError>,
    },
    /// One turns page finished.
    TurnsLoaded {
        /// UI epoch at request time.
        epoch: u64,
        /// Detail poll generation.
        generation: u64,
        /// Session id that was requested.
        session_id: String,
        /// Page size that was requested.
        limit: u32,
        /// Cursor that was requested; `None` for the newest page.
        after: Option<String>,
        /// Result of `GET /v1/agents/sessions/{id}/turns`.
        result: Result<Box<Turns>, ApiError>,
    },
    /// One session mutation finished.
    MutationFinished {
        /// UI epoch at request time.
        epoch: u64,
        /// Session id that was mutated.
        session_id: String,
        /// Which mutation was attempted.
        kind: MutationKind,
        /// Typed result.
        result: Result<MutationData, ApiError>,
    },
    /// The worker could not start.
    Fatal {
        /// Safe description without credentials.
        message: String,
    },
}

/// Commands from the UI to the worker. Deliberately not `Debug`.
pub enum Command {
    /// Probe candidate OpenObserve settings without saving them.
    TestObservability {
        /// UI epoch.
        epoch: u64,
        /// Candidate origin.
        base_url: String,
        /// Candidate organization.
        org: String,
        /// Candidate user. Never printed.
        user: String,
        /// Candidate password. Never printed.
        password: String,
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
    /// Run one validated read-only SQL statement for the Query page.
    RunQuery {
        /// UI epoch.
        epoch: u64,
        /// Query generation.
        generation: u64,
        /// Single `SELECT` statement, already validated by the UI.
        sql: String,
        /// Window start, Unix microseconds.
        start_us: u64,
        /// Window end, Unix microseconds.
        end_us: u64,
        /// Row offset.
        from: u64,
        /// Row count.
        size: u64,
    },
    /// Validate candidate credentials without adopting them.
    Validate {
        /// UI epoch.
        epoch: u64,
        /// Why the credentials are checked.
        purpose: ValidationPurpose,
        /// Candidate origin.
        base_url: String,
        /// Candidate key. Never printed.
        api_key: String,
    },
    /// Revalidate the active connection without changing credentials.
    Revalidate {
        /// UI epoch.
        epoch: u64,
        /// Why the check runs.
        purpose: ValidationPurpose,
    },
    /// Adopt credentials after the config file was written.
    Adopt {
        /// Validated origin.
        base_url: String,
        /// Validated key. Never printed.
        api_key: String,
    },
    /// Adopt an OpenObserve connection after Settings saved it.
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
    /// Run one health poll.
    Health {
        /// UI epoch.
        epoch: u64,
    },
    /// Fetch one page of sessions.
    ListSessions {
        /// UI epoch.
        epoch: u64,
        /// Poll generation the request belongs to.
        generation: u64,
        /// Page size.
        limit: u32,
    },
    /// Create one session.
    CreateSession {
        /// UI epoch.
        epoch: u64,
        /// Validated request body fields.
        spec: CreateSessionSpec,
        /// Submission-scoped idempotency key.
        key: IdempotencyKey,
    },
    /// Fetch one session snapshot.
    GetSession {
        /// UI epoch.
        epoch: u64,
        /// Detail poll generation.
        generation: u64,
        /// Session id.
        session_id: String,
    },
    /// Fetch one items page.
    ListItems {
        /// UI epoch.
        epoch: u64,
        /// Detail poll generation.
        generation: u64,
        /// Session id.
        session_id: String,
        /// Page size.
        limit: u32,
        /// Cursor; `None` fetches the newest page.
        after: Option<String>,
    },
    /// Fetch one turns page.
    ListTurns {
        /// UI epoch.
        epoch: u64,
        /// Detail poll generation.
        generation: u64,
        /// Session id.
        session_id: String,
        /// Page size.
        limit: u32,
        /// Cursor; `None` fetches the newest page.
        after: Option<String>,
    },
    /// Update one session.
    UpdateSession {
        /// UI epoch.
        epoch: u64,
        /// Session id.
        session_id: String,
        /// Validated update fields.
        spec: UpdateSessionSpec,
        /// Submission-scoped idempotency key.
        key: IdempotencyKey,
    },
    /// Delete one session.
    DeleteSession {
        /// UI epoch.
        epoch: u64,
        /// Session id.
        session_id: String,
        /// Submission-scoped idempotency key.
        key: IdempotencyKey,
    },
    /// Send one user message.
    SendInput {
        /// UI epoch.
        epoch: u64,
        /// Session id.
        session_id: String,
        /// Message text.
        text: String,
        /// Submission-scoped idempotency key.
        key: IdempotencyKey,
    },
    /// Cancel the running turn.
    CancelTurn {
        /// UI epoch.
        epoch: u64,
        /// Session id.
        session_id: String,
        /// Submission-scoped idempotency key.
        key: IdempotencyKey,
    },
    /// Answer one pending approval.
    AnswerApproval {
        /// UI epoch.
        epoch: u64,
        /// Session id.
        session_id: String,
        /// Opaque approval request id.
        request_id: String,
        /// Approve or deny.
        decision: ApprovalDecision,
        /// Submission-scoped idempotency key.
        key: IdempotencyKey,
    },
    /// Drop the active client (sign out).
    SignOut,
    /// Stop the worker loop.
    Shutdown,
}
