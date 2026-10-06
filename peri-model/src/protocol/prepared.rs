use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{ModelError, ModelResult, ModelStream, ProtocolErrorKind};

pub struct PreparedModelCall {
    checkpoint: Value,
    start: Box<dyn FnOnce(CancellationToken) -> ModelResult<ModelStream> + Send>,
}

impl PreparedModelCall {
    /// Freezes a trusted provider's exact request and its one-shot send operation.
    ///
    /// Preparation must not perform network or other execution effects. The
    /// checkpoint must contain the complete wire body, including all messages,
    /// dynamic system contributions and tool schemas, without redaction or
    /// truncation. Model, provider endpoint and credential-grant references must
    /// be frozen too; plaintext credentials and authentication headers must not
    /// be included. The send closure must consume that same frozen request,
    /// rather than rebuild it from mutable configuration or prompt sources.
    ///
    /// A cancelled token prevents invocation of the closure. The closure and
    /// returned stream must also honor subsequent cancellation before effects.
    ///
    /// ```
    /// use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    /// use peri_model::{ModelError, PreparedModelCall};
    /// use tokio_util::sync::CancellationToken;
    ///
    /// let sends = Arc::new(AtomicUsize::new(0));
    /// let observed = sends.clone();
    /// let checkpoint = serde_json::json!({
    ///     "provider": "trusted-provider",
    ///     "model": "frozen-model",
    ///     "endpoint": "https://provider.example/messages",
    ///     "credentialRef": "configured-provider:trusted-provider",
    ///     "body": {"messages": [{"role": "user", "content": "complete input"}]}
    /// });
    /// let prepared = PreparedModelCall::new(checkpoint.clone(), move |_| {
    ///     observed.fetch_add(1, Ordering::SeqCst);
    ///     Err(ModelError::cancelled())
    /// });
    /// assert_eq!(prepared.checkpoint(), &checkpoint);
    /// assert_eq!(sends.load(Ordering::SeqCst), 0);
    /// let cancellation = CancellationToken::new();
    /// cancellation.cancel();
    /// assert!(prepared.start(cancellation).is_err());
    /// assert_eq!(sends.load(Ordering::SeqCst), 0);
    /// ```
    pub fn new(
        checkpoint: Value,
        start: impl FnOnce(CancellationToken) -> ModelResult<ModelStream> + Send + 'static,
    ) -> Self {
        Self {
            checkpoint,
            start: Box::new(start),
        }
    }

    /// Returns the complete frozen wire checkpoint, not a safe telemetry view.
    ///
    /// Do not redact or truncate this durable checkpoint, or send a different
    /// body. It may contain sensitive prompt content and must not be logged as
    /// a diagnostic snapshot.
    pub fn checkpoint(&self) -> &Value {
        &self.checkpoint
    }

    pub fn start(self, cancellation: CancellationToken) -> ModelResult<ModelStream> {
        if cancellation.is_cancelled() {
            return Err(ModelError::cancelled());
        }
        (self.start)(cancellation)
    }
}

pub(crate) fn checkpoint(provider: &str, endpoint: &url::Url, body: &Value) -> ModelResult<Value> {
    if !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
    {
        return Err(ModelError::protocol(ProtocolErrorKind::Provider));
    }
    Ok(serde_json::json!({
        "provider": provider,
        "endpoint": endpoint.as_str(),
        "model": body.get("model"),
        "credentialRef": format!("configured-provider:{provider}"),
        "body": body,
    }))
}
