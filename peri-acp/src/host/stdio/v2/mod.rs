//! Opt-in stdio-only adapter around the shared ACP Host.

mod prompts;
mod wire;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::transport::stdio::StdioTransport;
use crate::transport::types::{AcpError, IncomingMessage, RequestId};
use crate::transport::AcpTransport;
use prompts::{Admission, Prompts};

pub(in crate::host) const INPUT: &str = "_peri/stdio-v2/input";
pub(in crate::host) const WITHDRAW: &str = "_peri/stdio-v2/withdraw";
pub(in crate::host) const CANCEL: &str = "_peri/stdio-v2/cancel";
pub(in crate::host) const CLOSE: &str = "_peri/stdio-v2/close";

pub(super) struct V2Stdio {
    inner: Arc<StdioTransport>,
    permissions: Arc<SharedPermissionMode>,
    wire: Mutex<Connection>,
}

#[derive(Default)]
enum Version {
    #[default]
    Unset,
    One,
    Two,
}

#[derive(Default)]
struct Connection {
    version: Version,
    replies: HashMap<RequestId, Reply>,
    prompts: Prompts,
    active: Option<String>,
    foreground: bool,
    execution_failed: bool,
    replay: Option<Replay>,
}

enum Reply {
    Initialize,
    Setup {
        session: Option<String>,
        replay: bool,
    },
    Close(String),
    Input(String),
    Withdraw(String),
    Cancel,
}

struct Replay {
    session: String,
    messages: HashSet<(String, String)>,
}

impl V2Stdio {
    pub(super) fn new(inner: StdioTransport, permissions: Arc<SharedPermissionMode>) -> Self {
        Self {
            inner: Arc::new(inner),
            permissions,
            wire: Mutex::default(),
        }
    }

    async fn update(&self, sid: &str, update: Value) -> Result<(), AcpError> {
        self.inner
            .send_notification("session/update", json!({"sessionId":sid,"update":update}))
            .await
    }

    async fn inbound(&self, msg: IncomingMessage) -> Option<IncomingMessage> {
        let mut wire = self.wire.lock().await;
        let (id, mut method, mut params) = match msg {
            IncomingMessage::Request { id, method, params } => (id, method, params),
            IncomingMessage::Notification { method, params }
                if matches!(wire.version, Version::Two) =>
            {
                if method == "session/cancel" {
                    let sid = params.get("sessionId").and_then(Value::as_str)?;
                    if wire.active.as_deref() != Some(sid) {
                        return None;
                    }
                    let id = RequestId::String(uuid::Uuid::now_v7().to_string());
                    let inputs = wire.prompts.pending_ids(sid);
                    wire.replies.insert(id.clone(), Reply::Cancel);
                    return Some(IncomingMessage::Request {
                        id,
                        method: CANCEL.into(),
                        params: json!({"sessionId": sid, "inputIds": inputs}),
                    });
                }
                if method == "$/cancel_request" {
                    let id = serde_json::from_value(params.get("requestId")?.clone()).ok()?;
                    let (sid, input) = wire.prompts.target(&id)?;
                    let internal_id = RequestId::String(uuid::Uuid::now_v7().to_string());
                    wire.replies
                        .insert(internal_id.clone(), Reply::Withdraw(input.clone()));
                    return Some(IncomingMessage::Request {
                        id: internal_id,
                        method: WITHDRAW.into(),
                        params: json!({"sessionId": sid, "inputId": input}),
                    });
                }
                return None;
            }
            _ => return Some(msg),
        };
        if method == "initialize" {
            let requested = params.get("protocolVersion").and_then(Value::as_u64);
            if matches!(wire.version, Version::Two) {
                let _ = self
                    .inner
                    .send_response(
                        id,
                        Err(AcpError::new(-32600, "Connection already initialized")),
                    )
                    .await;
                return None;
            }
            if matches!(wire.version, Version::Unset)
                && requested.is_some_and(|n| n >= 2)
                && params.get("info").is_some()
            {
                match wire::initialize_params(&params) {
                    Ok(translated) => {
                        params = translated;
                        wire.replies.insert(id.clone(), Reply::Initialize);
                    }
                    Err(error) => {
                        let _ = self.inner.send_response(id, Err(error)).await;
                        return None;
                    }
                }
                return Some(IncomingMessage::Request { id, method, params });
            }
        }

        if !matches!(wire.version, Version::Two) {
            if matches!(method.as_str(), INPUT | WITHDRAW | CANCEL | CLOSE) {
                let _ = self
                    .inner
                    .send_response(id, Err(AcpError::new(-32601, "Unknown method")))
                    .await;
                return None;
            }
            wire.version = Version::One;
            return Some(IncomingMessage::Request { id, method, params });
        }

        let sid = params
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let reject = |error| async {
            let _ = self.inner.send_response(id.clone(), Err(error)).await;
        };
        match method.as_str() {
            "session/new" => {
                wire.replies.insert(
                    id.clone(),
                    Reply::Setup {
                        session: None,
                        replay: false,
                    },
                );
            }
            "session/resume" => {
                let Some(sid) = sid else {
                    reject(AcpError::new(-32602, "Missing sessionId")).await;
                    return None;
                };
                let replay = match params.get("replayFrom").filter(|cursor| !cursor.is_null()) {
                    None => false,
                    Some(cursor) if cursor.get("type").and_then(Value::as_str) == Some("start") => {
                        true
                    }
                    _ => {
                        reject(AcpError::new(-32602, "Unsupported replay cursor")).await;
                        return None;
                    }
                };
                if replay && wire.foreground {
                    reject(AcpError::new(
                        -32000,
                        "Replay is unavailable while this connection is running a turn",
                    ))
                    .await;
                    return None;
                }
                if let Some(object) = params.as_object_mut() {
                    object.remove("replayFrom");
                }
                if replay {
                    method = "session/load".into();
                    wire.replay = Some(Replay {
                        session: sid.clone(),
                        messages: HashSet::new(),
                    });
                }
                wire.replies.insert(
                    id.clone(),
                    Reply::Setup {
                        session: Some(sid),
                        replay,
                    },
                );
            }
            "session/list" => {
                if params.get("cursor").is_some_and(|v| !v.is_null()) {
                    reject(AcpError::new(-32602, "Unsupported session list cursor")).await;
                    return None;
                }
            }
            "session/close" => {
                let Some(sid) = sid else {
                    reject(AcpError::new(-32602, "Missing sessionId")).await;
                    return None;
                };
                if wire.active.as_ref() != Some(&sid) {
                    let _ = self.inner.send_response(id, Ok(json!({}))).await;
                    return None;
                }
                let input_ids = wire.prompts.pending_ids(&sid);
                wire.replies.insert(id.clone(), Reply::Close(sid.clone()));
                method = CLOSE.into();
                params = json!({"sessionId":sid,"inputIds":input_ids});
            }
            "session/prompt" => {
                let Some(sid) = sid.filter(|sid| wire.active.as_ref() == Some(sid)) else {
                    reject(AcpError::new(-32002, "Session not active")).await;
                    return None;
                };
                let blocks = match wire::prompt_blocks(&params) {
                    Ok(blocks) => blocks,
                    Err(error) => {
                        reject(error).await;
                        return None;
                    }
                };
                let key = match params.get("idempotencyKey").filter(|key| !key.is_null()) {
                    None => None,
                    Some(key)
                        if key
                            .as_str()
                            .is_some_and(|key| !key.is_empty() && key.len() <= 512) =>
                    {
                        key.as_str()
                    }
                    _ => {
                        reject(AcpError::new(-32602, "Invalid idempotencyKey")).await;
                        return None;
                    }
                };
                match wire.prompts.admit(&sid, key, blocks.clone(), id.clone()) {
                    Ok(Admission::Submit(input)) => {
                        let internal_id = RequestId::String(uuid::Uuid::now_v7().to_string());
                        wire.replies
                            .insert(internal_id.clone(), Reply::Input(input.clone()));
                        return Some(IncomingMessage::Request {
                            id: internal_id,
                            method: INPUT.into(),
                            params: json!({"sessionId":sid,"inputId":input,"prompt":blocks}),
                        });
                    }
                    Ok(Admission::Pending) => return None,
                    Ok(Admission::Accepted(input)) => {
                        let _ = self
                            .inner
                            .send_response(id, Ok(json!({"messageId":input})))
                            .await;
                        return None;
                    }
                    Err(error) => {
                        reject(error).await;
                        return None;
                    }
                }
            }
            _ => {
                reject(AcpError::new(
                    -32601,
                    "Method not supported by Peri ACP v2 Preview",
                ))
                .await;
                return None;
            }
        }
        if let Some(fields) = params.as_object_mut() {
            fields.remove("_meta");
        }
        Some(IncomingMessage::Request { id, method, params })
    }
}

#[async_trait]
impl AcpTransport for V2Stdio {
    fn owns_stdio_preview_requests(&self) -> bool {
        true
    }

    async fn observe_execution_failure(&self, sid: &str) {
        let mut wire = self.wire.lock().await;
        if matches!(wire.version, Version::Two) && wire.active.as_deref() == Some(sid) {
            wire.execution_failed = true;
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
        let v2 = matches!(self.wire.lock().await.version, Version::Two);
        let params = if v2 {
            wire::peer_request(method, params)?
        } else {
            params
        };
        let sid = params
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let requires_action =
            if v2 && matches!(method, "session/request_permission" | "elicitation/create") {
                let guard = self.wire.lock().await;
                if let Some(sid) = sid
                    .as_deref()
                    .filter(|sid| guard.foreground && guard.active.as_deref() == Some(*sid))
                {
                    self.update(sid, wire::state("requires_action", None))
                        .await?;
                    true
                } else {
                    false
                }
            } else {
                false
            };
        let result = if requires_action {
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
            .map_err(|_| AcpError::new(-32603, "Peer request task failed"))?
        } else {
            self.inner
                .send_cancellable_request(method, params, token.clone())
                .await
        };
        if requires_action
            && !token.is_cancelled()
            && !result.as_ref().is_err_and(|e| e.code == -32800)
        {
            let guard = self.wire.lock().await;
            if let Some(sid) = sid
                .as_deref()
                .filter(|sid| guard.foreground && guard.active.as_deref() == Some(*sid))
            {
                self.update(sid, wire::state("running", None)).await?;
            }
        }
        result
    }

    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        let mut wire = self.wire.lock().await;
        if !matches!(wire.version, Version::Two) {
            return self.inner.send_notification(method, params).await;
        }
        if method == "$/cancel_request" {
            return self.inner.send_notification(method, params).await;
        }
        let Some(sid) = params.get("sessionId").and_then(Value::as_str) else {
            return Ok(());
        };
        if wire.active.as_deref() != Some(sid)
            && !wire
                .replay
                .as_ref()
                .is_some_and(|replay| replay.session == sid)
        {
            return Ok(());
        }
        match method {
            "session/update" => {
                let Some(update) = params.get("update") else {
                    return Ok(());
                };
                let replay = wire.replay.as_mut().filter(|replay| replay.session == sid);
                let reset = if let Some(replay) = replay {
                    match (
                        update.get("sessionUpdate").and_then(Value::as_str),
                        update.get("messageId").and_then(Value::as_str),
                    ) {
                        (Some(kind), Some(id)) if kind.ends_with("_chunk") => {
                            replay.messages.insert((kind.into(), id.into()))
                        }
                        _ => false,
                    }
                } else {
                    false
                };
                if let Some(updates) = wire::project_update(update.clone(), reset) {
                    for update in updates {
                        self.update(sid, update).await?;
                    }
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
                match event {
                    crate::event::AcpEvent::UserInputRunStarted { .. }
                    | crate::event::AcpEvent::ExecutionStarted { .. }
                        if !wire.foreground =>
                    {
                        wire.foreground = true;
                        wire.execution_failed = false;
                        self.update(sid, wire::state("running", None)).await?;
                    }
                    crate::event::AcpEvent::UserInputDelivered { input_id, .. } => {
                        if let Some((message, waiters)) = wire.prompts.accepted(sid, &input_id) {
                            if !wire.foreground {
                                wire.foreground = true;
                                self.update(sid, wire::state("running", None)).await?;
                            }
                            self.update(sid, message).await?;
                            for id in waiters {
                                self.inner
                                    .send_response(id, Ok(json!({"messageId":input_id})))
                                    .await?;
                            }
                        }
                    }
                    _ => {}
                }
            }
            "peri/agent_event_done" => {
                let reason = params
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .unwrap_or("error");
                let reason = if std::mem::take(&mut wire.execution_failed) && reason != "cancelled"
                {
                    "error"
                } else {
                    reason
                };
                if wire.foreground {
                    wire.foreground = false;
                    self.update(sid, wire::state("idle", Some(reason))).await?;
                }
                // A turn ending is not evidence that a separately queued
                // prompt was cancelled. Only a committed withdrawal settles it.
            }
            _ => {}
        }
        Ok(())
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        loop {
            if let Some(message) = self.inbound(self.inner.recv().await?).await {
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
        let result = match wire.replies.remove(&id) {
            Some(Reply::Initialize) => result.map(|_| {
                wire.version = Version::Two;
                // v1 stdio keeps its historical bypass mode. The negotiated
                // preview uses the existing Peri approval policy and broker.
                self.permissions.store(PermissionMode::Default);
                wire::initialize_response()
            }),
            Some(Reply::Input(input)) => {
                if let Err(error) = result {
                    for waiter in wire.prompts.reject(&input) {
                        self.inner.send_response(waiter, Err(error.clone())).await?;
                    }
                }
                return Ok(());
            }
            Some(Reply::Cancel) => {
                if let Some(inputs) = result.as_ref().ok().and_then(Value::as_array) {
                    for input in inputs.iter().filter_map(Value::as_str) {
                        for waiter in wire.prompts.reject(input) {
                            self.inner.send_response(waiter, Err(cancelled())).await?;
                        }
                    }
                }
                return Ok(());
            }
            Some(Reply::Withdraw(input)) => {
                if result.is_ok() {
                    for waiter in wire.prompts.reject(&input) {
                        self.inner.send_response(waiter, Err(cancelled())).await?;
                    }
                }
                return Ok(());
            }
            Some(Reply::Setup { session, replay }) => {
                if replay {
                    wire.replay = None;
                }
                if let Ok(value) = &result {
                    let sid = session
                        .as_deref()
                        .or_else(|| value.get("sessionId").and_then(Value::as_str));
                    if let Some(sid) = sid {
                        if replay {
                            self.update(sid, json!({"sessionUpdate":"replay_complete"}))
                                .await?;
                        }
                        if session.is_some() {
                            self.update(
                                sid,
                                wire::state(if wire.foreground { "running" } else { "idle" }, None),
                            )
                            .await?;
                        }
                        wire.active = Some(sid.to_owned());
                    }
                }
                result.map(|value| {
                    wire::lifecycle_result(
                        if session.is_some() {
                            "session/resume"
                        } else {
                            "session/new"
                        },
                        value,
                    )
                })
            }
            Some(Reply::Close(sid)) if result.is_ok() => {
                if wire.foreground {
                    wire.foreground = false;
                    self.update(&sid, wire::state("idle", Some("cancelled")))
                        .await?;
                }
                for waiter in wire.prompts.reject_session(&sid) {
                    self.inner.send_response(waiter, Err(cancelled())).await?;
                }
                wire.active = None;
                Ok(json!({}))
            }
            _ => result,
        };
        self.inner.send_response(id, result).await
    }
}

fn cancelled() -> AcpError {
    AcpError::new(-32800, "Prompt cancelled before insertion")
}
