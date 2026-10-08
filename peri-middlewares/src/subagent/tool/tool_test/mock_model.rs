//! 请求捕获 mock Model（H1/H2 子侧验收）。
//!
//! 经**生产装配路径**（`SubagentLlmSource::model` → `build_subagent_session_v2`
//! 的 bridge → `collect_tools`）捕获真实 `ModelRequest`：system 段与 messages
//! 都是最终请求面，而不是测试替身内部状态。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_model::{
    ContentBlock, Model, ModelCapabilities, ModelMessage, ModelRequest, ModelResponse, ModelResult,
    ModelStream, ModelStreamEvent, StopReason,
};
use tokio_util::sync::CancellationToken;

/// 固定回答的捕获模型：每次 `stream` 记录请求并返回一条 assistant 文本。
pub(crate) struct RecordingModel {
    requests: Mutex<Vec<ModelRequest>>,
    answer: String,
    calls: Mutex<usize>,
}

impl RecordingModel {
    pub(crate) fn new(answer: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            answer: answer.into(),
            calls: Mutex::new(0),
        })
    }

    pub(crate) fn call_count(&self) -> usize {
        *self.calls.lock().unwrap()
    }

    /// 最后一次请求的 system 段文本（bridge 在 messages[0] 插入 `System`）。
    pub(crate) fn last_system(&self) -> String {
        self.requests
            .lock()
            .unwrap()
            .last()
            .map(extract_system)
            .unwrap_or_default()
    }

    /// 最后一次请求的全部消息（含插入的 system）。
    pub(crate) fn last_messages(&self) -> Vec<ModelMessage> {
        self.requests
            .lock()
            .unwrap()
            .last()
            .map(|request| request.messages.clone())
            .unwrap_or_default()
    }
}

/// 提取请求中的 system 文本（按消息顺序拼接 `System` 段）。
pub(crate) fn extract_system(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .filter_map(|message| match message {
            ModelMessage::System { content } => Some(text_of(content)),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn text_of(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>()
}

/// messages 中非 system 消息的文本投影（断言身份/贡献不重复进入对话体）。
pub(crate) fn conversation_texts(messages: &[ModelMessage]) -> Vec<String> {
    messages
        .iter()
        .map(|message| match message {
            ModelMessage::System { content } => format!("[system]{}", text_of(content)),
            ModelMessage::User { content } => text_of(content),
            ModelMessage::Assistant { content, .. } => text_of(content),
            ModelMessage::ToolResult { result } => format!("[tool]{}", text_of(&result.content)),
        })
        .collect()
}

#[async_trait]
impl Model for RecordingModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_streaming: true,
            ..ModelCapabilities::default()
        }
    }

    /// v2 stages 走 durable prepared 路径：冻结请求（捕获于此）并在 start 时
    /// 惰性产出 Completed 流（取消可中断）。
    fn prepare_stream(&self, request: ModelRequest) -> ModelResult<peri_model::PreparedModelCall> {
        *self.calls.lock().unwrap() += 1;
        self.requests.lock().unwrap().push(request.clone());
        let answer = self.answer.clone();
        let checkpoint = serde_json::json!({
            "provider": "fixture-capture",
            "model": "capture-model",
            "endpoint": "https://fixture.invalid/messages",
            "credentialRef": "fixture:no-credentials",
            "body": request,
        });
        Ok(peri_model::PreparedModelCall::new(
            checkpoint,
            move |cancellation| {
                let response = ModelResponse::new(
                    ModelMessage::assistant_text(answer),
                    StopReason::EndTurn,
                    None,
                    None,
                )?;
                Ok(ModelStream::with_parent_cancellation(
                    futures::stream::iter(vec![Ok(ModelStreamEvent::Completed(response))]),
                    cancellation,
                ))
            },
        ))
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        *self.calls.lock().unwrap() += 1;
        self.requests.lock().unwrap().push(request);
        let response = ModelResponse::new(
            ModelMessage::assistant_text(self.answer.clone()),
            StopReason::EndTurn,
            None,
            None,
        )?;
        Ok(ModelStream::with_parent_cancellation(
            futures::stream::iter(vec![Ok(ModelStreamEvent::Completed(response))]),
            cancellation,
        ))
    }
}
