//! Opt-in stdio-only adapter around the shared ACP Host.
//!
//! The adapter owns the v2 wire shape, not turn execution. It holds one slot per
//! session attached over this connection, routes notifications and blocking peer
//! requests through that slot's foreground state machine, and keeps v1 traffic
//! byte-identical by forwarding it untouched.

mod capabilities;
mod config;
mod errors;
mod lifecycle;
mod params;
mod prompts;
mod state;
mod updates;

#[cfg(test)]
#[path = "test_harness.rs"]
mod harness;

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};
use serde_json::{json, Value};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

use crate::transport::types::{AcpError, IncomingMessage, RequestId};
use crate::transport::AcpTransport;

use capabilities::ClientCapabilities;

use state::{Attach, Connection, Foreground, Reply, Routed};

pub(in crate::host) const INPUT: &str = "_peri/stdio-v2/input";
pub(in crate::host) const WITHDRAW: &str = "_peri/stdio-v2/withdraw";
pub(in crate::host) const CANCEL: &str = "_peri/stdio-v2/cancel";
pub(in crate::host) const CLOSE: &str = "_peri/stdio-v2/close";
pub(in crate::host) const CONFIG: &str = "_peri/stdio-v2/config";

/// Methods v2 never advertises per-object cancellation for.
const OBJECT_CANCELS: [&str; 2] = ["terminal/cancel", "tool_call/cancel"];

/// The internal input reply state for a submission already in the transcript.
const HANDED_OFF: &str = "delivered";

/// Whether the host reported the prompt's message as already inserted.
fn handed_off(reply: &Value) -> bool {
    reply.get("state").and_then(Value::as_str) == Some(HANDED_OFF)
}

pub(super) struct V2Stdio {
    inner: Arc<dyn AcpTransport>,
    permissions: Arc<SharedPermissionMode>,
    wire: Mutex<Connection>,
    /// Requests the adapter owes the host while answering it (the configuration
    /// snapshot a `session/resume` reply waits for).
    ///
    /// The host answers a setup request on its receive loop, so calling back
    /// through `inner` would wait on the loop that is currently blocked on this
    /// reply. These are returned from `recv` instead, exactly like a client
    /// request, and the client's own reply stays parked until they are answered.
    pending: Mutex<VecDeque<IncomingMessage>>,
    pending_wake: Notify,
}

impl V2Stdio {
    pub(super) fn new(
        inner: Arc<dyn AcpTransport>,
        permissions: Arc<SharedPermissionMode>,
    ) -> Self {
        Self {
            inner,
            permissions,
            wire: Mutex::default(),
            pending: Mutex::default(),
            pending_wake: Notify::new(),
        }
    }

    /// Queue a request the host must answer before it can see the client's reply.
    async fn park(&self, message: IncomingMessage) {
        self.pending.lock().await.push_back(message);
        self.pending_wake.notify_waiters();
    }

    async fn update(&self, sid: &str, update: Value) -> Result<(), AcpError> {
        self.inner
            .send_notification("session/update", json!({"sessionId":sid,"update":update}))
            .await
    }

    /// Send projected updates for one session.
    ///
    /// A `config_option_update` also refreshes the option cache that
    /// `session/set_config_option` validates against, so the adapter's view of
    /// what is settable always matches what the client was last told.
    async fn emit(
        &self,
        slot: &mut state::Slot,
        sid: &str,
        updates: Vec<Value>,
    ) -> Result<(), AcpError> {
        for update in updates {
            if update.get("sessionUpdate").and_then(Value::as_str) == Some("config_option_update") {
                if let Some(options) = update.get("configOptions").and_then(Value::as_array) {
                    slot.config = options.clone();
                }
            }
            self.update(sid, update).await?;
        }
        Ok(())
    }

    /// Route one inbound message, translating or answering it as needed.
    async fn inbound(&self, msg: IncomingMessage) -> Option<IncomingMessage> {
        let mut wire = self.wire.lock().await;
        let (id, method, params) = match msg {
            IncomingMessage::Request { id, method, params } => {
                if !wire.is_two() && method != "initialize" {
                    return self
                        .forward_or_reject_v1(&mut wire, id, method, params)
                        .await;
                }
                if method == "initialize" {
                    return self.initialize(&mut wire, id, params).await;
                }
                (id, method, params)
            }
            IncomingMessage::Notification { method, params } if wire.is_two() => {
                return self.notification(&mut wire, method, params).await;
            }
            other => return Some(other),
        };
        wire.initialized = true;
        self.v2_request(&mut wire, id, method, params).await
    }

    /// Before v2 is negotiated the connection may still be a v1 client.
    async fn forward_or_reject_v1(
        &self,
        wire: &mut Connection,
        id: RequestId,
        method: String,
        params: Value,
    ) -> Option<IncomingMessage> {
        if matches!(method.as_str(), INPUT | WITHDRAW | CANCEL | CLOSE | CONFIG) {
            let _ = self
                .inner
                .send_response(id, Err(errors::unknown_method(&method)))
                .await;
            return None;
        }
        wire.initialized = true;
        wire.version = state::Version::One;
        // The shared host skipped its one-session gate for this connection
        // because the wrapper reports multi-session support before the version
        // is known; a v1 connection keeps that gate here.
        match wire.admit_v1_setup(&method, &params) {
            state::V1Admission::Pass => {}
            state::V1Admission::Tracked(reply) => {
                wire.replies.insert(id.clone(), reply);
            }
            state::V1Admission::Refused(error) => {
                let _ = self.inner.send_response(id, Err(error)).await;
                return None;
            }
        }
        // A confirmed close/delete releases the v1 binding, as it does in the
        // shared host.
        if matches!(method.as_str(), "session/close" | "session/delete") {
            if let Some(sid) = params.get("sessionId").and_then(Value::as_str) {
                wire.replies
                    .insert(id.clone(), Reply::V1Close(sid.to_owned()));
            }
        }
        Some(IncomingMessage::Request { id, method, params })
    }

    async fn initialize(
        &self,
        wire: &mut Connection,
        id: RequestId,
        params: Value,
    ) -> Option<IncomingMessage> {
        let requested = params.get("protocolVersion").and_then(Value::as_u64);
        let repeat = wire.is_two();
        if repeat && wire.initialized {
            let _ = self
                .inner
                .send_response(id, Err(errors::already_initialized()))
                .await;
            return None;
        }
        // ACP v2 reads protocolVersion before any other parameter and rejects
        // out-of-range values (0..=65535) with Invalid params.
        let negotiable = (matches!(wire.version, state::Version::Unset) || repeat)
            && requested.is_some_and(|version| version >= 2)
            && params.get("info").is_some();
        if negotiable {
            if requested.is_some_and(|version| version > 65535) {
                let _ = self
                    .inner
                    .send_response(id, Err(errors::invalid_protocol_version()))
                    .await;
                return None;
            }
            match params::initialize_params(&params) {
                Ok(translated) => {
                    wire.replies.insert(
                        id.clone(),
                        Reply::Initialize(ClientCapabilities::capture(&params)),
                    );
                    return Some(IncomingMessage::Request {
                        id,
                        method: "initialize".into(),
                        params: translated,
                    });
                }
                Err(error) => {
                    let _ = self.inner.send_response(id, Err(error)).await;
                    return None;
                }
            }
        }
        if repeat {
            // The pinned protocol allows repeats only before the first other
            // request; a repeat below v2 cannot renegotiate this connection and
            // must not disturb the last success.
            let _ = self
                .inner
                .send_response(
                    id,
                    Err(errors::invalid_params(
                        "ACP v2 Preview requires protocolVersion 2 and client info",
                    )),
                )
                .await;
            return None;
        }
        wire.initialized = true;
        wire.version = state::Version::One;
        Some(IncomingMessage::Request {
            id,
            method: "initialize".into(),
            params,
        })
    }

    /// The v2 method table. Unknown methods are rejected, never forwarded.
    async fn v2_request(
        &self,
        wire: &mut Connection,
        id: RequestId,
        method: String,
        params: Value,
    ) -> Option<IncomingMessage> {
        let routed = match method.as_str() {
            "session/new" => lifecycle::new_session(wire, id.clone(), &params),
            "session/resume" => lifecycle::resume_session(wire, id.clone(), &params),
            "session/list" => lifecycle::list_sessions(id.clone(), &params),
            "session/close" => lifecycle::close_session(wire, id.clone(), &params),
            "session/delete" => lifecycle::delete_session(wire, id.clone(), &params),
            "session/prompt" => lifecycle::prompt(wire, id.clone(), &params),
            "session/set_config_option" => config::set_config_option(wire, id.clone(), &params),
            "auth/login" | "auth/logout" | "session/fork" | "mcp/message" => {
                Routed::Answered(Err(errors::unknown_method(&method)))
            }
            m if OBJECT_CANCELS.contains(&m) => {
                Routed::Answered(Err(errors::unsupported_object_cancel(m)))
            }
            _ => Routed::Answered(Err(errors::unknown_method(&method))),
        };
        self.settle(routed, id).await
    }

    async fn settle(&self, routed: Routed, answer_id: RequestId) -> Option<IncomingMessage> {
        match routed {
            Routed::Forward {
                id,
                method,
                params: mut forwarded,
            } => {
                if let Some(fields) = forwarded.as_object_mut() {
                    fields.remove("_meta");
                }
                Some(IncomingMessage::Request {
                    id,
                    method,
                    params: forwarded,
                })
            }
            Routed::Answered(result) => {
                let _ = self.inner.send_response(answer_id, result).await;
                None
            }
            Routed::Parked => None,
        }
    }

    /// Notifications the connection consumes instead of forwarding.
    async fn notification(
        &self,
        wire: &mut Connection,
        method: String,
        params: Value,
    ) -> Option<IncomingMessage> {
        match method.as_str() {
            // A cancel for a request id we can still withdraw settles that
            // prompt; anything else is ignored per the pinned rule (the original
            // request still has its own eventual reply).
            "$/cancel_request" => {
                let target = params
                    .get("requestId")
                    .cloned()
                    .and_then(|id| serde_json::from_value::<RequestId>(id).ok())
                    .and_then(|id| wire.prompt_target(&id))?;
                let (session_id, input_id) = target;
                let internal_id = RequestId::String(uuid::Uuid::now_v7().to_string());
                wire.replies
                    .insert(internal_id.clone(), Reply::Withdraw(input_id.clone()));
                Some(IncomingMessage::Request {
                    id: internal_id,
                    method: WITHDRAW.into(),
                    params: json!({"sessionId": session_id, "inputId": input_id}),
                })
            }
            "session/cancel" => {
                let sid = params.get("sessionId").and_then(Value::as_str)?.to_owned();
                let input_ids = {
                    let slot = wire
                        .slots
                        .get_mut(&sid)
                        .filter(|slot| slot.attach == Attach::Attached)?;
                    // The cancelled turn's outstanding peer requests must not
                    // republish a foreground state afterwards.
                    slot.cancel_turn();
                    slot.prompts.pending_ids(&sid)
                };
                let internal_id = RequestId::String(uuid::Uuid::now_v7().to_string());
                wire.replies.insert(internal_id.clone(), Reply::Cancel);
                Some(IncomingMessage::Request {
                    id: internal_id,
                    method: CANCEL.into(),
                    params: json!({"sessionId": sid, "inputIds": input_ids}),
                })
            }
            // v2 does not implement MCP over ACP.
            "mcp/message" => None,
            _ => None,
        }
    }

    /// Publish `running` for a turn that just started (or resumed) and settle a
    /// delivered prompt.
    async fn agent_event(&self, wire: &mut Connection, sid: &str, event: &crate::event::AcpEvent) {
        let Some(slot) = wire.slots.get_mut(sid) else {
            return;
        };
        // A slot that is still attaching only accepts live events while it is
        // replaying, which is the one case where the host streams activity
        // before the setup reply.
        if slot.attach != Attach::Attached && slot.replay.is_none() {
            return;
        }
        match event {
            crate::event::AcpEvent::UserInputRunStarted { .. }
            | crate::event::AcpEvent::ExecutionStarted { .. } => {
                if slot.begin_turn() {
                    let _ = self
                        .update(sid, updates::state(Foreground::Running, None))
                        .await;
                }
            }
            crate::event::AcpEvent::UserInputDelivered { input_id, .. } => {
                let delivered = slot.prompts.accepted(sid, input_id);
                if slot.begin_turn() {
                    let _ = self
                        .update(sid, updates::state(Foreground::Running, None))
                        .await;
                }
                if let Some((message, waiters)) = delivered {
                    let _ = self.update(sid, message).await;
                    for waiter in waiters {
                        let _ = self
                            .inner
                            .send_response(waiter, Ok(json!({"messageId": input_id})))
                            .await;
                    }
                }
            }
            _ => {}
        }
    }
}

#[async_trait]
impl AcpTransport for V2Stdio {
    fn owns_stdio_preview_requests(&self) -> bool {
        true
    }

    fn allows_concurrent_sessions(&self) -> bool {
        // v2 sessions are addressed by id, so one connection may hold several.
        true
    }

    async fn observe_execution_failure(&self, sid: &str) {
        let mut wire = self.wire.lock().await;
        if !wire.is_two() {
            return;
        }
        if let Some(slot) = wire.slots.get_mut(sid) {
            slot.execution_failed = true;
        }
    }

    async fn settle_withdrawn(&self, session_id: &str, input_ids: &[String]) {
        if input_ids.is_empty() {
            return;
        }
        let waiters = {
            let mut wire = self.wire.lock().await;
            if !wire.is_two() {
                return;
            }
            let Some(slot) = wire.slots.get_mut(session_id) else {
                return;
            };
            input_ids
                .iter()
                .flat_map(|input| slot.prompts.reject_in_session(session_id, input))
                .collect::<Vec<_>>()
        };
        for waiter in waiters {
            let _ = self
                .inner
                .send_response(waiter, Err(errors::cancelled()))
                .await;
        }
    }

    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.send_cancellable_request(method, params, CancellationToken::new())
            .await
    }

    async fn send_cancellable_request(
        &self,
        method: &str,
        params: Value,
        token: CancellationToken,
    ) -> Result<Value, AcpError> {
        let (v2, client) = {
            let wire = self.wire.lock().await;
            (wire.is_two(), wire.client)
        };
        if !v2 {
            return self
                .inner
                .send_cancellable_request(method, params, token)
                .await;
        }
        // This adapter only ever emits form elicitations. A client that did not
        // advertise `elicitation.form` would answer -32601 anyway; failing here
        // lets the broker take its MethodUnsupported degradation without a
        // round-trip or a dangling request.
        if method == "elicitation/create" && !client.supports_form_elicitation() {
            return Err(errors::unknown_method(method));
        }
        let params = params::peer_request(method, params)?;
        let sid = params
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        // Capture the foreground generation together with the `requires_action`
        // snapshot. A completion that outlived its turn must not overwrite the
        // state of a newer turn (which may already be `requires_action`).
        let mut blocked = None;
        if matches!(method, "session/request_permission" | "elicitation/create") {
            if let Some(sid) = sid.as_deref() {
                let mut wire = self.wire.lock().await;
                let step = wire.slots.get_mut(sid).and_then(|slot| {
                    slot.block()
                        .map(|(epoch, publish)| (slot.incarnation, epoch, publish))
                });
                if let Some((incarnation, epoch, publish)) = step {
                    if publish {
                        self.update(sid, updates::state(Foreground::RequiresAction, None))
                            .await?;
                    }
                    blocked = Some((incarnation, epoch));
                }
            }
        }
        let result = if blocked.is_some() {
            // The broker can drop its future when the turn stops. An owned
            // cancellable write still has to send the exact peer request ID.
            let inner = Arc::clone(&self.inner);
            let method = method.to_owned();
            let request_token = token.child_token();
            let _cancel_on_drop = request_token.clone().drop_guard();
            tokio::spawn(async move {
                inner
                    .send_cancellable_request(&method, params, request_token)
                    .await
            })
            .await
            .map_err(|_| AcpError::new(errors::INTERNAL, "Peer request task failed"))?
        } else {
            self.inner
                .send_cancellable_request(method, params, token.clone())
                .await
        };
        if let (Some((incarnation, epoch)), Some(sid)) = (blocked, sid.as_deref()) {
            let mut wire = self.wire.lock().await;
            let published = wire
                .slots
                .get_mut(sid)
                .is_some_and(|slot| slot.unblock(incarnation, epoch));
            if published {
                self.update(sid, updates::state(Foreground::Running, None))
                    .await?;
            }
        }
        result
    }

    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        let mut wire = self.wire.lock().await;
        if !wire.is_two() {
            return self.inner.send_notification(method, params).await;
        }
        if method == "$/cancel_request" {
            return self.inner.send_notification(method, params).await;
        }
        let Some(sid) = params
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Ok(());
        };
        match method {
            "session/update" => {
                let Some(update) = params.get("update").cloned() else {
                    return Ok(());
                };
                let Some(slot) = wire.slots.get_mut(&sid) else {
                    return Ok(());
                };
                let replaying = slot.replay.is_some();
                if slot.attach == Attach::Attaching
                    && !replaying
                    && updates::is_attach_held(&update)
                {
                    slot.hold(update);
                    return Ok(());
                }
                // During replay the first chunk of each message gets a
                // replacement boundary, so a message already streamed live is
                // not appended twice.
                let reset = slot.replay.as_mut().is_some_and(|replay| {
                    match (
                        update.get("sessionUpdate").and_then(Value::as_str),
                        update.get("messageId").and_then(Value::as_str),
                    ) {
                        (Some(kind), Some(id)) if kind.ends_with("_chunk") => {
                            replay.messages.insert((kind.into(), id.into()))
                        }
                        _ => false,
                    }
                });
                if let Some(updates) = updates::project_update(update, reset) {
                    self.emit(slot, &sid, updates).await?;
                }
            }
            "peri/agent_event" => {
                let Some(event) = params
                    .get("event_json")
                    .and_then(Value::as_str)
                    .and_then(|data| serde_json::from_str::<crate::event::AcpEvent>(data).ok())
                else {
                    return Ok(());
                };
                self.agent_event(&mut wire, &sid, &event).await;
            }
            "peri/agent_event_done" => {
                let Some(slot) = wire.slots.get_mut(&sid) else {
                    return Ok(());
                };
                if slot.attach != Attach::Attached && slot.replay.is_none() {
                    return Ok(());
                }
                let reason = params
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .unwrap_or("error");
                let failed = std::mem::take(&mut slot.execution_failed);
                let reason = if failed && reason != "cancelled" {
                    "error"
                } else {
                    reason
                };
                // A turn ending is not evidence that a separately queued prompt
                // was cancelled. Only a committed withdrawal settles it.
                if slot.end_turn() {
                    self.update(&sid, updates::state(Foreground::Idle, Some(reason)))
                        .await?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        loop {
            // Register before observing the queue so a park that lands while this
            // task is waiting for the client cannot be missed.
            let parked = self.pending_wake.notified();
            tokio::pin!(parked);
            parked.as_mut().enable();
            if let Some(message) = self.pending.lock().await.pop_front() {
                return Some(message);
            }
            let message = tokio::select! {
                biased;
                () = &mut parked => continue,
                message = self.inner.recv() => message?,
            };
            if let Some(message) = self.inbound(message).await {
                return Some(message);
            }
        }
    }

    async fn send_response(
        &self,
        id: RequestId,
        result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        let mut wire = self.wire.lock().await;
        let two = wire.is_two();
        let mut flush: Vec<(String, Value)> = Vec::new();
        let result = match wire.replies.remove(&id) {
            Some(Reply::Initialize(client)) => result.map(|_| {
                wire.version = state::Version::Two;
                wire.client = client;
                // v1 stdio keeps its historical bypass mode. The negotiated
                // preview uses the existing Peri approval policy and broker.
                self.permissions.store(PermissionMode::Default);
                capabilities::initialize_response()
            }),
            Some(Reply::Input(input)) => {
                match result {
                    Err(error) => {
                        for waiter in wire.reject_prompt(&input) {
                            self.inner.send_response(waiter, Err(error.clone())).await?;
                        }
                    }
                    // A keyed retry whose message is already in the transcript:
                    // no delivery event will follow it, so the original id is
                    // acknowledged here instead of waiting for one.
                    Ok(reply) if handed_off(&reply) => {
                        for waiter in wire.acknowledge_prompt(&input) {
                            let _ = self
                                .inner
                                .send_response(waiter, Ok(json!({"messageId": input})))
                                .await;
                        }
                    }
                    Ok(_) => {}
                }
                return Ok(());
            }
            Some(Reply::Cancel) => {
                if let Some(inputs) = result.as_ref().ok().and_then(Value::as_array) {
                    for input in inputs.iter().filter_map(Value::as_str) {
                        for waiter in wire.reject_prompt(input) {
                            self.inner
                                .send_response(waiter, Err(errors::cancelled()))
                                .await?;
                        }
                    }
                }
                return Ok(());
            }
            Some(Reply::Withdraw(input)) => {
                if result.is_ok() {
                    for waiter in wire.reject_prompt(&input) {
                        self.inner
                            .send_response(waiter, Err(errors::cancelled()))
                            .await?;
                    }
                }
                return Ok(());
            }
            Some(Reply::ConfigOptions) => result.map(|mut value| {
                if let Some(object) = value.as_object_mut() {
                    config::project_options(object);
                }
                value
            }),
            Some(Reply::ConfigFetch {
                client,
                session,
                result: mut parked,
            }) => {
                let fetch = result;
                match (parked.as_mut(), fetch) {
                    (Ok(value), Ok(reply)) => {
                        let options = config::options_from(&reply);
                        if let Some(slot) = wire.slots.get_mut(&session) {
                            slot.config = options.clone();
                        }
                        if let Some(object) = value.as_object_mut() {
                            object.insert("configOptions".into(), Value::Array(options));
                        }
                    }
                    // The session is attached and usable; a missing option list
                    // is reported as "no options advertised" rather than by
                    // failing a resume the host already committed.
                    (Ok(_), Err(error)) => tracing::warn!(
                        code = error.code,
                        session = %session,
                        "ACP v2 configuration fetch failed; the resume reply omits configOptions"
                    ),
                    (Err(_), _) => {}
                }
                let parked =
                    parked.map_err(|error| if two { errors::normalize(error) } else { error });
                return self.inner.send_response(client, parked).await;
            }
            Some(Reply::Setup {
                session,
                replay,
                created,
                owner,
            }) => {
                let method = if session.is_some() {
                    "session/resume"
                } else {
                    "session/new"
                };
                let mut sid: Option<String> = None;
                match &result {
                    Ok(value) => {
                        sid = session
                            .as_deref()
                            .or_else(|| value.get("sessionId").and_then(Value::as_str))
                            .map(str::to_owned);
                        if let Some(sid) = sid.as_deref() {
                            let slot = wire.insert_slot(sid, Attach::Attached);
                            slot.attach = Attach::Attached;
                            let foreground = slot.foreground;
                            if replay {
                                slot.replay = None;
                                self.update(sid, json!({"sessionUpdate":"replay_complete"}))
                                    .await?;
                            }
                            if session.is_some() {
                                // v2 reports the state snapshot and the session
                                // metadata before the resume reply: the snapshot
                                // is required after any replay and before it, and
                                // the host pushes the command table and options
                                // ahead of its own reply.
                                self.update(sid, updates::state(foreground, None)).await?;
                                let held = slot.flush_held();
                                let projected = held
                                    .into_iter()
                                    .flat_map(|update| {
                                        updates::project_update(update, false).unwrap_or_default()
                                    })
                                    .collect();
                                self.emit(slot, sid, projected).await?;
                            } else {
                                // A new session's id only becomes routable with
                                // the reply, so its metadata waits for it.
                                flush = slot
                                    .flush_held()
                                    .into_iter()
                                    .map(|update| (sid.to_owned(), update))
                                    .collect();
                            }
                        }
                    }
                    Err(_) => {
                        if let Some(sid) = session.as_deref() {
                            // A newer attach may already own the slot; only the
                            // attempt that still owns it may unwind it, and a
                            // takeover keeps its own held updates and waiter.
                            let owned = wire.slots.get(sid).is_some_and(|slot| slot.setup == owner);
                            if owned {
                                if created {
                                    wire.slots.remove(sid);
                                } else if let Some(slot) = wire.slots.get_mut(sid) {
                                    // The previous attach is still live.
                                    slot.attach = Attach::Attached;
                                    slot.replay = None;
                                    slot.flush_held();
                                }
                            }
                        }
                    }
                }
                let projected = result
                    .map(|value| lifecycle::lifecycle_result(method, value))
                    .map_err(|error| if two { errors::normalize(error) } else { error });
                // Cache the option list the host reported; `session/new` and
                // `session/load` already carry it in their reply.
                if let Ok(value) = &projected {
                    if let Some(options) = value.get("configOptions").and_then(Value::as_array) {
                        if let Some(slot) = sid
                            .as_deref()
                            .and_then(|session_id| wire.slots.get_mut(session_id))
                        {
                            slot.config = options.clone();
                        }
                    }
                }
                // A resume reply has no v1 `configOptions`; ask the host for the
                // list `session/new` returns and answer the client once it is
                // known, so both entry points expose the same configuration
                // surface. `session/load` already replayed it.
                if projected.is_ok() && session.is_some() && !replay {
                    if let Some(session_id) = sid.as_deref() {
                        let config_id = RequestId::String(uuid::Uuid::now_v7().to_string());
                        wire.replies.insert(
                            config_id.clone(),
                            Reply::ConfigFetch {
                                client: id,
                                session: session_id.to_owned(),
                                result: projected,
                            },
                        );
                        self.park(IncomingMessage::Request {
                            id: config_id,
                            method: CONFIG.into(),
                            params: config::fetch_params(session_id),
                        })
                        .await;
                        return Ok(());
                    }
                }
                self.inner.send_response(id, projected).await?;
                for (sid, update) in flush {
                    let slot = wire.slots.get_mut(&sid);
                    let projected = updates::project_update(update, false).unwrap_or_default();
                    match slot {
                        Some(slot) => self.emit(slot, &sid, projected).await?,
                        None => {
                            for update in projected {
                                self.update(&sid, update).await?;
                            }
                        }
                    }
                }
                return Ok(());
            }
            Some(Reply::V1Setup { session }) => {
                match &result {
                    Ok(value) => {
                        let sid = session
                            .as_deref()
                            .or_else(|| value.get("sessionId").and_then(Value::as_str))
                            .map(str::to_owned);
                        match sid {
                            Some(sid) => wire.commit_v1_setup(&sid),
                            None => wire.fail_v1_setup(),
                        }
                    }
                    Err(_) => wire.fail_v1_setup(),
                }
                result
            }
            Some(Reply::V1Close(sid)) if result.is_ok() => {
                wire.release_v1_session(&sid);
                result
            }
            Some(Reply::V1Close(_)) => result,
            Some(Reply::Close(sid) | Reply::Delete(sid)) if result.is_ok() => {
                Ok(self.teardown(&mut wire, &sid).await)
            }
            Some(Reply::Close(_) | Reply::Delete(_)) => result,
            None => result,
        };
        let result = if two {
            result.map_err(errors::normalize)
        } else {
            result
        };
        self.inner.send_response(id, result).await
    }
}

impl V2Stdio {
    /// Settle one session's pending prompts and drop its slot.
    ///
    /// A foreground turn is reported as `idle cancelled` first: close/delete end
    /// that work from the client's point of view, and the settlement replies
    /// (`-32800`) must not arrive before the state that explains them.
    async fn teardown(&self, wire: &mut Connection, sid: &str) -> Value {
        let Some(slot) = wire.slots.get_mut(sid) else {
            return json!({});
        };
        let foreground = !slot.foreground.is_idle();
        let waiters = slot.prompts.reject_session(sid);
        slot.cancel_turn();
        if foreground {
            let _ = self
                .update(sid, updates::state(Foreground::Idle, Some("cancelled")))
                .await;
        }
        for waiter in waiters {
            let _ = self
                .inner
                .send_response(waiter, Err(errors::cancelled()))
                .await;
        }
        wire.slots.remove(sid);
        json!({})
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

#[cfg(test)]
#[path = "boundary_test.rs"]
mod boundary_tests;
