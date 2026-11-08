//! Immutable model-call attribution. Tool scopes carry diagnostics into child construction only.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use peri_model::{
    Model, ModelCallContext, ModelCallParent, ModelCapabilities, ModelRequest, ModelResponse,
    ModelResult, ModelStream, PreparedModelCall, PreparedModelRequest,
};
use tokio_util::sync::CancellationToken;

tokio::task_local! {
    static TOOL_MODEL_PARENT: Option<ModelCallParent>;
}

pub(crate) async fn scope_tool_parent<F: Future>(
    parent: Option<ModelCallParent>,
    future: F,
) -> F::Output {
    TOOL_MODEL_PARENT.scope(parent, future).await
}

/// Capture before handing child execution to another task; task-local state is not inherited.
pub(crate) fn capture_tool_parent(tool_call: Option<&str>) -> Option<ModelCallParent> {
    TOOL_MODEL_PARENT
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .filter(|parent| Some(parent.tool_call.as_str()) == tool_call)
}

/// Identity for one auxiliary request made by the currently executing canonical tool.
/// Missing scope stays missing; callers must not infer an owner from provider state.
pub fn tool_model_call_context(purpose: &str) -> Option<ModelCallContext> {
    let parent = TOOL_MODEL_PARENT.try_with(Clone::clone).ok().flatten()?;
    Some(ModelCallContext {
        owner: parent.owner.clone(),
        execution: parent.execution.clone(),
        call: format!("{purpose}:{}:{}", parent.call, parent.tool_call),
        purpose: purpose.to_owned(),
        // This request belongs to the already running execution, not a newly spawned child.
        parent: None,
    })
}

/// One auxiliary operation may make several logical requests (for example summary continuation).
/// Its counter is operation-local; shared provider instances never hold mutable call identity.
pub(crate) struct ScopedModel<'a> {
    inner: &'a dyn Model,
    context: Option<ModelCallContext>,
    next_call: AtomicU64,
}

impl<'a> ScopedModel<'a> {
    pub(crate) fn new(inner: &'a dyn Model, context: Option<ModelCallContext>) -> Self {
        Self {
            inner,
            context,
            next_call: AtomicU64::new(0),
        }
    }

    fn bind_request(&self, mut request: ModelRequest) -> ModelRequest {
        if let Some(mut context) = self.context.clone() {
            let index = self.next_call.fetch_add(1, Ordering::Relaxed);
            context.call = format!("{}:{index}", context.call);
            request.call_context = Some(context);
        }
        request
    }
}

#[async_trait]
impl Model for ScopedModel<'_> {
    fn capabilities(&self) -> ModelCapabilities {
        self.inner.capabilities()
    }

    fn output_token_limit(&self) -> Option<u32> {
        self.inner.output_token_limit()
    }

    fn context_window(&self) -> Option<u32> {
        self.inner.context_window()
    }

    fn prepare_request(&self, request: &ModelRequest) -> ModelResult<PreparedModelRequest> {
        self.inner.prepare_request(request)
    }

    fn prepare_stream(&self, request: ModelRequest) -> ModelResult<PreparedModelCall> {
        self.inner.prepare_stream(self.bind_request(request))
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        self.inner
            .stream(self.bind_request(request), cancellation)
            .await
    }

    async fn complete(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelResponse> {
        // Preserve provider overrides; the default aggregate-via-stream implementation is
        // only appropriate when the wrapped provider itself selects it.
        self.inner
            .complete(self.bind_request(request), cancellation)
            .await
    }
}

#[cfg(test)]
#[path = "model_call_test.rs"]
mod tests;
