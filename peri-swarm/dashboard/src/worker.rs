//! Background API worker.
//!
//! A dedicated OS thread owns a current-thread Tokio runtime and the
//! [`HttpSwarmApi`] client. The UI thread only exchanges owned messages, so a
//! slow or unreachable gateway never blocks input, rendering, or terminal
//! restoration.
//!
//! Commands carry the UI epoch; every result echoes it so responses that belong
//! to a signed-out or replaced connection can be discarded. Commands are not
//! `Debug`: they may contain the API key and must never be printed.

use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::api::{
    AgentMetrics, ApiError, ApprovalDecision, ApprovalReceipt, CreateSessionSpec, CreatedSession,
    Health, HttpSwarmApi, IdempotencyKey, InputReceipt, ItemsPage, Session, SessionPage, SwarmApi,
    Turns, UpdateSessionSpec,
};
use crate::config::{Config, ObservabilityConfig};
use crate::metrics::{self, CoreMetricsSnapshot, RequestLogPage, TimeWindow};
use crate::observability::{ObservabilityError, OpenObserveClient, SearchPage};
use crate::ui::text;

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

/// Handles for the UI thread.
pub struct WorkerHandle {
    commands: UnboundedSender<Command>,
    events: UnboundedReceiver<WorkerEvent>,
    _thread: Option<JoinHandle<()>>,
}

impl WorkerHandle {
    /// Start the worker, optionally with already-loaded credentials.
    #[must_use]
    pub fn spawn(initial: Option<Config>) -> Self {
        let (commands, command_rx) = unbounded_channel();
        let (event_tx, events) = unbounded_channel();
        let thread = std::thread::Builder::new()
            .name("peri-swarm-dashboard-api".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = event_tx.send(WorkerEvent::Fatal {
                            message: text::worker_start_failed(&error.to_string()),
                        });
                        return;
                    }
                };
                runtime.block_on(run(command_rx, event_tx, initial));
            })
            .ok();
        Self {
            commands,
            events,
            _thread: thread,
        }
    }

    /// Queue a command. A closed channel means the worker is gone; the UI keeps
    /// running and the health dot stays stale.
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// Drain one pending event without blocking.
    pub fn try_event(&mut self) -> Option<WorkerEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}

async fn run(
    mut commands: UnboundedReceiver<Command>,
    events: UnboundedSender<WorkerEvent>,
    initial: Option<Config>,
) {
    let mut active = initial.clone();
    let mut observability = initial.as_ref().and_then(Config::observability).cloned();
    let mut client = initial
        .as_ref()
        .and_then(|config| HttpSwarmApi::from_config(config).ok());

    while let Some(command) = commands.recv().await {
        match command {
            Command::TestObservability {
                epoch,
                base_url,
                org,
                user,
                password,
            } => {
                let started = Instant::now();
                let result = match OpenObserveClient::new(&base_url, &org, &user, &password) {
                    Ok(client) => {
                        client
                            .test_connection()
                            .await
                            .map(|streams| ObservabilityCheck {
                                elapsed: started.elapsed(),
                                streams,
                            })
                    }
                    Err(error) => Err(error),
                };
                if events
                    .send(WorkerEvent::ObservabilityTested { epoch, result })
                    .is_err()
                {
                    break;
                }
            }
            Command::LoadCoreMetrics {
                epoch,
                generation,
                window,
            } => {
                let result = match observability_client(observability.as_ref()) {
                    Ok(Some(client)) => {
                        match metrics::load_core_metrics(&client, window, unix_micros_now()).await {
                            Ok(snapshot) => ObservabilityOutcome::Ready(snapshot),
                            Err(error) => ObservabilityOutcome::Failed(error),
                        }
                    }
                    Ok(None) => ObservabilityOutcome::NotConfigured,
                    Err(error) => ObservabilityOutcome::Failed(error),
                };
                if events
                    .send(WorkerEvent::CoreMetricsLoaded {
                        epoch,
                        generation,
                        window,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Command::LoadAgentMetrics {
                epoch,
                generation,
                window,
            } => {
                let result = match client.as_ref() {
                    Some(client) => {
                        let end_us = i64::try_from(unix_micros_now()).unwrap_or(i64::MAX);
                        let start_us = end_us
                            .saturating_sub(i64::try_from(window.micros()).unwrap_or(i64::MAX));
                        client
                            .agent_metrics(start_us, end_us, metrics::AGENT_METRICS_LIMIT)
                            .await
                    }
                    None => Err(ApiError::local(text::WORKER_CLIENT_UNAVAILABLE)),
                };
                if events
                    .send(WorkerEvent::AgentMetricsLoaded {
                        epoch,
                        generation,
                        window,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Command::LoadRequestLog {
                epoch,
                generation,
                window,
                page,
            } => {
                let result = match observability_client(observability.as_ref()) {
                    Ok(Some(client)) => {
                        match metrics::load_request_log(&client, window, page, unix_micros_now())
                            .await
                        {
                            Ok(loaded) => ObservabilityOutcome::Ready(loaded),
                            Err(error) => ObservabilityOutcome::Failed(error),
                        }
                    }
                    Ok(None) => ObservabilityOutcome::NotConfigured,
                    Err(error) => ObservabilityOutcome::Failed(error),
                };
                if events
                    .send(WorkerEvent::RequestLogLoaded {
                        epoch,
                        generation,
                        window,
                        page,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Command::RunQuery {
                epoch,
                generation,
                sql,
                start_us,
                end_us,
                from,
                size,
            } => {
                let result = match observability_client(observability.as_ref()) {
                    Ok(Some(client)) => {
                        match client.search(&sql, start_us, end_us, from, size).await {
                            Ok(page) => ObservabilityOutcome::Ready(page),
                            Err(error) => ObservabilityOutcome::Failed(error),
                        }
                    }
                    Ok(None) => ObservabilityOutcome::NotConfigured,
                    Err(error) => ObservabilityOutcome::Failed(error),
                };
                if events
                    .send(WorkerEvent::QueryLoaded {
                        epoch,
                        generation,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Command::Validate {
                epoch,
                purpose,
                base_url,
                api_key,
            } => {
                let started = Instant::now();
                let result = validate_new(&base_url, &api_key)
                    .await
                    .map(|()| started.elapsed());
                if events
                    .send(WorkerEvent::ValidationFinished {
                        epoch,
                        purpose,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Command::Revalidate { epoch, purpose } => {
                let started = Instant::now();
                let result = match (&active, &client) {
                    (Some(_), Some(client)) => {
                        validate_client(client).await.map(|()| started.elapsed())
                    }
                    _ => Err(ValidationFailure {
                        stage: ValidationStage::Authentication,
                        error: ApiError::local(text::NO_ACTIVE_CONNECTION),
                    }),
                };
                if events
                    .send(WorkerEvent::ValidationFinished {
                        epoch,
                        purpose,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Command::Adopt { base_url, api_key } => {
                // Keep the OpenObserve section the worker already adopted.
                let preserved = observability.clone();
                if let Ok(config) = Config::new(&base_url, &api_key) {
                    let config = match preserved {
                        Some(observability) => config.with_observability(observability),
                        None => config,
                    };
                    client = HttpSwarmApi::from_config(&config).ok();
                    active = Some(config);
                } else {
                    client = None;
                    active = None;
                }
            }
            Command::AdoptObservability {
                base_url,
                org,
                user,
                password,
            } => {
                if let Ok(adopted) = ObservabilityConfig::new(&base_url, &org, &user, &password) {
                    observability = Some(adopted.clone());
                    if let Some(config) = active.take() {
                        active = Some(config.with_observability(adopted));
                    }
                }
            }
            Command::Health { epoch } => {
                let started = Instant::now();
                let result = match client.as_ref() {
                    Some(client) => client.health().await,
                    None => Err(ApiError::local(text::WORKER_CLIENT_UNAVAILABLE)),
                };
                let latency = result.as_ref().ok().map(|_| started.elapsed());
                let contract = client.as_ref().and_then(|client| client.contract_version());
                if events
                    .send(WorkerEvent::HealthFinished(HealthReport {
                        epoch,
                        result,
                        latency,
                        checked_at: Instant::now(),
                        contract,
                    }))
                    .is_err()
                {
                    break;
                }
            }
            Command::ListSessions {
                epoch,
                generation,
                limit,
            } => {
                let result = match client.as_ref() {
                    Some(client) => client.list_sessions(limit).await,
                    None => Err(ApiError::local(text::WORKER_CLIENT_UNAVAILABLE)),
                };
                if events
                    .send(WorkerEvent::SessionsLoaded {
                        epoch,
                        generation,
                        limit,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Command::CreateSession { epoch, spec, key } => {
                let result = match client.as_ref() {
                    Some(client) => client.create_session(&spec, &key).await.map(Box::new),
                    None => Err(ApiError::local(text::WORKER_CLIENT_UNAVAILABLE)),
                };
                if events
                    .send(WorkerEvent::SessionCreated { epoch, result })
                    .is_err()
                {
                    break;
                }
            }
            Command::GetSession {
                epoch,
                generation,
                session_id,
            } => {
                let result = match client.as_ref() {
                    Some(client) => client.get_session(&session_id).await.map(Box::new),
                    None => Err(ApiError::local(text::WORKER_CLIENT_UNAVAILABLE)),
                };
                if events
                    .send(WorkerEvent::SessionLoaded {
                        epoch,
                        generation,
                        session_id,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Command::ListItems {
                epoch,
                generation,
                session_id,
                limit,
                after,
            } => {
                let result = match client.as_ref() {
                    Some(client) => client
                        .list_items(&session_id, limit, after.as_deref())
                        .await
                        .map(Box::new),
                    None => Err(ApiError::local(text::WORKER_CLIENT_UNAVAILABLE)),
                };
                if events
                    .send(WorkerEvent::ItemsLoaded {
                        epoch,
                        generation,
                        session_id,
                        limit,
                        after,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Command::ListTurns {
                epoch,
                generation,
                session_id,
                limit,
                after,
            } => {
                let result = match client.as_ref() {
                    Some(client) => client
                        .list_turns(&session_id, limit, after.as_deref())
                        .await
                        .map(Box::new),
                    None => Err(ApiError::local(text::WORKER_CLIENT_UNAVAILABLE)),
                };
                if events
                    .send(WorkerEvent::TurnsLoaded {
                        epoch,
                        generation,
                        session_id,
                        limit,
                        after,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Command::UpdateSession {
                epoch,
                session_id,
                spec,
                key,
            } => {
                let result = match client.as_ref() {
                    Some(client) => client
                        .update_session(&session_id, &spec, &key)
                        .await
                        .map(|session| MutationData::Updated(Box::new(session))),
                    None => Err(ApiError::local(text::WORKER_CLIENT_UNAVAILABLE)),
                };
                let finished = WorkerEvent::MutationFinished {
                    epoch,
                    session_id,
                    kind: MutationKind::Update,
                    result,
                };
                if events.send(finished).is_err() {
                    break;
                }
            }
            Command::DeleteSession {
                epoch,
                session_id,
                key,
            } => {
                let result = match client.as_ref() {
                    Some(client) => client
                        .delete_session(&session_id, &key)
                        .await
                        .map(|()| MutationData::Deleted),
                    None => Err(ApiError::local(text::WORKER_CLIENT_UNAVAILABLE)),
                };
                let finished = WorkerEvent::MutationFinished {
                    epoch,
                    session_id,
                    kind: MutationKind::Delete,
                    result,
                };
                if events.send(finished).is_err() {
                    break;
                }
            }
            Command::SendInput {
                epoch,
                session_id,
                text: message,
                key,
            } => {
                let result = match client.as_ref() {
                    Some(client) => client
                        .send_input(&session_id, &message, &key)
                        .await
                        .map(|receipt| MutationData::Input(Box::new(receipt))),
                    None => Err(ApiError::local(text::WORKER_CLIENT_UNAVAILABLE)),
                };
                let finished = WorkerEvent::MutationFinished {
                    epoch,
                    session_id,
                    kind: MutationKind::SendInput,
                    result,
                };
                if events.send(finished).is_err() {
                    break;
                }
            }
            Command::CancelTurn {
                epoch,
                session_id,
                key,
            } => {
                let result = match client.as_ref() {
                    Some(client) => client
                        .cancel(&session_id, &key)
                        .await
                        .map(|receipt| MutationData::Input(Box::new(receipt))),
                    None => Err(ApiError::local(text::WORKER_CLIENT_UNAVAILABLE)),
                };
                let finished = WorkerEvent::MutationFinished {
                    epoch,
                    session_id,
                    kind: MutationKind::CancelTurn,
                    result,
                };
                if events.send(finished).is_err() {
                    break;
                }
            }
            Command::AnswerApproval {
                epoch,
                session_id,
                request_id,
                decision,
                key,
            } => {
                let result = match client.as_ref() {
                    Some(client) => client
                        .approve(&session_id, &request_id, decision, &key)
                        .await
                        .map(|receipt| MutationData::Approval(Box::new(receipt))),
                    None => Err(ApiError::local(text::WORKER_CLIENT_UNAVAILABLE)),
                };
                let finished = WorkerEvent::MutationFinished {
                    epoch,
                    session_id,
                    kind: MutationKind::Approval,
                    result,
                };
                if events.send(finished).is_err() {
                    break;
                }
            }
            Command::SignOut => {
                active = None;
                client = None;
                observability = None;
            }
            Command::Shutdown => break,
        }
    }
}

/// Current Unix time in microseconds.
fn unix_micros_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Build an OpenObserve client from the adopted connection.
///
/// `Ok(None)` means no connection was configured; a client construction
/// failure is reported rather than hidden.
fn observability_client(
    observability: Option<&ObservabilityConfig>,
) -> Result<Option<OpenObserveClient>, ObservabilityError> {
    let Some(observability) = observability else {
        return Ok(None);
    };
    OpenObserveClient::from_config(observability).map(Some)
}

async fn validate_new(base_url: &str, api_key: &str) -> Result<(), ValidationFailure> {
    let client = HttpSwarmApi::new(base_url, api_key).map_err(|error| ValidationFailure {
        stage: ValidationStage::Health,
        error,
    })?;
    validate_client(&client).await
}

async fn validate_client(client: &HttpSwarmApi) -> Result<(), ValidationFailure> {
    client.health().await.map_err(|error| ValidationFailure {
        stage: ValidationStage::Health,
        error,
    })?;
    client
        .list_sessions(1)
        .await
        .map_err(|error| ValidationFailure {
            stage: ValidationStage::Authentication,
            error,
        })?;
    Ok(())
}

#[cfg(test)]
#[path = "worker_test.rs"]
mod tests;
