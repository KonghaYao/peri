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
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::api::{ApiError, HttpSwarmApi, SwarmApi};
use crate::config::{Config, ObservabilityConfig};
use crate::metrics;
use crate::observability::{ObservabilityError, OpenObserveClient};
use crate::ui::text;

mod protocol;
pub use protocol::*;

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
