//! HTTP client for the Peri Swarm gateway.
//!
//! The dashboard talks to the gateway over the documented `/v1/agents/sessions`
//! contract. DTOs here are local projections: the gateway crate is intentionally
//! not a dependency. See `peri-swarm/src/domain/*.rs` for the source of truth.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, RequestBuilder, Response};
use serde::de::DeserializeOwned;
use serde_json::{json, Map, Value};
use url::Url;

use crate::config::{self, Config};

/// Gateway contract header carrying the contract version.
pub const CONTRACT_HEADER: &str = "x-peri-swarm-contract";
/// Gateway header that opts into the strict contract.
pub const EXPECT_HEADER: &str = "x-peri-swarm-expect";
/// Contract version this client speaks.
pub const CONTRACT_VERSION: &str = "1";
/// Header that makes write retries idempotent.
pub const IDEMPOTENCY_HEADER: &str = "Idempotency-Key";
/// Every request is bounded; polling adds its own backoff.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Largest page the gateway accepts.
pub const MAX_PAGE_LIMIT: u32 = 500;

// DTOs live in a submodule to keep this file within the repository size budget.
pub mod dto;
pub use dto::*;

// ---------------------------------------------------------------------------
// Idempotency and request bodies
// ---------------------------------------------------------------------------

/// A submission-scoped `Idempotency-Key`.
///
/// Retrying a write must reuse the same value; construct one key per user
/// submission and clone it for retries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// Generate a UUID v7 key.
    #[must_use]
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7().to_string())
    }

    /// Header value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for IdempotencyKey {
    fn default() -> Self {
        Self::new()
    }
}

/// Fields accepted by the create form.
#[derive(Clone, Debug, Default)]
pub struct CreateSessionSpec {
    /// Optional caller-supplied logical id.
    pub logical_id: Option<String>,
    /// Optional working directory.
    pub cwd: Option<String>,
    /// Optional model; omitted lets the gateway use its default.
    pub model: Option<String>,
    /// Optional instructions.
    pub instructions: Option<String>,
    /// Metadata rows; an empty map is omitted.
    pub metadata: BTreeMap<String, String>,
    /// Advanced JSON tools array.
    pub tools: Option<Value>,
    /// Advanced JSON environment object.
    pub environment: Option<Value>,
    /// Advanced JSON prompt cache options.
    pub prompt_cache_options: Option<Value>,
}

/// Fields accepted by the edit form.
///
/// `instructions` is absent on purpose: the gateway's `SessionUpdateParams`
/// accepts `agent.model`, `metadata` and `prompt_cache_options` only, with
/// `deny_unknown_fields`. Sending a top-level `instructions` would be rejected
/// as `400 unknown_field`.
#[derive(Clone, Debug, Default)]
pub struct UpdateSessionSpec {
    /// Replacement model; blank is rejected locally.
    pub model: Option<String>,
    /// `Some(map)` replaces metadata (`Some(empty)` clears it); `None` keeps it.
    pub metadata: Option<BTreeMap<String, String>>,
    /// `None` keeps the value, `Some(Value::Null)` clears it, `Some(object)`
    /// replaces it.
    pub prompt_cache_options: Option<Value>,
}

/// Build the create body documented in the gateway contract.
#[must_use]
pub fn create_session_body(spec: &CreateSessionSpec) -> Value {
    let mut body = Map::new();
    insert_text(&mut body, "logicalId", spec.logical_id.as_deref());
    insert_text(&mut body, "cwd", spec.cwd.as_deref());
    insert_text(&mut body, "model", spec.model.as_deref());
    insert_text(&mut body, "instructions", spec.instructions.as_deref());
    if !spec.metadata.is_empty() {
        body.insert("metadata".to_owned(), json!(spec.metadata));
    }
    if let Some(tools) = &spec.tools {
        body.insert("tools".to_owned(), tools.clone());
    }
    if let Some(environment) = &spec.environment {
        body.insert("environment".to_owned(), environment.clone());
    }
    if let Some(options) = &spec.prompt_cache_options {
        body.insert("prompt_cache_options".to_owned(), options.clone());
    }
    body.insert("stream".to_owned(), Value::Bool(false));
    Value::Object(body)
}

/// Build the update body accepted by the gateway.
///
/// The gateway nests the model under `agent`; see [`UpdateSessionSpec`].
pub fn update_session_body(spec: &UpdateSessionSpec) -> Result<Value, ApiError> {
    let mut body = Map::new();
    if let Some(model) = &spec.model {
        if model.trim().is_empty() {
            return Err(ApiError::local("model must not be empty"));
        }
        body.insert("agent".to_owned(), json!({ "model": model }));
    }
    if let Some(metadata) = &spec.metadata {
        body.insert("metadata".to_owned(), json!(metadata));
    }
    if let Some(options) = &spec.prompt_cache_options {
        body.insert("prompt_cache_options".to_owned(), options.clone());
    }
    Ok(Value::Object(body))
}

fn insert_text(body: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(text) = value {
        if !text.trim().is_empty() {
            body.insert(key.to_owned(), Value::String(text.to_owned()));
        }
    }
}

/// The official message input event.
#[must_use]
pub fn message_event_body(text: &str) -> Value {
    json!({
        "events": [{
            "type": "agent.session.input.message",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": text }]
            }]
        }]
    })
}

/// The official cancel input event.
#[must_use]
pub fn cancel_event_body() -> Value {
    json!({ "events": [{ "type": "agent.session.input.cancel" }] })
}

/// Approval decision submitted for one pending request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalDecision {
    /// Run the tool.
    Approve,
    /// Reject the tool call.
    Deny,
}

impl ApprovalDecision {
    /// Official decision spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Deny => "deny",
        }
    }
}

/// The official MCP approval answer event.
#[must_use]
pub fn approval_event_body(request_id: &str, decision: ApprovalDecision) -> Value {
    json!({
        "events": [{
            "type": "agent.session.input.mcp_approval_request_result",
            "request_id": request_id,
            "response": {
                "type": "mcp_tool_call",
                "decision": decision.as_str()
            }
        }]
    })
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Stable error classes the UI can branch on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApiErrorKind {
    /// 400/405/413/415 or a locally rejected body.
    BadRequest,
    /// 401: pause polling and re-authenticate.
    Unauthorized,
    /// 404: the session no longer exists.
    NotFound,
    /// 409: identity or turn conflict.
    Conflict,
    /// 422: validation failure.
    Validation,
    /// 502: the gateway could not reach its host.
    BadGateway,
    /// Any other 5xx.
    Server,
    /// A 3xx response; redirects are intentionally not followed.
    Redirect,
    /// Transport failure or timeout.
    Network,
    /// A 2xx response whose body did not match the DTO.
    Decode,
}

/// Which step of a transport or decoding failure failed.
///
/// The client records this cause and the raw detail only; the user-facing
/// sentence is produced by [`crate::ui::text::api_error`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApiErrorCause {
    /// The HTTP client could not be constructed.
    ClientBuild,
    /// The request could not be sent (DNS, connect or timeout).
    Request,
    /// The response body could not be read.
    ResponseRead,
    /// A success body did not match the expected DTO.
    ResponseDecode,
}

/// One gateway failure with a code/message and no request headers or key.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct ApiError {
    kind: ApiErrorKind,
    status: Option<u16>,
    code: Option<String>,
    message: String,
    details: Box<Value>,
    cause: Option<ApiErrorCause>,
}

impl ApiError {
    fn new(kind: ApiErrorKind, status: Option<u16>, message: impl Into<String>) -> Self {
        Self {
            kind,
            status,
            code: None,
            message: message.into(),
            details: Box::new(Value::Null),
            cause: None,
        }
    }

    /// A body rejected before sending.
    #[must_use]
    pub fn local(message: impl Into<String>) -> Self {
        Self::new(ApiErrorKind::BadRequest, None, message)
    }

    /// A transport or decoding failure with its structured cause and the raw
    /// detail, which never includes request headers, the body or the API key.
    fn failed(cause: ApiErrorCause, detail: impl Into<String>) -> Self {
        let kind = match cause {
            ApiErrorCause::ResponseDecode => ApiErrorKind::Decode,
            ApiErrorCause::ClientBuild | ApiErrorCause::Request | ApiErrorCause::ResponseRead => {
                ApiErrorKind::Network
            }
        };
        Self {
            kind,
            status: None,
            code: None,
            message: detail.into(),
            details: Box::new(Value::Null),
            cause: Some(cause),
        }
    }

    /// Map a non-success status plus its body to a structured error.
    ///
    /// Non-JSON bodies are summarized by status; the body is never echoed.
    pub fn from_status(status: u16, body: &[u8]) -> Self {
        let envelope = serde_json::from_slice::<ErrorEnvelope>(body).ok();
        let (code, message, details) = match envelope {
            Some(envelope) => (
                Some(envelope.error.code),
                envelope.error.message,
                Box::new(envelope.error.details),
            ),
            None => (
                None,
                default_message(status).to_owned(),
                Box::new(Value::Null),
            ),
        };
        let kind = match status {
            400 | 405 | 413 | 415 => ApiErrorKind::BadRequest,
            401 => ApiErrorKind::Unauthorized,
            404 => ApiErrorKind::NotFound,
            409 => ApiErrorKind::Conflict,
            422 => ApiErrorKind::Validation,
            502 => ApiErrorKind::BadGateway,
            300..=399 => ApiErrorKind::Redirect,
            500..=599 => ApiErrorKind::Server,
            _ => ApiErrorKind::BadRequest,
        };
        Self {
            kind,
            status: Some(status),
            code,
            message,
            details,
            cause: None,
        }
    }

    /// Error class.
    #[must_use]
    pub fn kind(&self) -> ApiErrorKind {
        self.kind
    }

    /// HTTP status, or `None` for local/transport failures.
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        self.status
    }

    /// Gateway code, when the body carried an error envelope.
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    /// Raw detail without headers, the body or the key.
    ///
    /// Transport and decoding failures store only the underlying detail here;
    /// the user-facing sentence comes from [`crate::ui::text::api_error`].
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Gateway details, when present.
    #[must_use]
    pub fn details(&self) -> &Value {
        &self.details
    }

    /// Structured cause of a transport or decoding failure.
    ///
    /// `None` for gateway status failures and locally rejected bodies.
    #[must_use]
    pub fn cause(&self) -> Option<ApiErrorCause> {
        self.cause
    }

    /// Whether a later poll may succeed without user action.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self.kind,
            ApiErrorKind::Network | ApiErrorKind::Server | ApiErrorKind::BadGateway
        )
    }

    /// Whether this error must pause polling until re-authentication.
    #[must_use]
    pub fn is_unauthorized(&self) -> bool {
        self.kind == ApiErrorKind::Unauthorized
    }

    /// Whether the session is gone.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        self.kind == ApiErrorKind::NotFound
    }

    /// Whether the request conflicted with a running turn or identity.
    #[must_use]
    pub fn is_conflict(&self) -> bool {
        self.kind == ApiErrorKind::Conflict
    }

    /// Whether the gateway host was unreachable.
    #[must_use]
    pub fn is_bad_gateway(&self) -> bool {
        self.kind == ApiErrorKind::BadGateway
    }
}

fn default_message(status: u16) -> &'static str {
    crate::ui::text::api_default_message(status)
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Operations the dashboard needs from the gateway.
#[async_trait]
pub trait SwarmApi: Send + Sync {
    /// Liveness probe; no bearer is required.
    async fn health(&self) -> Result<Health, ApiError>;

    /// List sessions in gateway order (`logical_id` desc).
    async fn list_sessions(&self, limit: u32) -> Result<SessionPage, ApiError>;

    /// Retrieve one session.
    async fn get_session(&self, session_id: &str) -> Result<Session, ApiError>;

    /// List turns for one session.
    async fn list_turns(
        &self,
        session_id: &str,
        limit: u32,
        after: Option<&str>,
    ) -> Result<Turns, ApiError>;

    /// List items for one session.
    async fn list_items(
        &self,
        session_id: &str,
        limit: u32,
        after: Option<&str>,
    ) -> Result<ItemsPage, ApiError>;

    /// Create or attach a session.
    async fn create_session(
        &self,
        spec: &CreateSessionSpec,
        key: &IdempotencyKey,
    ) -> Result<CreatedSession, ApiError>;

    /// Update an existing session.
    async fn update_session(
        &self,
        session_id: &str,
        spec: &UpdateSessionSpec,
        key: &IdempotencyKey,
    ) -> Result<Session, ApiError>;

    /// Delete a session.
    async fn delete_session(&self, session_id: &str, key: &IdempotencyKey) -> Result<(), ApiError>;

    /// Submit a user message.
    async fn send_input(
        &self,
        session_id: &str,
        text: &str,
        key: &IdempotencyKey,
    ) -> Result<InputReceipt, ApiError>;

    /// Cancel the active turn.
    async fn cancel(
        &self,
        session_id: &str,
        key: &IdempotencyKey,
    ) -> Result<InputReceipt, ApiError>;

    /// Answer one pending MCP approval.
    async fn approve(
        &self,
        session_id: &str,
        request_id: &str,
        decision: ApprovalDecision,
        key: &IdempotencyKey,
    ) -> Result<ApprovalReceipt, ApiError>;

    /// Last `x-peri-swarm-contract` value observed, if any.
    fn contract_version(&self) -> Option<String>;
}

/// Reqwest-backed client.
pub struct HttpSwarmApi {
    base_url: Url,
    api_key: String,
    client: Client,
    contract: Mutex<Option<String>>,
}

impl fmt::Debug for HttpSwarmApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpSwarmApi")
            .field("base_url", &self.base_url.as_str())
            .field("api_key", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl HttpSwarmApi {
    /// Build a client from already validated credentials.
    pub fn from_config(config: &Config) -> Result<Self, ApiError> {
        Self::new(config.base_url(), config.api_key())
    }

    /// Build a client, normalizing the base URL the same way the config does.
    pub fn new(base_url: &str, api_key: &str) -> Result<Self, ApiError> {
        let normalized = config::normalize_base_url(base_url)
            .map_err(|error| ApiError::local(format!("base URL is invalid: {error}")))?;
        let base_url = Url::parse(&normalized)
            .map_err(|error| ApiError::local(format!("base URL is invalid: {error}")))?;
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            // Authorization must never leave the configured origin.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| ApiError::failed(ApiErrorCause::ClientBuild, error.to_string()))?;
        Ok(Self {
            base_url,
            api_key: api_key.to_owned(),
            client,
            contract: Mutex::new(None),
        })
    }

    fn api_url(&self, tail: &[&str]) -> Result<Url, ApiError> {
        let mut url = self.base_url.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| ApiError::local("base URL cannot be used as an API request base"))?;
            segments.clear();
            segments.push("v1").push("agents").push("sessions");
            for segment in tail {
                segments.push(segment);
            }
        }
        Ok(url)
    }

    fn page_url(&self, tail: &[&str], limit: u32, after: Option<&str>) -> Result<Url, ApiError> {
        if limit == 0 || limit > MAX_PAGE_LIMIT {
            return Err(ApiError::local(format!(
                "limit must be within 1..={MAX_PAGE_LIMIT}"
            )));
        }
        let mut url = self.api_url(tail)?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("limit", &limit.to_string());
            query.append_pair("order", "desc");
            if let Some(after) = after.filter(|value| !value.is_empty()) {
                query.append_pair("after", after);
            }
        }
        Ok(url)
    }

    fn authorized(&self, builder: RequestBuilder) -> RequestBuilder {
        builder
            .bearer_auth(&self.api_key)
            .header(EXPECT_HEADER, CONTRACT_VERSION)
    }

    async fn send_json<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T, ApiError> {
        let response = request.send().await.map_err(network_error)?;
        self.decode(response).await
    }

    async fn send_write<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        body: &Value,
        key: &IdempotencyKey,
    ) -> Result<T, ApiError> {
        let response = request
            .header(IDEMPOTENCY_HEADER, key.as_str())
            .json(body)
            .send()
            .await
            .map_err(network_error)?;
        self.decode(response).await
    }

    async fn send_empty(
        &self,
        request: RequestBuilder,
        key: &IdempotencyKey,
    ) -> Result<(), ApiError> {
        let response = request
            .header(IDEMPOTENCY_HEADER, key.as_str())
            .send()
            .await
            .map_err(network_error)?;
        let response = self.read_response(response).await?;
        match response {
            ResponseBody::Success(_) => Ok(()),
            ResponseBody::Failure(error) => Err(error),
        }
    }

    async fn decode<T: DeserializeOwned>(&self, response: Response) -> Result<T, ApiError> {
        match self.read_response(response).await? {
            ResponseBody::Success(body) => serde_json::from_slice(&body).map_err(|error| {
                ApiError::failed(ApiErrorCause::ResponseDecode, error.to_string())
            }),
            ResponseBody::Failure(error) => Err(error),
        }
    }

    async fn read_response(&self, response: Response) -> Result<ResponseBody, ApiError> {
        let status = response.status();
        if let Some(contract) = response
            .headers()
            .get(CONTRACT_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
        {
            self.record_contract(contract);
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| ApiError::failed(ApiErrorCause::ResponseRead, error.to_string()))?;
        if status.is_success() {
            Ok(ResponseBody::Success(body.to_vec()))
        } else {
            Ok(ResponseBody::Failure(ApiError::from_status(
                status.as_u16(),
                &body,
            )))
        }
    }

    fn record_contract(&self, value: String) {
        match self.contract.lock() {
            Ok(mut slot) => *slot = Some(value),
            Err(poisoned) => *poisoned.into_inner() = Some(value),
        }
    }
}

enum ResponseBody {
    Success(Vec<u8>),
    Failure(ApiError),
}

fn network_error(error: reqwest::Error) -> ApiError {
    ApiError::failed(ApiErrorCause::Request, error.to_string())
}

#[async_trait]
impl SwarmApi for HttpSwarmApi {
    async fn health(&self) -> Result<Health, ApiError> {
        let url = self
            .base_url
            .join("/healthz")
            .map_err(|error| ApiError::local(format!("failed to build health URL: {error}")))?;
        let response = self
            .client
            .get(url)
            // Health is outside the bearer gate; do not send the key there.
            .header(EXPECT_HEADER, CONTRACT_VERSION)
            .send()
            .await
            .map_err(network_error)?;
        self.decode(response).await
    }

    async fn list_sessions(&self, limit: u32) -> Result<SessionPage, ApiError> {
        let url = self.page_url(&[], limit, None)?;
        self.send_json(self.authorized(self.client.get(url))).await
    }

    async fn get_session(&self, session_id: &str) -> Result<Session, ApiError> {
        let url = self.api_url(&[session_id])?;
        self.send_json(self.authorized(self.client.get(url))).await
    }

    async fn list_turns(
        &self,
        session_id: &str,
        limit: u32,
        after: Option<&str>,
    ) -> Result<Turns, ApiError> {
        let url = self.page_url(&[session_id, "turns"], limit, after)?;
        self.send_json(self.authorized(self.client.get(url))).await
    }

    async fn list_items(
        &self,
        session_id: &str,
        limit: u32,
        after: Option<&str>,
    ) -> Result<ItemsPage, ApiError> {
        let url = self.page_url(&[session_id, "items"], limit, after)?;
        self.send_json(self.authorized(self.client.get(url))).await
    }

    async fn create_session(
        &self,
        spec: &CreateSessionSpec,
        key: &IdempotencyKey,
    ) -> Result<CreatedSession, ApiError> {
        let url = self.api_url(&[])?;
        let body = create_session_body(spec);
        self.send_write(self.authorized(self.client.post(url)), &body, key)
            .await
    }

    async fn update_session(
        &self,
        session_id: &str,
        spec: &UpdateSessionSpec,
        key: &IdempotencyKey,
    ) -> Result<Session, ApiError> {
        let body = update_session_body(spec)?;
        let url = self.api_url(&[session_id])?;
        self.send_write(self.authorized(self.client.post(url)), &body, key)
            .await
    }

    async fn delete_session(&self, session_id: &str, key: &IdempotencyKey) -> Result<(), ApiError> {
        let url = self.api_url(&[session_id])?;
        self.send_empty(self.authorized(self.client.delete(url)), key)
            .await
    }

    async fn send_input(
        &self,
        session_id: &str,
        text: &str,
        key: &IdempotencyKey,
    ) -> Result<InputReceipt, ApiError> {
        let url = self.api_url(&[session_id, "events"])?;
        let body = message_event_body(text);
        self.send_write(self.authorized(self.client.post(url)), &body, key)
            .await
    }

    async fn cancel(
        &self,
        session_id: &str,
        key: &IdempotencyKey,
    ) -> Result<InputReceipt, ApiError> {
        let url = self.api_url(&[session_id, "events"])?;
        let body = cancel_event_body();
        self.send_write(self.authorized(self.client.post(url)), &body, key)
            .await
    }

    async fn approve(
        &self,
        session_id: &str,
        request_id: &str,
        decision: ApprovalDecision,
        key: &IdempotencyKey,
    ) -> Result<ApprovalReceipt, ApiError> {
        let url = self.api_url(&[session_id, "events"])?;
        let body = approval_event_body(request_id, decision);
        self.send_write(self.authorized(self.client.post(url)), &body, key)
            .await
    }

    fn contract_version(&self) -> Option<String> {
        match self.contract.lock() {
            Ok(slot) => slot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

#[cfg(test)]
#[path = "api_test.rs"]
mod tests;
