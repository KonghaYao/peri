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

/// Absolute `cwd` for setup requests. A non-absolute path is rejected with
/// Invalid params before the adapter forwards anything, so this must be a platform
/// absolute path, not a POSIX-only literal.
#[cfg(windows)]
pub(super) const CWD: &str = r"C:\workspace";
#[cfg(not(windows))]
pub(super) const CWD: &str = "/workspace";

/// A v1-shaped option list matching what the host builds for a live session.
pub(super) fn config_options(mode: &str) -> Value {
    json!([{
        "id": "mode",
        "name": "Session Mode",
        "type": "select",
        "currentValue": mode,
        "category": "mode",
        "options": [
            {"value": "default", "name": "Default"},
            {"value": "accept_edit", "name": "Accept Edit"},
            {"value": "auto", "name": "Auto Mode"},
            {"value": "bypass", "name": "Bypass"}
        ]
    }])
}

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
            .unwrap_or_else(|_| Err(AcpError::new(super::errors::INTERNAL, "test peer dropped")))
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
    ///
    /// The declaration goes under the v2 field name `capabilities`; the adapter
    /// translates it to the v1 `clientCapabilities` the host reads.
    pub(super) async fn negotiate_with(&mut self, capabilities: Value) {
        let id = self.id();
        let mut params = json!({
            "protocolVersion": 2,
            "info": {"name": "probe", "version": "1"}
        });
        if capabilities != json!({}) {
            params["capabilities"] = capabilities;
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

    /// Complete a v1 `initialize`, which the adapter forwards untouched.
    pub(super) async fn negotiate_v1(&mut self) {
        let id = self.id();
        self.request(
            id.clone(),
            "initialize",
            json!({"protocolVersion": 1, "clientCapabilities": {"fs": {"readTextFile": true}}}),
        )
        .await;
        let forwarded = self.expect_forwarded().await;
        assert_eq!(forwarded.method, "initialize");
        assert_eq!(
            forwarded.params["clientCapabilities"]["fs"]["readTextFile"], true,
            "v1 parameters must reach the host unchanged"
        );
        self.respond(
            forwarded.id,
            Ok(json!({"protocolVersion": 1, "agentCapabilities": {}})),
        )
        .await;
        let (answered, result) = self.expect_out().await.response();
        assert_eq!(answered, id);
        assert_eq!(result.expect("v1 initialize result")["protocolVersion"], 1);
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
        self.open_with_config(sid, json!([])).await;
    }

    /// Create a session whose host reply advertises a configuration list.
    pub(super) async fn open_with_config(&mut self, sid: &str, options: Value) {
        let id = self.id();
        self.request(id.clone(), "session/new", json!({"cwd": CWD}))
            .await;
        let forwarded = self.expect_forwarded().await;
        assert_eq!(forwarded.method, "session/new");
        self.respond(
            forwarded.id,
            Ok(json!({"sessionId": sid, "modes": {}, "configOptions": options})),
        )
        .await;
        let (answered, result) = self.expect_out().await.response();
        assert_eq!(answered, id);
        assert_eq!(result.expect("new result")["sessionId"], sid);
    }

    /// Submit a prompt the host queued successfully (internal input replied
    /// `queued`).
    ///
    /// The client's prompt request stays open until committed delivery.
    pub(super) async fn enqueue_prompt(&mut self, sid: &str, text: &str) -> Submission {
        let (request, forwarded) = self.send_prompt(sid, text).await;
        let input = forwarded.params["inputId"]
            .as_str()
            .expect("input id")
            .to_owned();
        self.respond(
            forwarded.id.clone(),
            Ok(json!({"messageId": input, "state": "queued"})),
        )
        .await;
        self.expect_quiet().await;
        Submission { request, input }
    }

    /// Submit a prompt the host reports as already inserted (a keyed retry).
    pub(super) async fn send_prompt(&mut self, sid: &str, text: &str) -> (RequestId, Forwarded) {
        self.send_prompt_with(sid, text, None).await
    }

    /// Submit a prompt that names an idempotency key.
    pub(super) async fn send_prompt_with(
        &mut self,
        sid: &str,
        text: &str,
        key: Option<&str>,
    ) -> (RequestId, Forwarded) {
        let id = self.request_prompt(sid, text, key).await;
        let forwarded = self.expect_forwarded().await;
        assert_eq!(forwarded.method, super::INPUT);
        assert_eq!(forwarded.params["sessionId"], sid);
        (id, forwarded)
    }

    /// Send a `session/prompt` without expecting the adapter to forward it (a
    /// keyed retry may be parked on the submission already in flight).
    pub(super) async fn request_prompt(
        &mut self,
        sid: &str,
        text: &str,
        key: Option<&str>,
    ) -> RequestId {
        let id = self.id();
        let mut params = json!({
            "sessionId": sid,
            "prompt": [{"type": "text", "text": text}]
        });
        if let Some(key) = key {
            params["idempotencyKey"] = json!(key);
        }
        self.request(id.clone(), "session/prompt", params).await;
        id
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

    /// Answer the option snapshot a `session/resume` reply waits for.
    ///
    /// A resume has no v1 `configOptions`, so the adapter asks the host for the
    /// list before it answers the client; the reply keeps its v1 shape.
    pub(super) async fn fetch_config(&mut self, session_id: &str, options: Value) {
        let forwarded = self.expect_forwarded().await;
        assert_eq!(forwarded.method, super::CONFIG);
        assert_eq!(forwarded.params["sessionId"], session_id);
        self.respond(forwarded.id, Ok(json!({"configOptions": options})))
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

/// The error code of a failed response.
pub(super) fn code(error: &AcpError) -> i64 {
    error.code
}

/// Read the next outbound item and require it to be a `session/update` state.
pub(super) async fn expect_state(harness: &Harness, sid: &str, state: &str) -> Value {
    let out = harness.expect_out().await;
    let (method, params) = match out {
        Out::Notification { method, params } => (method, params),
        other => panic!("expected a state notification, got {other:?}"),
    };
    assert_eq!(method, "session/update");
    assert_eq!(params["sessionId"], sid, "state update for another session");
    let update = &params["update"];
    assert_eq!(update["sessionUpdate"], "state_update", "{update}");
    assert_eq!(update["state"], state, "{update}");
    update.clone()
}

pub(super) async fn expect_error(harness: &Harness, id: RequestId, expected: i64) -> AcpError {
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, id);
    let error = result.expect_err("expected an error reply");
    assert_eq!(code(&error), expected, "{error}");
    error
}

pub(super) async fn begin_turn(harness: &Harness, sid: &str) {
    harness.start_turn(sid).await;
    expect_state(harness, sid, "running").await;
}

pub(super) fn permission(sid: &str) -> Value {
    json!({"sessionId": sid, "toolCall": {"title": "run a tool"}})
}
