//! UDS registry: the MCP → gateway control channel.
//!
//! The socket carries newline-delimited JSON in both directions. The first
//! message on a connection is normally `register`; every later message for the
//! same session rebinds the connection's writer, so an MCP that reconnects
//! after a gateway restart is routable again on its next message.
//!
//! One writer queue per connection: gateway → MCP messages (`yield_request`,
//! `command`, `human_request`, `human_result`, `reclaimed`) are enqueued and
//! serialised by a dedicated task, so a slow reader never blocks the HTTP or
//! WebSocket handlers.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

use crate::error::GatewayError;
use crate::protocol::{GatewayMessage, HumanStatus, RegistryMessage};
use crate::session::SessionStore;

/// Pending gateway → MCP messages per connection before `send` reports a full
/// queue. The MCP drains this promptly; a full queue means it is not reading.
pub const REGISTRY_QUEUE: usize = 64;

/// Routes gateway → MCP messages to the connection that registered a session.
#[derive(Debug, Default)]
pub struct RegistryHandle {
    writers: RwLock<HashMap<String, mpsc::Sender<GatewayMessage>>>,
}

impl RegistryHandle {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes `writer` the destination for `session_id`.
    pub async fn bind(&self, session_id: &str, writer: mpsc::Sender<GatewayMessage>) {
        self.writers
            .write()
            .await
            .insert(session_id.to_owned(), writer);
    }

    /// Drops the destination for `session_id`, if any.
    pub async fn unbind(&self, session_id: &str) {
        self.writers.write().await.remove(session_id);
    }

    /// Drops the destination only when it is still `writer`.
    ///
    /// A session can move to a newer registry connection while the old one is
    /// still closing; the old connection must not remove its successor.
    pub async fn unbind_if(&self, session_id: &str, writer: &mpsc::Sender<GatewayMessage>) {
        let mut writers = self.writers.write().await;
        if writers
            .get(session_id)
            .is_some_and(|current| current.same_channel(writer))
        {
            writers.remove(session_id);
        }
    }

    /// Enqueues one message for `session_id`.
    ///
    /// # Errors
    ///
    /// Fails when the session has no live registry connection or its queue is
    /// full. Callers log and surface the failure; they never block on the MCP.
    pub async fn send(
        &self,
        session_id: &str,
        message: GatewayMessage,
    ) -> Result<(), GatewayError> {
        let writers = self.writers.read().await;
        let Some(writer) = writers.get(session_id) else {
            return Err(GatewayError::Internal(format!(
                "no registry connection for session {session_id}"
            )));
        };
        writer.try_send(message).map_err(|error| {
            GatewayError::Internal(format!(
                "registry queue for session {session_id} rejected a message: {error}"
            ))
        })
    }
}

/// Everything a registry connection needs.
#[derive(Clone)]
pub struct RegistryContext {
    pub sessions: Arc<SessionStore>,
    pub handle: Arc<RegistryHandle>,
    /// Used only for the human-readable session URL in the log.
    pub monitor_base: String,
}

/// Accepts registry connections forever.
///
/// The listener is created by the process entry (which also owns stale-socket
/// cleanup and permissions); this task only serves it.
pub async fn serve(listener: UnixListener, context: RegistryContext) {
    loop {
        match listener.accept().await {
            Ok((stream, _address)) => {
                let context = context.clone();
                tokio::spawn(async move {
                    serve_connection(stream, context).await;
                });
            }
            Err(error) => {
                warn!(error = %error, "registry accept failed");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

async fn serve_connection(stream: UnixStream, context: RegistryContext) {
    let (reader, writer) = stream.into_split();
    let (tx, rx) = mpsc::channel(REGISTRY_QUEUE);
    let writer_task = tokio::spawn(write_loop(writer, rx));

    let mut lines = BufReader::new(reader).lines();
    let mut bound: Vec<String> = Vec::new();

    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                warn!(error = %error, "registry read failed");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let message = match serde_json::from_str::<RegistryMessage>(&line) {
            Ok(RegistryMessage::Unknown) => {
                // `human_request` is the one frame the MCP sends with the
                // gateway vocabulary: it has the same shape the gateway would
                // use, and `RegistryMessage` does not repeat it (README §6.3).
                // Parse it here rather than adding a second vocabulary.
                match serde_json::from_str::<GatewayMessage>(&line) {
                    Ok(GatewayMessage::HumanRequest {
                        session_id,
                        request_id,
                        page_id,
                        prompt,
                        timeout_ms,
                    }) => {
                        handle_human_request(
                            &context, &tx, &mut bound, session_id, request_id, page_id, prompt,
                            timeout_ms,
                        )
                        .await;
                    }
                    _ => warn!("ignoring unknown registry message type"),
                }
                continue;
            }
            Ok(message) => message,
            Err(error) => {
                warn!(error = %error, "ignoring malformed registry line");
                continue;
            }
        };
        handle_message(message, &context, &tx, &mut bound).await;
    }

    for session_id in &bound {
        context.handle.unbind_if(session_id, &tx).await;
        info!(
            session = %session_id,
            "registry connection closed; session retained for takeover"
        );
    }
    drop(tx);
    let _ = writer_task.await;
}

async fn handle_message(
    message: RegistryMessage,
    context: &RegistryContext,
    writer: &mpsc::Sender<GatewayMessage>,
    bound: &mut Vec<String>,
) {
    match message {
        RegistryMessage::Register {
            session_id,
            name,
            cdp_url,
            pages,
            state,
        } => {
            let session = context
                .sessions
                .register(session_id.clone(), name, cdp_url, pages, state)
                .await;
            context.handle.bind(&session_id, writer.clone()).await;
            remember(bound, &session_id);
            info!(
                session = %session.id,
                name = %session.name,
                monitor = %context.monitor_base,
                pages = session.pages.len(),
                "session registered"
            );
        }
        RegistryMessage::UpdatePages { session_id, pages } => {
            match context.sessions.update_pages(&session_id, pages).await {
                Some(session) => {
                    context.handle.bind(&session_id, writer.clone()).await;
                    remember(bound, &session_id);
                    debug!(
                        session = %session_id,
                        pages = session.pages.len(),
                        "pages updated"
                    );
                }
                None => warn!(session = %session_id, "update_pages for an unregistered session"),
            }
        }
        RegistryMessage::State { session_id, state } => {
            if context
                .sessions
                .set_state(&session_id, state)
                .await
                .is_some()
            {
                context.handle.bind(&session_id, writer.clone()).await;
                remember(bound, &session_id);
            } else {
                warn!(session = %session_id, "state for an unregistered session");
            }
        }
        RegistryMessage::YieldOk {
            session_id,
            page_id,
        } => {
            context.handle.bind(&session_id, writer.clone()).await;
            remember(bound, &session_id);
            debug!(session = %session_id, page = %page_id, "yield acknowledged");
            // The driver opens the controller, broadcasts the interactive
            // `hello` and answers a late acknowledgement with `reclaimed`.
            if let Some(session) = context.sessions.get(&session_id).await {
                session.ownership.yield_ok(page_id);
            } else {
                warn!(session = %session_id, "yield_ok for an unregistered session");
            }
        }
        RegistryMessage::Reclaim {
            session_id,
            page_id,
        } => {
            context.handle.bind(&session_id, writer.clone()).await;
            remember(bound, &session_id);
            // Where a controller exists the driver closes it and replies
            // `reclaimed` itself; an unknown session has nothing to close, so
            // the MCP is told it may reconnect instead of waiting out
            // `reclaim_wait_ms`.
            match context.sessions.get(&session_id).await {
                Some(session) => session.ownership.reclaim(page_id),
                None => {
                    warn!(session = %session_id, "reclaim for an unregistered session");
                    let ack = GatewayMessage::Reclaimed {
                        session_id: session_id.clone(),
                        page_id,
                        ok: false,
                    };
                    if let Err(error) = context.handle.send(&session_id, ack).await {
                        warn!(session = %session_id, error = %error, "cannot answer reclaim");
                    }
                }
            }
        }
        RegistryMessage::CommandResult {
            session_id,
            request_id,
            ok,
            error,
        } => {
            context.handle.bind(&session_id, writer.clone()).await;
            remember(bound, &session_id);
            // No command is in flight before the CDP stage; keep the result
            // visible for debugging instead of dropping it silently.
            debug!(
                session = %session_id,
                request_id = %request_id,
                ok,
                error = ?error,
                "command result"
            );
        }
        RegistryMessage::Unregister { session_id } => {
            let removed = context.sessions.unregister(&session_id).await.is_some();
            context.handle.unbind(&session_id).await;
            bound.retain(|bound_id| bound_id != &session_id);
            info!(session = %session_id, removed, "session unregistered");
        }
        RegistryMessage::Unknown => {
            warn!("ignoring unknown registry message type");
        }
    }
}

/// Routes one `human_request` from the MCP to the session's ownership driver.
///
/// The driver broadcasts the banner, asks the MCP to yield the page and answers
/// with `human_result` (`done`, `declined` or `timeout`). A request for a
/// session this gateway does not know is answered with `timeout` immediately:
/// the MCP blocks on that answer, and leaving it to its own deadline would hide
/// the real reason (there is no monitor to ask).
#[allow(clippy::too_many_arguments)]
async fn handle_human_request(
    context: &RegistryContext,
    writer: &mpsc::Sender<GatewayMessage>,
    bound: &mut Vec<String>,
    session_id: String,
    request_id: String,
    page_id: String,
    prompt: String,
    timeout_ms: u64,
) {
    context.handle.bind(&session_id, writer.clone()).await;
    remember(bound, &session_id);
    match context.sessions.get(&session_id).await {
        Some(session) => {
            info!(
                session = %session_id,
                request_id = %request_id,
                page = %page_id,
                "human handoff requested by the MCP"
            );
            session
                .ownership
                .human_request(request_id, page_id, prompt, timeout_ms);
        }
        None => {
            warn!(
                session = %session_id,
                request_id = %request_id,
                "human_request for an unregistered session"
            );
            let result = GatewayMessage::HumanResult {
                session_id: session_id.clone(),
                request_id,
                status: HumanStatus::Timeout,
                url: String::new(),
                title: String::new(),
            };
            if let Err(error) = context.handle.send(&session_id, result).await {
                warn!(session = %session_id, error = %error, "cannot answer human_request");
            }
        }
    }
}

/// Records that this connection currently routes `session_id`.
///
/// One MCP connection may register several sessions; the list is the cleanup
/// set for disconnect and is intentionally not a single slot.
fn remember(bound: &mut Vec<String>, session_id: &str) {
    if !bound.iter().any(|bound_id| bound_id == session_id) {
        bound.push(session_id.to_owned());
    }
}

async fn write_loop(mut writer: OwnedWriteHalf, mut rx: mpsc::Receiver<GatewayMessage>) {
    while let Some(message) = rx.recv().await {
        let line = match encode_line(&message) {
            Ok(line) => line,
            Err(error) => {
                warn!(error = %error, "cannot encode gateway message");
                continue;
            }
        };
        if let Err(error) = writer.write_all(line.as_bytes()).await {
            debug!(error = %error, "registry writer closed");
            break;
        }
    }
    let _ = writer.shutdown().await;
}

/// Serialises one registry frame, including the trailing newline.
///
/// # Errors
///
/// Only fails if the message itself cannot be represented as JSON.
pub fn encode_line(message: &impl Serialize) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(message)?;
    line.push('\n');
    Ok(line)
}

#[cfg(test)]
#[path = "registry_test.rs"]
mod tests;
