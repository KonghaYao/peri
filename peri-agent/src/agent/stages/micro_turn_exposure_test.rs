//! 跨真实 StageContext / user turn 的消费证据与冷恢复回归。
use super::compact::run_compact;
use super::*;
use crate::agent::compact_v2::projection::{render_persisted_llm_view, ProjectionAction};
use crate::session::{test_resources::TestSession, FrozenContext, MessageTranscript, Session};
use crate::tools::{
    BaseTool, ContextRetention, ToolContext, ToolExecutionEvidence, ToolExecutionStatus, ToolOutput,
};
use std::sync::atomic::{AtomicUsize, Ordering};

struct WriteReceipt;
#[async_trait::async_trait]
impl BaseTool for WriteReceipt {
    fn name(&self) -> &str {
        "WriteReceipt"
    }
    fn description(&self) -> &str {
        "Write the fixture and return a receipt"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type":"object"})
    }
    fn is_direct(&self) -> bool {
        true
    }
    fn context_retention(&self) -> ContextRetention {
        ContextRetention::SideEffectReceipt
    }
    async fn invoke(
        &self,
        input: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.invoke_output(input, ctx).await?.text)
    }
    async fn invoke_output(
        &self,
        _: serde_json::Value,
        _: ToolContext<'_>,
    ) -> Result<ToolOutput, Box<dyn std::error::Error + Send + Sync>> {
        Ok(ToolOutput::with_execution(
            "receipt ".repeat(10_000),
            ToolExecutionEvidence {
                status: ToolExecutionStatus::Completed,
                task_id: None,
                exit_code: None,
                output_ref: None,
                output_truncated: false,
            },
        ))
    }
}
struct ArchiveTool;
#[async_trait::async_trait]
impl BaseTool for ArchiveTool {
    fn name(&self) -> &str {
        "ReadContextArchive"
    }
    fn description(&self) -> &str {
        "Recover canonical archive bytes"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type":"object"})
    }
    fn is_direct(&self) -> bool {
        true
    }
    fn context_retention(&self) -> ContextRetention {
        ContextRetention::ArchiveRead
    }
    async fn invoke(
        &self,
        _: serde_json::Value,
        _: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        unreachable!("本回归只压缩历史，不执行回查")
    }
}
struct TwoTurns {
    requests: parking_lot::Mutex<Vec<Vec<BaseMessage>>>,
    calls: AtomicUsize,
    write_first: bool,
    fail: bool,
}
#[async_trait::async_trait]
impl ReactLLM for TwoTurns {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _: &[&dyn BaseTool],
        _: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        self.requests.lock().push(messages.to_vec());
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        if self.fail {
            return Err(crate::error::AgentError::LlmError("failed request".into()));
        }
        let mut result = if self.write_first && call == 0 {
            crate::agent::react::Reasoning::with_tools(
                "",
                vec![crate::agent::react::ToolCall::new(
                    "write-old",
                    "WriteReceipt",
                    serde_json::json!({"path":"fixture.txt"}),
                )],
            )
        } else {
            crate::agent::react::Reasoning::with_answer("", "done")
        };
        result.usage = Some(peri_model::TokenUsage {
            input_tokens: 1_000,
            output_tokens: 1,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
        Ok(result)
    }
}
fn script(write_first: bool, fail: bool) -> Arc<TwoTurns> {
    Arc::new(TwoTurns {
        requests: Default::default(),
        calls: AtomicUsize::new(0),
        write_first,
        fail,
    })
}
fn context(session: &Session, llm: Arc<TwoTurns>) -> StageContext {
    let tools: SharedToolMap = Arc::new(parking_lot::RwLock::new(BTreeMap::from([
        (
            "WriteReceipt".into(),
            Arc::new(WriteReceipt) as Arc<dyn BaseTool>,
        ),
        (
            "ReadContextArchive".into(),
            Arc::new(ArchiveTool) as Arc<dyn BaseTool>,
        ),
    ])));
    StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_compact_exposure(session.compact_exposure())
    .with_llm(llm)
    .with_tools(tools)
    .with_context_budget(crate::agent::token::ContextBudget::new(100_000))
    .with_compact_config(CompactConfig {
        micro_compact_stale_steps: 0,
        ..Default::default()
    })
    .build()
}
async fn session(bound: &TestSession, cold: bool) -> Arc<Session> {
    let session = Session::new(
        Arc::from("/tmp/turn-exposure"),
        FrozenContext::builder().build(),
        Some(bound.thread_id.clone()),
    );
    let mut transcript = MessageTranscript::new();
    if cold {
        let snapshot = bound
            .resources()
            .load_session_snapshot(&bound.thread_id)
            .await
            .unwrap();
        transcript = transcript.with_own_payloads(snapshot.payloads);
        transcript.set_flags_batch(snapshot.flags);
    }
    *session.transcript().write() =
        transcript.with_persistence(bound.resources(), bound.thread_id.clone());
    session
}
fn prompt(session: &Session, text: impl Into<String>) {
    session.queue().push(crate::session::QueuedMessage::prompt(
        crate::session::MessageSource::UserInput,
        BaseMessage::human(text.into()),
    ));
}
fn receipt_id(ctx: &StageContext) -> crate::messages::MessageId {
    ctx.session
        .transcript
        .read()
        .entries()
        .iter()
        .find_map(|entry| match entry.as_message() {
            Some(BaseMessage::Tool {
                id, tool_call_id, ..
            }) if tool_call_id == "write-old" => Some(*id),
            _ => None,
        })
        .unwrap()
}
fn has_whole_omission(ctx: &StageContext, id: crate::messages::MessageId) -> bool {
    let transcript = ctx.session.transcript.read();
    !render_persisted_llm_view(&transcript, &Default::default())
        .unwrap()
        .iter()
        .any(|m| m.id() == id)
}
async fn first_turn(
    bound: &TestSession,
) -> (Arc<Session>, StageContext, crate::messages::MessageId) {
    let session = session(bound, false).await;
    prompt(&session, "write fixture");
    let context = context(&session, script(true, false));
    let result = run_react_loop(context.clone(), 4).await;
    assert!(matches!(result, LoopResult::Completed), "{result:?}");
    let id = receipt_id(&context);
    assert!(
        !context.session.transcript.read().is_truncated(id),
        "第一轮不触发压缩"
    );
    (session, context, id)
}

/// [回归测试] 正常新 user turn 重建 StageContext 后，已消费 Write 回执仍可整组省略。
#[tokio::test]
async fn test_new_turn_retains_exposure_and_terminal_evidence() {
    let bound = TestSession::open().await;
    let (_old_session, first, id) = first_turn(&bound).await;
    // Main Session is rebuilt by stage_builder for each prompt; the host owns this Arc.
    let session = session(&bound, true).await;
    session.set_compact_exposure(Arc::clone(&first.compact.exposure));
    let second = context(&session, script(false, false));
    assert_ne!(first.turn_id(), second.turn_id());
    assert!(Arc::ptr_eq(
        &first.compact.exposure,
        &second.compact.exposure
    ));
    prompt(&session, "user-owned constraint ".repeat(12_000));
    let outcome = run_react_loop(second.clone(), 2).await;
    assert!(matches!(outcome, LoopResult::Completed), "{outcome:?}");
    assert!(has_whole_omission(&second, id), "上一轮回执应可整组省略");
    assert!(
        second.session.transcript.read().get(id).is_some(),
        "canonical 正文保留"
    );
}

/// [回归测试] 已提交 preview 的 canonical 消费证据跨 turn 保留；
/// 但 preview→整组省略的增量收益低于完整窗口 10% 时必须延后，不得为凑门槛扩大删除。
#[tokio::test]
async fn test_committed_preview_upgrade_below_gain_gate_is_deferred() {
    let bound = TestSession::open().await;
    let (_old_session, first, id) = first_turn(&bound).await;
    first
        .compact
        .token_tracker
        .write()
        .accumulate(&peri_model::TokenUsage {
            input_tokens: 76_000,
            output_tokens: 1,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
    let compact = run_compact(CompactInput {
        context: first.clone(),
        has_tool_calls: false,
    })
    .await
    .unwrap();
    assert!(compact.compacted);
    {
        let transcript = first.session.transcript.read();
        let directive = transcript.projection_ref(id).unwrap();
        assert!(directive
            .entries
            .iter()
            .any(|entry| matches!(entry.action, ProjectionAction::CompactToolResult { .. })));
    }
    assert!(!has_whole_omission(&first, id), "第一轮仅提交 preview");
    let session = session(&bound, true).await;
    session.set_compact_exposure(Arc::clone(&first.compact.exposure));
    let second = context(&session, script(false, false));
    // 新 turn 的真实用户任务已在 canonical 中；它不参与本轮的省略候选。
    second
        .session
        .transcript
        .write()
        .append(BaseMessage::human("user-owned constraint ".repeat(12_000)));
    // Provider usage is a separate pressure authority from the reduced visible view.
    second
        .compact
        .token_tracker
        .write()
        .accumulate(&peri_model::TokenUsage {
            input_tokens: 76_000,
            output_tokens: 1,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
    let deferred = run_compact(CompactInput {
        context: second.clone(),
        has_tool_calls: false,
    })
    .await
    .unwrap();
    assert!(!deferred.compacted, "低收益升级必须延后");
    assert!(
        !has_whole_omission(&second, id),
        "延后不得把已提交 preview 升级为整组省略"
    );
    assert!(
        second
            .session
            .transcript
            .read()
            .projection_ref(id)
            .unwrap()
            .entries
            .iter()
            .any(|entry| matches!(entry.action, ProjectionAction::CompactToolResult { .. })),
        "延后必须保留原已提交 preview"
    );
}

/// [回归测试] 冷恢复先保护旧执行，成功全量重新暴露后可采用可信同步终态证据。
#[tokio::test]
async fn test_cold_history_reexposure_enables_typed_synchronous_completion() {
    let bound = TestSession::open().await;
    let (_hot, first, id) = first_turn(&bound).await;
    drop(first);
    let cold = session(&bound, true).await;
    let reexpose = context(&cold, script(false, false));
    assert!(reexpose.compact.exposure.lock().closed_calls().is_empty());
    prompt(&cold, "consume restored history");
    let outcome = run_react_loop(reexpose.clone(), 2).await;
    assert!(matches!(outcome, LoopResult::Completed), "{outcome:?}");
    assert!(reexpose
        .compact
        .exposure
        .lock()
        .closed_calls()
        .contains("write-old"));
    let next = context(&cold, script(false, false));
    prompt(&cold, "user-owned constraint ".repeat(12_000));
    let outcome = run_react_loop(next.clone(), 2).await;
    assert!(matches!(outcome, LoopResult::Completed), "{outcome:?}");
    assert!(has_whole_omission(&next, id), "冷历史成功重新暴露后可压缩");
}

/// [回归测试] 74.99% 仍跳过已有跨 turn 消费证据，不创建新投影。
#[tokio::test]
async fn test_new_turn_below_gate_does_not_create_projection() {
    let bound = TestSession::open().await;
    let (session, _first, id) = first_turn(&bound).await;
    session
        .transcript()
        .write()
        .append(BaseMessage::human("user-owned constraint ".repeat(12_000)));
    let second = context(&session, script(false, false));
    second
        .compact
        .token_tracker
        .write()
        .accumulate(&peri_model::TokenUsage {
            input_tokens: 74_990,
            output_tokens: 1,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
    let result = run_compact(CompactInput {
        context: second.clone(),
        has_tool_calls: false,
    })
    .await
    .unwrap();
    assert!(!result.compacted, "74.99% 不新建投影");
    assert!(!second.session.transcript.read().is_truncated(id));
    assert!(!has_whole_omission(&second, id));
}

/// [回归测试] 冷历史失败请求不能确认 canonical 消费，也不能借 typed Completed 绕过。
#[tokio::test]
async fn test_failed_cold_request_keeps_history_protected() {
    let bound = TestSession::open().await;
    let (_hot, _first, id) = first_turn(&bound).await;
    let cold = session(&bound, true).await;
    let failed = context(&cold, script(false, true));
    prompt(&cold, "consume restored history");
    let outcome = run_react_loop(failed.clone(), 2).await;
    assert!(
        !matches!(outcome, LoopResult::Completed),
        "失败请求应结束为错误"
    );
    let transcript = failed.session.transcript.read();
    let ledger = failed.compact.exposure.lock();
    assert!(!ledger.exposed_ids(&transcript).unwrap().contains(&id));
    assert!(!ledger.closed_calls().contains("write-old"));
    assert!(!transcript.is_truncated(id));
}
