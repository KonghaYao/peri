use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use crate::{ModelError, ModelResult, PreparedModelRequest, ProtocolErrorKind};
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;

use super::{JsonObject, ModelCapabilities, ModelRequest, ModelResponse, TokenUsage, ToolCall};

/// 流式模型输出的标准事件。
#[derive(Debug, Clone, PartialEq)]
pub enum ModelStreamEvent {
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: String,
    },
    Usage(TokenUsage),
    Completed(ModelResponse),
}

#[derive(Default)]
struct PendingToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

fn complete_tool_calls(tool_calls: BTreeMap<usize, PendingToolCall>) -> ModelResult<Vec<ToolCall>> {
    tool_calls
        .into_values()
        .map(|tool_call| {
            let id = tool_call
                .id
                .ok_or_else(|| ModelError::protocol(ProtocolErrorKind::ToolCallMissingId))?;
            let name = tool_call
                .name
                .ok_or_else(|| ModelError::protocol(ProtocolErrorKind::ToolCallMissingName))?;
            let value = serde_json::from_str(&tool_call.arguments)
                .map_err(|_| ModelError::protocol(ProtocolErrorKind::ToolCallInvalidArguments))?;
            let arguments = JsonObject::from_value(value)?;
            Ok(ToolCall::new(id, name, arguments))
        })
        .collect()
}

/// 可消费的模型输出流，并持有其内部取消作用域。
pub struct ModelStream {
    events: Pin<Box<dyn Stream<Item = ModelResult<ModelStreamEvent>> + Send>>,
    child_token: CancellationToken,
    child_cancelled: Pin<Box<dyn Future<Output = ()> + Send>>,
    cancellation_reported: bool,
    completed: bool,
}

#[allow(dead_code)]
impl ModelStream {
    pub fn new<S>(events: S) -> Self
    where
        S: Stream<Item = ModelResult<ModelStreamEvent>> + Send + 'static,
    {
        Self::with_child_token(events, CancellationToken::new())
    }

    /// 创建一个由 `parent_cancellation` 取消的流。
    ///
    /// 流持有的是 parent 的 child token，因此 [`ModelStream::abort`] 只会取消本流，
    /// 不会反向取消调用方或其他同级操作。
    pub fn with_parent_cancellation<S>(events: S, parent_cancellation: CancellationToken) -> Self
    where
        S: Stream<Item = ModelResult<ModelStreamEvent>> + Send + 'static,
    {
        Self::with_child_token(events, parent_cancellation.child_token())
    }

    /// 创建一个由外部取消 token 驱动的流。调用方必须传入调用方 token 的 child，
    /// 以确保 abort 不会反向取消父 token。
    pub(crate) fn with_cancellation<S>(events: S, cancellation: CancellationToken) -> Self
    where
        S: Stream<Item = ModelResult<ModelStreamEvent>> + Send + 'static,
    {
        Self::with_child_token(events, cancellation)
    }

    fn with_child_token<S>(events: S, child_token: CancellationToken) -> Self
    where
        S: Stream<Item = ModelResult<ModelStreamEvent>> + Send + 'static,
    {
        let child_cancelled = child_token.clone();
        Self {
            events: Box::pin(events),
            child_token,
            child_cancelled: Box::pin(async move { child_cancelled.cancelled().await }),
            cancellation_reported: false,
            completed: false,
        }
    }

    /// 触发本流的取消。Task 3 会将其连接到在途 transport。
    pub fn abort(&self) {
        self.child_token.cancel();
    }

    fn cancellation_token(&self) -> CancellationToken {
        self.child_token.clone()
    }
}

impl Stream for ModelStream {
    type Item = ModelResult<ModelStreamEvent>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.cancellation_reported {
            return Poll::Ready(None);
        }
        if this.child_cancelled.as_mut().poll(cx).is_ready() {
            this.cancellation_reported = true;
            return Poll::Ready(Some(Err(ModelError::cancelled())));
        }
        match this.events.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(ModelStreamEvent::Completed(response)))) => {
                this.completed = true;
                Poll::Ready(Some(Ok(ModelStreamEvent::Completed(response))))
            }
            result => result,
        }
    }
}

impl Drop for ModelStream {
    fn drop(&mut self) {
        if !self.completed {
            self.child_token.cancel();
        }
    }
}

/// 流式优先的模型接口。
#[async_trait]
pub trait Model: Send + Sync {
    fn capabilities(&self) -> ModelCapabilities;

    /// 构造可安全用于观测的 provider 请求投影。
    ///
    /// Provider 必须覆盖此方法；默认实现明确拒绝未实现的协议请求构造。
    fn prepare_request(&self, _request: &ModelRequest) -> ModelResult<PreparedModelRequest> {
        Err(ModelError::protocol(ProtocolErrorKind::Provider))
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelStream>;

    /// 仅聚合 [`Model::stream`] 产生的事件，不存在独立非流式调用路径。
    async fn complete(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelResponse> {
        if cancellation.is_cancelled() {
            return Err(ModelError::cancelled());
        }
        let mut stream = self.stream(request, cancellation.clone()).await?;
        let stream_cancellation = stream.cancellation_token();
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_calls = BTreeMap::<usize, PendingToolCall>::new();
        let mut usage = None;

        loop {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    stream.abort();
                    return Err(ModelError::cancelled());
                }
                _ = stream_cancellation.cancelled() => return Err(ModelError::cancelled()),
                event = stream.next() => match event {
                    Some(Ok(ModelStreamEvent::TextDelta { text: delta })) => text.push_str(&delta),
                    Some(Ok(ModelStreamEvent::ReasoningDelta { text: delta })) => reasoning.push_str(&delta),
                    Some(Ok(ModelStreamEvent::ToolCallDelta {
                        index,
                        id,
                        name,
                        arguments_delta,
                    })) => {
                        let tool_call = tool_calls.entry(index).or_default();
                        if id.is_some() {
                            tool_call.id = id;
                        }
                        if name.is_some() {
                            tool_call.name = name;
                        }
                        tool_call.arguments.push_str(&arguments_delta);
                    }
                    Some(Ok(ModelStreamEvent::Usage(event_usage))) => usage = Some(event_usage),
                    Some(Ok(ModelStreamEvent::Completed(mut response))) => {
                        response.set_text_if_empty(text);
                        response.set_reasoning_if_empty(reasoning);
                        response.set_tool_calls_if_empty(complete_tool_calls(tool_calls)?);
                        response.set_usage_if_none(usage);
                        return Ok(response);
                    }
                    Some(Err(error)) => return Err(error),
                    None => {
                        return Err(ModelError::protocol(
                            ProtocolErrorKind::StreamEndedWithoutCompleted,
                        ));
                    }
                },
            }
        }
    }
}
