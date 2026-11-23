//! Registry client for the optional monitor gateway.
//!
//! The MCP registers its session and pages over a Unix-domain socket using
//! newline-delimited JSON. The connection is best-effort: without a gateway the
//! browser tools keep working and `browser_status` reports the monitor as
//! unavailable (README §6.1, §8).
//!
//! Three message groups share the tag `type`:
//!
//! - registry messages the MCP sends (register, page updates, state, yield
//!   acknowledgements, reclaims, command results, unregister);
//! - `human_request`, the one gateway-addressed message the MCP sends on the
//!   same socket (the gateway routes it into its ownership driver);
//! - gateway-addressed messages the MCP receives (`yield_request`, `command`,
//!   `human_result`, `reclaimed`).
//!
//! The gateway crate owns the state machine. This module owns only the wire
//! shape and a reconnect loop, so a gateway restart is invisible until the next
//! message. Unknown tags are logged and ignored; a known tag with a malformed
//! payload is an error.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, warn};

use crate::config::BrowserConfig;

/// Delay before a failed registry connection is retried.
const RECONNECT_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// Agent lifecycle reported to the monitor.
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

/// Why the gateway asks the MCP to give up its page controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum YieldReason {
    Takeover,
    Idle,
    RequestHuman,
}

impl YieldReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Takeover => "takeover",
            Self::Idle => "idle",
            Self::RequestHuman => "request_human",
        }
    }
}

/// Outcome of a human handoff reported by the gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanStatus {
    Done,
    Declined,
    Timeout,
}

/// Top-level page owned by one MCP session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageRef {
    pub target_id: String,
    pub url: String,
    pub title: String,
}

/// Viewer-requested operation the MCP executes while it drives the page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum BrowserCommand {
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
    #[serde(other)]
    Unknown,
}

/// MCP → gateway frames.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Outgoing {
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
    HumanRequest {
        session_id: String,
        request_id: String,
        page_id: String,
        prompt: String,
        timeout_ms: u64,
    },
}

/// Gateway → MCP frames. Only the fields this server acts on are modelled;
/// serde ignores the rest.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Incoming {
    YieldRequest {
        page_id: String,
        reason: YieldReason,
        timeout_ms: u64,
    },
    Command {
        request_id: String,
        page_id: String,
        command: BrowserCommand,
    },
    HumanResult {
        request_id: String,
        status: HumanStatus,
        url: String,
        title: String,
    },
    Reclaimed {
        page_id: String,
        ok: bool,
    },
    #[serde(other)]
    Unknown,
}

struct Inner {
    sock: Option<PathBuf>,
    cdp_url: Option<String>,
    session_id: String,
    name: String,
    outbox: mpsc::UnboundedSender<Outgoing>,
    outbox_rx: Mutex<Option<mpsc::UnboundedReceiver<Outgoing>>>,
    inbox: mpsc::UnboundedSender<Incoming>,
    started: AtomicBool,
    connected: AtomicBool,
    generation: AtomicU64,
    state: StdMutex<AgentState>,
    pages: StdMutex<Vec<PageRef>>,
}

/// Cloneable registry handle. Every method is non-blocking: messages are queued
/// and dropped only when the process exits.
#[derive(Clone)]
pub struct GatewayClient {
    inner: Arc<Inner>,
}

impl GatewayClient {
    /// Builds a client for `config`. Call [`Self::start`] to spawn the writer.
    #[must_use]
    pub fn new(
        config: &BrowserConfig,
        session_id: &str,
        inbox: mpsc::UnboundedSender<Incoming>,
    ) -> Self {
        let (outbox, outbox_rx) = mpsc::unbounded_channel();
        Self {
            inner: Arc::new(Inner {
                sock: config.gateway_sock.clone(),
                cdp_url: config.cdp_url.clone(),
                session_id: session_id.to_owned(),
                name: config.session_name.clone(),
                outbox,
                outbox_rx: Mutex::new(Some(outbox_rx)),
                inbox,
                started: AtomicBool::new(false),
                connected: AtomicBool::new(false),
                generation: AtomicU64::new(0),
                state: StdMutex::new(AgentState::Idle),
                pages: StdMutex::new(Vec::new()),
            }),
        }
    }

    /// Starts the reconnect loop once. Without a socket and a CDP endpoint
    /// there is nothing to register, so the task is not created.
    pub fn start(&self) {
        if !self.is_configured() {
            return;
        }
        if self
            .inner
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let receiver = inner.outbox_rx.lock().await.take();
            if let Some(receiver) = receiver {
                run(inner, receiver).await;
            }
        });
    }

    /// The monitor can be reached at all (socket and endpoint configured).
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.inner.sock.is_some() && self.inner.cdp_url.is_some()
    }

    /// The registry socket currently has a live connection.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.inner.connected.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.inner.session_id
    }

    #[must_use]
    pub fn sock_path(&self) -> Option<&Path> {
        self.inner.sock.as_deref()
    }

    /// Replaces the page list shown to the monitor and tells it immediately.
    pub fn update_pages(&self, pages: Vec<PageRef>) {
        if let Ok(mut current) = self.inner.pages.lock() {
            *current = pages.clone();
        }
        self.send(Outgoing::UpdatePages {
            session_id: self.inner.session_id.clone(),
            pages,
        });
    }

    /// Reports the agent lifecycle. The value is also replayed on reconnect.
    pub fn update_state(&self, state: AgentState) {
        if let Ok(mut current) = self.inner.state.lock() {
            *current = state;
        }
        self.send(Outgoing::State {
            session_id: self.inner.session_id.clone(),
            state,
        });
    }

    /// Acknowledges a yield after the page connection has been closed.
    pub fn yield_ok(&self, page_id: &str) {
        self.send(Outgoing::YieldOk {
            session_id: self.inner.session_id.clone(),
            page_id: page_id.to_owned(),
        });
    }

    /// Asks the monitor to give the page back. Sent before reconnecting.
    pub fn reclaim(&self, page_id: &str) {
        self.send(Outgoing::Reclaim {
            session_id: self.inner.session_id.clone(),
            page_id: page_id.to_owned(),
        });
    }

    /// Answers a viewer command.
    pub fn command_result(&self, request_id: &str, ok: bool, error: Option<String>) {
        self.send(Outgoing::CommandResult {
            session_id: self.inner.session_id.clone(),
            request_id: request_id.to_owned(),
            ok,
            error,
        });
    }

    /// Starts a human handoff; the gateway answers with `human_result`.
    pub fn human_request(&self, request_id: &str, page_id: &str, prompt: &str, timeout_ms: u64) {
        self.send(Outgoing::HumanRequest {
            session_id: self.inner.session_id.clone(),
            request_id: request_id.to_owned(),
            page_id: page_id.to_owned(),
            prompt: prompt.to_owned(),
            timeout_ms,
        });
    }

    /// Leaves the session table on shutdown.
    pub fn unregister(&self) {
        self.send(Outgoing::Unregister {
            session_id: self.inner.session_id.clone(),
        });
    }

    fn send(&self, message: Outgoing) {
        if self.inner.sock.is_none() {
            return;
        }
        if self.inner.outbox.send(message).is_err() {
            debug!("gateway writer stopped; message dropped");
        }
    }
}

async fn run(inner: Arc<Inner>, mut receiver: mpsc::UnboundedReceiver<Outgoing>) {
    let Some(sock) = inner.sock.clone() else {
        return;
    };
    loop {
        match UnixStream::connect(&sock).await {
            Ok(stream) => {
                let (read, mut write) = stream.into_split();
                let generation = inner.generation.fetch_add(1, Ordering::AcqRel) + 1;
                if write_all(&mut write, &register(&inner)).await.is_err() {
                    inner.connected.store(false, Ordering::Release);
                    retry_delay().await;
                    continue;
                }
                inner.connected.store(true, Ordering::Release);
                debug!(sock = %sock.display(), "registered with the monitor gateway");
                let reader = tokio::spawn(read_loop(Arc::clone(&inner), read, generation));
                loop {
                    let Some(message) = receiver.recv().await else {
                        reader.abort();
                        return;
                    };
                    let line = match serde_json::to_string(&message) {
                        Ok(line) => line,
                        Err(error) => {
                            warn!(error = %error, "cannot serialize a gateway message");
                            continue;
                        }
                    };
                    if let Err(error) = write_all(&mut write, &line).await {
                        debug!(error = %error, "gateway write failed; reconnecting");
                        // Keep the message for the next connection instead of
                        // dropping a state change or a reclaim.
                        let _ = inner.outbox.send(message);
                        break;
                    }
                }
                reader.abort();
                if inner.generation.load(Ordering::Acquire) == generation {
                    inner.connected.store(false, Ordering::Release);
                }
            }
            Err(error) => {
                inner.connected.store(false, Ordering::Release);
                debug!(sock = %sock.display(), error = %error, "monitor gateway is not listening");
            }
        }
        retry_delay().await;
    }
}

async fn retry_delay() {
    tokio::time::sleep(RECONNECT_DELAY).await;
}

async fn write_all(write: &mut OwnedWriteHalf, line: &str) -> std::io::Result<()> {
    write.write_all(line.as_bytes()).await?;
    write.write_all(b"\n").await?;
    write.flush().await
}

async fn read_loop(inner: Arc<Inner>, read: OwnedReadHalf, generation: u64) {
    let mut lines = BufReader::new(read).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Incoming>(&line) {
                    Ok(Incoming::Unknown) => {
                        debug!("ignoring an unknown gateway message");
                    }
                    Ok(message) => {
                        if inner.inbox.send(message).is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        warn!(error = %error, "malformed gateway message");
                    }
                }
            }
            Ok(None) => break,
            Err(error) => {
                debug!(error = %error, "gateway connection read failed");
                break;
            }
        }
    }
    if inner.generation.load(Ordering::Acquire) == generation {
        inner.connected.store(false, Ordering::Release);
    }
}

fn register(inner: &Inner) -> String {
    let pages = inner
        .pages
        .lock()
        .map(|pages| pages.clone())
        .unwrap_or_default();
    let state = inner.state.lock().map(|state| *state).unwrap_or_default();
    let message = Outgoing::Register {
        session_id: inner.session_id.clone(),
        name: inner.name.clone(),
        cdp_url: inner.cdp_url.clone().unwrap_or_default(),
        pages,
        state,
    };
    serde_json::to_string(&message).unwrap_or_else(|_| "{}".to_owned())
}

#[cfg(test)]
#[path = "gateway_test.rs"]
mod tests;
