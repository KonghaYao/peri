//! In-process fake transport and driver for the v2 adapter tests.
//!
//! The adapter is constructed with `Arc<dyn AcpTransport>` so tests can drive it
//! exactly as the host does — `recv` for inbound requests, `send_response` for
//! their results, `send_notification` for host events, and
//! `send_cancellable_request` for broker-initiated permission/elicitation
//! requests — without any process or stdio plumbing.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, Mutex, Notify};
use tokio_util::sync::CancellationToken;

use crate::transport::types::{AcpError, IncomingMessage, RequestId};
use crate::transport::AcpTransport;

use super::V2Stdio;

/// How long a test waits for the adapter to react before failing.
const WAIT: Duration = Duration::from_secs(2);
/// How long a "nothing must happen" assertion observes.
const QUIET: Duration = Duration::from_millis(80);

#[derive(Debug)]
pub(super) enum Out {
    Notification {
        method: String,
        params: Value,
    },
    Response {
        id: RequestId,
        result: Result<Value, AcpError>,
    },
}

impl Out {
    pub(super) fn notification(self) -> (String, Value) {
        match self {
            Self::Notification { method, params } => (method, params),
            other => panic!("expected a notification, got {other:?}"),
        }
    }

    pub(super) fn response(self) -> (RequestId, Result<Value, AcpError>) {
        match self {
            Self::Response { id, result } => (id, result),
            other => panic!("expected a response, got {other:?}"),
        }
    }

    pub(super) fn update(self) -> Value {
        let (method, params) = self.notification();
        assert_eq!(method, "session/update");
        params["update"].clone()
    }
}

pub(super) struct PeerCall {
    pub method: String,
    pub params: Value,
    reply: oneshot::Sender<Result<Value, AcpError>>,
}

impl PeerCall {
    pub(super) fn respond(self, result: Result<Value, AcpError>) {
        let _ = self.reply.send(result);
    }
}

#[derive(Default)]
pub(super) struct Fake {
    inbound: Mutex<VecDeque<IncomingMessage>>,
    inbound_wake: Notify,
    out: Mutex<VecDeque<Out>>,
    peers: Mutex<VecDeque<PeerCall>>,
    peer_wake: Notify,
}

impl Fake {
    async fn push(&self, message: IncomingMessage) {
        self.inbound.lock().await.push_back(message);
        self.inbound_wake.notify_one();
    }

    pub(super) async fn next_peer(&self) -> PeerCall {
        tokio::time::timeout(WAIT, async {
            loop {
                if let Some(call) = self.peers.lock().await.pop_front() {
                    return call;
                }
                self.peer_wake.notified().await;
            }
        })
        .await
        .expect("adapter sent no peer request")
    }

    pub(super) async fn peer_count(&self) -> usize {
        self.peers.lock().await.len()
    }

    pub(super) async fn out_count(&self) -> usize {
        self.out.lock().await.len()
    }

    async fn take_out(&self) -> Option<Out> {
        self.out.lock().await.pop_front()
    }
}

#[async_trait]
impl AcpTransport for Fake {
    fn allows_concurrent_sessions(&self) -> bool {
        true
    }

    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.send_cancellable_request(method, params, CancellationToken::new())
            .await
    }

    async fn send_cancellable_request(
        &self,
        method: &str,
        params: Value,
        _token: CancellationToken,
    ) -> Result<Value, AcpError> {
        let (reply, response) = oneshot::channel();
        self.peers.lock().await.push_back(PeerCall {
            method: method.to_owned(),
            params,
            reply,
        });
        self.peer_wake.notify_one();
        response
            .await
            .unwrap_or_else(|_| Err(AcpError::new(-32603, "test peer dropped")))
    }

    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        self.out.lock().await.push_back(Out::Notification {
            method: method.to_owned(),
            params,
        });
        Ok(())
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        loop {
            if let Some(message) = self.inbound.lock().await.pop_front() {
                return Some(message);
            }
            self.inbound_wake.notified().await;
        }
    }

    async fn send_response(
        &self,
        id: RequestId,
        result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        self.out
            .lock()
            .await
            .push_back(Out::Response { id, result });
        Ok(())
    }
}

/// Drives one adapter the way the host would.
pub(super) struct Harness {
    v2: Arc<V2Stdio>,
    fake: Arc<Fake>,
    forwarded: mpsc::UnboundedReceiver<IncomingMessage>,
    next_id: i64,
}

impl Harness {
    pub(super) fn start() -> Self {
        let fake = Arc::new(Fake::default());
        let permissions = SharedPermissionMode::new(PermissionMode::Default);
        let v2 = Arc::new(V2Stdio::new(
            Arc::clone(&fake) as Arc<dyn AcpTransport>,
            permissions,
        ));
        let (tx, forwarded) = mpsc::unbounded_channel();
        let pump = Arc::clone(&v2);
        tokio::spawn(async move {
            while let Some(message) = pump.recv().await {
                if tx.send(message).is_err() {
                    break;
                }
            }
        });
        Self {
            v2,
            fake,
            forwarded,
            next_id: 0,
        }
    }

    fn id(&mut self) -> RequestId {
        self.next_id += 1;
        RequestId::Number(self.next_id)
    }

    pub(super) async fn negotiate(&mut self) {
        self.negotiate_with(json!({})).await;
    }

    /// Complete `initialize`, optionally declaring client capabilities.
    pub(super) async fn negotiate_with(&mut self, capabilities: Value) {
        let id = self.id();
        let mut params = json!({
            "protocolVersion": 2,
            "info": {"name": "probe", "version": "1"}
        });
        if capabilities != json!({}) {
            params["clientCapabilities"] = capabilities;
        }
        self.request(id.clone(), "initialize", params).await;
        let forwarded = self.expect_forwarded().await;
        assert_eq!(forwarded.method, "initialize");
        self.respond(forwarded.id.clone(), Ok(json!({"protocolVersion": 1})))
            .await;
        let (answered, result) = self.expect_out().await.response();
        assert_eq!(answered, id);
        assert_eq!(result.expect("initialize result")["protocolVersion"], 2);
    }

    pub(super) async fn request(&self, id: RequestId, method: &str, params: Value) {
        self.fake
            .push(IncomingMessage::Request {
                id,
                method: method.into(),
                params,
            })
            .await;
    }

    pub(super) async fn notification(&self, method: &str, params: Value) {
        self.fake
            .push(IncomingMessage::Notification {
                method: method.into(),
                params,
            })
            .await;
    }

    pub(super) async fn expect_forwarded(&mut self) -> Forwarded {
        let message = tokio::time::timeout(WAIT, self.forwarded.recv())
            .await
            .expect("adapter forwarded nothing")
            .expect("adapter pump stopped");
        match message {
            IncomingMessage::Request { id, method, params } => Forwarded { id, method, params },
            other => panic!("expected a forwarded request, got {other:?}"),
        }
    }

    pub(super) async fn expect_out(&self) -> Out {
        tokio::time::timeout(WAIT, async {
            loop {
                if let Some(out) = self.fake.take_out().await {
                    return out;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("adapter produced no output")
    }

    /// Assert the adapter produced nothing and forwarded nothing.
    pub(super) async fn expect_quiet(&mut self) {
        tokio::time::sleep(QUIET).await;
        if let Some(out) = self.fake.take_out().await {
            panic!("unexpected adapter output: {out:?}");
        }
        if let Ok(message) = self.forwarded.try_recv() {
            panic!("unexpected forwarded request: {message:?}");
        }
    }

    pub(super) async fn respond(&self, id: RequestId, result: Result<Value, AcpError>) {
        self.v2
            .send_response(id, result)
            .await
            .expect("adapter response write");
    }

    pub(super) async fn send_update(&self, sid: &str, update: Value) {
        self.v2
            .send_notification(
                "session/update",
                json!({"sessionId": sid, "update": update}),
            )
            .await
            .expect("update write");
    }

    pub(super) async fn send_event(&self, sid: &str, event: Value) {
        self.v2
            .send_notification(
                "peri/agent_event",
                json!({"sessionId": sid, "event_json": event.to_string()}),
            )
            .await
            .expect("event write");
    }

    pub(super) async fn send_done(&self, sid: &str, stop_reason: &str) {
        self.v2
            .send_notification(
                "peri/agent_event_done",
                json!({"sessionId": sid, "stopReason": stop_reason}),
            )
            .await
            .expect("done write");
    }

    /// Create a session as the client would: `session/new` + accepted reply.
    pub(super) async fn open(&mut self, sid: &str) {
        let id = self.id();
        self.request(id.clone(), "session/new", json!({"cwd": "/workspace"}))
            .await;
        let forwarded = self.expect_forwarded().await;
        assert_eq!(forwarded.method, "session/new");
        self.respond(
            forwarded.id,
            Ok(json!({"sessionId": sid, "modes": {}, "configOptions": []})),
        )
        .await;
        let (answered, result) = self.expect_out().await.response();
        assert_eq!(answered, id);
        assert_eq!(result.expect("new result")["sessionId"], sid);
    }

    /// Submit a prompt that the host has already queued (internal input Ok).
    /// Submit a prompt the host queued successfully (internal input replied Ok).
    ///
    /// The client's prompt request stays open until committed delivery.
    pub(super) async fn enqueue_prompt(&mut self, sid: &str, text: &str) -> Submission {
        let (request, forwarded) = self.send_prompt(sid, text).await;
        self.respond(forwarded.id.clone(), Ok(json!({}))).await;
        self.expect_quiet().await;
        Submission {
            request,
            input: forwarded.params["inputId"]
                .as_str()
                .expect("input id")
                .to_owned(),
        }
    }

    /// A prompt the host has not answered yet (no committed delivery).
    pub(super) async fn send_prompt(&mut self, sid: &str, text: &str) -> (RequestId, Forwarded) {
        let id = self.id();
        self.request(
            id.clone(),
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type": "text", "text": text}]}),
        )
        .await;
        let forwarded = self.expect_forwarded().await;
        assert_eq!(forwarded.method, super::INPUT);
        assert_eq!(forwarded.params["sessionId"], sid);
        (id, forwarded)
    }

    /// Report a committed delivery, which is what acknowledges a prompt.
    pub(super) async fn deliver(&self, sid: &str, submission: &Submission) {
        self.send_event(
            sid,
            json!({"type": "user_input_delivered", "value": {
                "input_id": submission.input, "generation": "g", "content": "delivered"
            }}),
        )
        .await;
    }

    /// Turn started event (`running` publication point for tests).
    pub(super) async fn start_turn(&self, sid: &str) {
        self.send_event(
            sid,
            json!({"type": "user_input_run_started", "value": {
                "generation": "g", "request_id": "r"
            }}),
        )
        .await;
    }

    pub(super) fn v2(&self) -> Arc<V2Stdio> {
        Arc::clone(&self.v2)
    }

    pub(super) fn fake(&self) -> Arc<Fake> {
        Arc::clone(&self.fake)
    }
}

pub(super) struct Forwarded {
    pub id: RequestId,
    pub method: String,
    pub params: Value,
}

/// A prompt request the client still waits on, plus its internal input id.
pub(super) struct Submission {
    pub request: RequestId,
    pub input: String,
}
