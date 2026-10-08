//! 同一轮工具循环中的 Full 失败恢复，不依赖下一条用户 prompt。

use super::*;
use crate::agent::events_v2::{EventBus, EventHandles, ObserveEvent};
use crate::agent::react::{ReactLLM, Reasoning, StreamingContext, ToolCall};
use crate::agent::stages::{run_react_loop, LoopResult, StageContext};
use crate::agent::token::ContextBudget;
use crate::error::AgentError;
use crate::messages::BaseMessage;
use crate::session::store::FrozenContext;
use crate::session::test_resources::TestSession;
use crate::session::{MessageSource, QueuedMessage, Session};
use crate::tools::{BaseTool, ToolContext};
use peri_model::{ModelCapabilities, ModelMessage, ModelRequest, ModelResponse, ModelStream};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio_util::sync::CancellationToken;

struct LoopModel {
    requests: parking_lot::Mutex<Vec<Vec<BaseMessage>>>,
    order: Arc<parking_lot::Mutex<Vec<&'static str>>>,
}

#[async_trait::async_trait]
impl ReactLLM for LoopModel {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _: &[&dyn BaseTool],
        _: Option<StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        self.order.lock().push("reason");
        let mut requests = self.requests.lock();
        requests.push(messages.to_vec());
        let first = requests.len() == 1;
        let mut response = if first {
            Reasoning::with_tools(
                "",
                vec![ToolCall::new("call", "Work", serde_json::json!({}))],
            )
        } else {
            Reasoning::with_answer("", "done")
        };
        response.usage = Some(peri_model::TokenUsage {
            input_tokens: if first { 109_000 } else { 1_000 },
            output_tokens: 100,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
        Ok(response)
    }
}

struct WorkTool(Arc<parking_lot::Mutex<Vec<&'static str>>>);

#[async_trait::async_trait]
impl BaseTool for WorkTool {
    fn name(&self) -> &str {
        "Work"
    }
    fn description(&self) -> &str {
        "Complete one work item"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn invoke(
        &self,
        _: serde_json::Value,
        _: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.0.lock().push("tool");
        Ok("work completed".into())
    }
}

struct SummaryModel {
    failures: usize,
    calls: AtomicUsize,
    order: Arc<parking_lot::Mutex<Vec<&'static str>>>,
    cancel: Option<Arc<CancellationToken>>,
}

#[async_trait::async_trait]
impl peri_model::Model for SummaryModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
    async fn stream(
        &self,
        _: ModelRequest,
        _: CancellationToken,
    ) -> peri_model::ModelResult<ModelStream> {
        unreachable!("摘要只使用 complete")
    }
    async fn complete(
        &self,
        _: ModelRequest,
        _: CancellationToken,
    ) -> peri_model::ModelResult<ModelResponse> {
        self.order.lock().push("summary");
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(cancel) = &self.cancel {
            if call == 1 {
                cancel.cancel();
                std::future::pending::<()>().await;
            }
        }
        ModelResponse::new(
            ModelMessage::assistant_text(if call < self.failures {
                "<analysis>no usable summary</analysis>"
            } else {
                "<summary>RECOVERED: work completed; continue the task.</summary>"
            }),
            peri_model::StopReason::EndTurn,
            None,
            None,
        )
    }
}

async fn make_scenario(
    failures: usize,
    cancel_retry: bool,
) -> (
    TestSession,
    StageContext,
    Arc<LoopModel>,
    Arc<SummaryModel>,
    EventHandles,
) {
    let bound = TestSession::open().await;
    let session = Session::new(
        Arc::from("/tmp/compact-retry"),
        FrozenContext::builder().build(),
        Some(bound.thread_id.clone()),
    );
    {
        let transcript = session.transcript();
        let mut guard = transcript.write();
        *guard = std::mem::take(&mut *guard)
            .with_persistence(bound.resources(), bound.thread_id.clone());
    }
    let order = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let model = Arc::new(LoopModel {
        requests: Default::default(),
        order: order.clone(),
    });
    let turn = session.start_turn();
    let summary = Arc::new(SummaryModel {
        failures,
        calls: AtomicUsize::new(0),
        order: order.clone(),
        cancel: cancel_retry.then(|| turn.cancel_token.clone()),
    });
    let (bus, handles) = EventBus::new(Default::default());
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(model.clone())
        .with_compact_llm(summary.clone())
        .with_context_budget(ContextBudget::new(100_000))
        .with_compact_config(CompactConfig::default())
        .with_tools(Arc::new(parking_lot::RwLock::new(
            std::collections::BTreeMap::from([(
                "Work".into(),
                Arc::new(WorkTool(order)) as Arc<dyn BaseTool>,
            )]),
        )))
        .with_event_bus(Arc::new(bus))
        .build();
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("original task"),
    ));
    (bound, ctx, model, summary, handles)
}

/// [回归测试] 109% 时摘要先失败两次，必须在下一次 Reason 前完成 Full。
#[tokio::test]
async fn test_compact_retry_recovers_inside_tool_loop() {
    let (bound, ctx, model, summary, mut handles) = make_scenario(2, false).await;
    let result = run_react_loop(ctx.clone(), 10).await;
    assert!(matches!(result, LoopResult::Completed), "{result:?}");
    assert_eq!(
        *model.order.lock(),
        ["reason", "tool", "summary", "summary", "summary", "reason"]
    );
    assert_eq!(summary.calls.load(Ordering::SeqCst), 3);
    assert!(model.requests.lock()[1]
        .iter()
        .any(|message| message.content().contains("RECOVERED")));
    assert_eq!(
        std::iter::from_fn(|| handles.try_observe())
            .filter(|e| matches!(e, ObserveEvent::MessagesCompacted { .. }))
            .count(),
        1
    );
    let transcript = std::mem::take(&mut *ctx.session.transcript.write());
    transcript.flush_persistence().await.unwrap();
    let snapshot = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    for message in transcript.entries().iter().take(3) {
        assert!(
            snapshot.flags[&message.id()].excluded,
            "成功摘要必须在持久化历史中替换旧工作"
        );
    }
}

/// [回归测试] 连续空摘要不得静默禁用 Compact 并继续发送超预算请求。
#[tokio::test]
async fn test_compact_retry_exhaustion_stops_before_next_reason() {
    let (_bound, ctx, model, summary, mut handles) = make_scenario(usize::MAX, false).await;
    let result = run_react_loop(ctx.clone(), 10).await;
    let LoopResult::Error(error) = result else {
        panic!("压缩失败必须显式终止：{result:?}");
    };
    assert!(
        matches!(
            error,
            AgentError::CompactRetriesExhausted {
                attempts: 3,
                // 工具调用名称/参数的请求增量约 2 tokens，工具结果约 3。
                context_tokens: 109_005,
                context_window: 100_000,
            }
        ),
        "{error}"
    );
    assert_eq!(summary.calls.load(Ordering::SeqCst), 3);
    assert_eq!(model.requests.lock().len(), 1);
    assert!(ctx
        .session
        .transcript
        .read()
        .visible_messages()
        .iter()
        .any(|m| m.content() == "original task"));
    let events: Vec<_> = std::iter::from_fn(|| handles.try_observe()).collect();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, ObserveEvent::CompactEnded { .. }))
            .count(),
        1
    );
    assert!(!events
        .iter()
        .any(|e| matches!(e, ObserveEvent::MessagesCompacted { .. })));
}

/// [回归测试] 同阶段摘要重试必须继续响应用户取消。
#[tokio::test]
async fn test_compact_retry_cancel_stops_before_next_reason() {
    let (_bound, ctx, model, summary, _) = make_scenario(usize::MAX, true).await;
    let result = run_react_loop(ctx.clone(), 10).await;
    assert!(matches!(result, LoopResult::Interrupted), "{result:?}");
    assert_eq!(summary.calls.load(Ordering::SeqCst), 2);
    assert_eq!(model.requests.lock().len(), 1);
    assert!(!ctx.session.transcript.read().visible_messages().is_empty());
}

/// [回归测试] 已达失败上限的高压状态不能经 Skipped 继续进入 Reason。
#[tokio::test]
async fn test_compact_retry_existing_failure_limit_is_terminal() {
    let (_bound, ctx, model, summary, _) = make_scenario(0, false).await;
    ctx.compact
        .compact_consecutive_failures
        .store(3, Ordering::Relaxed);
    ctx.compact
        .token_tracker
        .write()
        .accumulate(&peri_model::TokenUsage {
            input_tokens: 109_000,
            output_tokens: 0,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
    let result = run_react_loop(ctx, 10).await;
    assert!(
        matches!(
            result,
            LoopResult::Error(AgentError::CompactRetriesExhausted {
                attempts: 3,
                context_tokens: 109_000,
                context_window: 100_000,
            })
        ),
        "{result:?}"
    );
    assert_eq!(summary.calls.load(Ordering::SeqCst), 0);
    assert!(model.requests.lock().is_empty());
}
