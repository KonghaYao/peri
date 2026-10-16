//! 模型视图复用（OPT-03）行为锁定。
//!
//! 目标：同一次 Reason 准备范围内，transcript 修订未变时只渲染一次模型视图；
//! 任何内容变更（消息追加、flag、投影、实例替换、provider 能力变化）都必须重建。
//!
//! 另有缓存生命周期回归锁定：同一时刻至多一份整份正文存活——键不匹配先释放旧项再
//! 渲染、真正压缩前释放（Full 摘要请求期间不留历史正文）、退出准备作用域统一释放。

use super::model_view::ModelViewScope;
use super::reason::run_reason;
use super::{CompactInput, ReasonInput, StageContext};

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use peri_model::{
    Model, ModelCapabilities, ModelMessage, ModelRequest, ModelResponse, ModelResult, ModelStream,
    StopReason,
};
use tokio_util::sync::CancellationToken;

use crate::agent::compact_v2::projection::ProviderCapabilities;
use crate::agent::compact_v2::CompactConfig;
use crate::agent::token::ContextBudget;
use crate::messages::{BaseMessage, MessageContent};
use crate::session::store::FrozenContext;
use crate::session::test_resources::mock::MockSessionResources;
use crate::session::transcript::MessageTranscript;
use crate::session::Session;
use crate::thread::ThreadMeta;

fn human(text: &str) -> BaseMessage {
    BaseMessage::human(MessageContent::text(text))
}

#[test]
fn test_view_reuses_render_when_revision_unchanged() {
    let scope = ModelViewScope::default();
    let mut transcript = MessageTranscript::new();
    transcript.append(human("hello"));
    let caps = ProviderCapabilities::anthropic();

    let first = scope.view(&transcript, &caps).expect("首次渲染");
    let second = scope.view(&transcript, &caps).expect("复用渲染结果");

    assert!(
        Arc::ptr_eq(&first, &second),
        "修订未变必须复用同一份 Arc，不得重建视图"
    );
    assert_eq!(scope.render_count(), 1, "同一修订只应渲染一次");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].content(), "hello");
}

#[test]
fn test_view_rebuilds_after_content_change() {
    let scope = ModelViewScope::default();
    let mut transcript = MessageTranscript::new();
    transcript.append(human("first"));
    let caps = ProviderCapabilities::anthropic();

    let before = scope.view(&transcript, &caps).expect("首次渲染");
    transcript.append(human("second"));
    let after = scope.view(&transcript, &caps).expect("内容变更后重建");

    assert!(!Arc::ptr_eq(&before, &after), "内容变更后不得复用旧视图");
    assert_eq!(scope.render_count(), 2);
    assert_eq!(after.len(), 2);
    assert_eq!(after[1].content(), "second");
}

#[test]
fn test_view_does_not_reuse_across_transcript_instances() {
    let scope = ModelViewScope::default();
    let mut left = MessageTranscript::new();
    left.append(human("left"));
    let mut right = MessageTranscript::new();
    right.append(human("right"));
    let caps = ProviderCapabilities::anthropic();

    // Compact 用 `std::mem::take` 换出 transcript：换入的新实例修订号可能与旧实例相同，
    // 只有实例身份能区分二者。
    assert_eq!(
        left.view_revision().revision,
        right.view_revision().revision,
        "构造前提：两个实例的修订号相同"
    );
    assert_ne!(
        left.view_revision().instance,
        right.view_revision().instance
    );

    let left_view = scope.view(&left, &caps).expect("左实例视图");
    let right_view = scope.view(&right, &caps).expect("右实例视图");

    assert!(
        !Arc::ptr_eq(&left_view, &right_view),
        "实例身份不同不得复用视图"
    );
    assert_eq!(right_view[0].content(), "right");
    assert_eq!(scope.render_count(), 2);
}

#[test]
fn test_view_rebuilds_when_capabilities_change() {
    let scope = ModelViewScope::default();
    let mut transcript = MessageTranscript::new();
    transcript.append(human("hello"));

    let anthropic = scope
        .view(&transcript, &ProviderCapabilities::anthropic())
        .expect("Anthropic 视图");
    let generic = scope
        .view(&transcript, &ProviderCapabilities::default())
        .expect("Generic 视图");

    assert!(
        !Arc::ptr_eq(&anthropic, &generic),
        "provider 能力变化后不得复用旧视图"
    );
    assert_eq!(scope.render_count(), 2);
}

#[test]
fn test_release_and_reset_clear_cached_view() {
    let scope = ModelViewScope::default();
    let mut transcript = MessageTranscript::new();
    transcript.append(human("hello"));
    let caps = ProviderCapabilities::anthropic();

    let first = scope.view(&transcript, &caps).expect("首次渲染");
    assert!(scope.is_cached());
    scope.release();
    assert!(!scope.is_cached(), "release 必须清空缓存槽");

    // 槽已清空：同修订也重建，内容仍等价
    let again = scope.view(&transcript, &caps).expect("释放后重建");
    assert_eq!(scope.render_count(), 2);
    assert_eq!(first.len(), again.len());
    assert_eq!(first[0].content(), again[0].content());

    assert!(scope.is_cached());
    scope.reset();
    assert!(!scope.is_cached(), "reset 必须清空缓存槽");
}

/// 验收：一次 Reason 内（修订未变）只渲染一次模型视图，且缓存不跨执行复用。
#[tokio::test]
async fn test_run_reason_renders_model_view_once_per_execution() {
    let cwd: Arc<str> = Arc::from("/tmp/model-view-scope");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone()).build();
    ctx.session.transcript.write().append(human("hello"));

    let _ = run_reason(ReasonInput {
        context: ctx.clone(),
        has_tool_calls: false,
    })
    .await;

    assert_eq!(
        ctx.model_view.render_count(),
        1,
        "准备检查、before_model 复检与请求快照必须复用同一份视图"
    );
    assert!(
        !ctx.model_view.is_cached(),
        "请求快照取出后必须释放缓存槽，正文副本不跨 Act 常驻"
    );

    let _ = run_reason(ReasonInput {
        context: ctx.clone(),
        has_tool_calls: false,
    })
    .await;

    assert_eq!(
        ctx.model_view.render_count(),
        2,
        "缓存不跨执行复用：第二次执行重新渲染一次"
    );
}

/// 验收：覆盖 Compact 检查时，相同版本仍只构建一次。
///
/// 每轮迭代先跑 Compact 阶段（判断前 refresh），随后 Reason 再复检并取请求快照：
/// 三者输入未变时必须共用同一份视图。
#[tokio::test]
async fn test_compact_check_and_reason_reuse_same_view() {
    let cwd: Arc<str> = Arc::from("/tmp/model-view-compact-check");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone()).build();
    ctx.session.transcript.write().append(human("hello"));

    // 迭代入口清槽（与 run_react_loop 一致）
    ctx.model_view.reset();
    // Compact 阶段：判断是否 Skip 前的 refresh
    assert!(
        super::compact::context_pressure::refresh(&ctx).expect("Compact 检查 refresh"),
        "首次 refresh 建立估算基线，按增长处理"
    );

    let _ = run_reason(ReasonInput {
        context: ctx.clone(),
        has_tool_calls: false,
    })
    .await;

    assert_eq!(
        ctx.model_view.render_count(),
        1,
        "Compact 检查、Reason 复检与请求快照必须复用同一份同版本视图"
    );
    assert!(!ctx.model_view.is_cached(), "请求快照取出后必须释放缓存槽");
}

/// 验收：`before_model` 之后 transcript 增长时，复检必须看到新内容并报告增长。
#[test]
fn test_refresh_reports_growth_after_transcript_append() {
    let cwd: Arc<str> = Arc::from("/tmp/model-view-growth");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone()).build();

    assert!(
        super::compact::context_pressure::refresh(&ctx).expect("首次 refresh"),
        "首次 refresh 建立估算基线，按增长处理"
    );
    assert!(
        !super::compact::context_pressure::refresh(&ctx).expect("同值 refresh"),
        "同值重复 refresh 不构成增长"
    );
    assert_eq!(ctx.model_view.render_count(), 1, "修订未变不应重建视图");

    ctx.session
        .transcript
        .write()
        .append(human(&"x".repeat(4_000)));

    assert!(
        super::compact::context_pressure::refresh(&ctx).expect("增长后 refresh"),
        "transcript 增长后 refresh 必须报告 grew=true"
    );
    assert_eq!(
        ctx.model_view.render_count(),
        2,
        "内容变更后必须重建视图，不能复用旧视图"
    );
}

// ─── 缓存生命周期回归（同一时刻至多一份整份正文存活）─────────────────────────────

/// 验收：键不匹配的重建路径上，同时存活的视图正文至多一份。
///
/// 观察点是缓存槽持有的正文句柄数峰值：旧项若未先释放就写入新项，写入瞬间两份正文
/// 同时存在（长上下文下等于两份历史正文常驻），峰值会升到 2。
#[test]
fn test_rebuild_keeps_at_most_one_live_view_body() {
    let scope = ModelViewScope::default();
    let mut transcript = MessageTranscript::new();
    transcript.append(human("first"));
    let caps = ProviderCapabilities::anthropic();

    let first = scope.view(&transcript, &caps).expect("首次渲染");
    let stale = Arc::downgrade(&first);
    drop(first); // 只留缓存槽一份强引用：旧正文的存活完全由该槽决定
    assert!(stale.upgrade().is_some(), "前提：旧正文由缓存槽持有");

    transcript.append(human("second"));
    let rebuilt = scope.view(&transcript, &caps).expect("键不匹配后重建");

    assert_eq!(scope.render_count(), 2, "修订变更必须重建");
    assert_eq!(rebuilt.len(), 2);
    assert_eq!(rebuilt[1].content(), "second");
    assert_eq!(
        scope.peak_live_views(),
        1,
        "重建期间同时存活的视图正文至多一份：旧正文必须先释放再渲染新视图"
    );
    assert!(stale.upgrade().is_none(), "重建结束后旧正文不得继续存活");
}

/// Full 摘要请求观察点：摘要模型被调用（摘要视图已构造并正在发送）时的缓存槽状态。
#[derive(Default)]
struct SummarySlotProbe {
    /// 会话上下文在 `build()` 之后才存在，由测试回填。
    scope: std::sync::OnceLock<Arc<ModelViewScope>>,
    calls: AtomicUsize,
    slot_cached: std::sync::Mutex<Vec<bool>>,
}

#[async_trait]
impl Model for SummarySlotProbe {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: false,
            supports_reasoning: false,
            supports_vision: false,
            supports_streaming: true,
        }
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        unreachable!("Full 摘要请求走 complete")
    }

    async fn complete(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let cached = self.scope.get().is_some_and(|scope| scope.is_cached());
        self.slot_cached.lock().unwrap().push(cached);
        Ok(ModelResponse::new(
            ModelMessage::assistant_text("<summary>full compact summary</summary>"),
            StopReason::EndTurn,
            None,
            None,
        )?)
    }
}

/// 验收：真实 Full Compact 链路上，摘要请求期间缓存槽必须为空。
///
/// Full 会在 `compact_v2` 内独立构造整份历史视图作为摘要请求。若 Compact 检查填充的
/// 槽一直留到 Reason 才释放，整个摘要请求期间就多驻留一份历史正文。这里断言两件事：
/// 摘要模型被调用时槽已空，以及 Compact 前槽内那份正文已经被释放（弱引用探针），
/// 且释放没有引入额外渲染。
#[tokio::test]
async fn test_full_compact_summary_request_holds_no_cached_view() {
    let cwd: Arc<str> = Arc::from("/tmp/model-view-full-compact");
    let session = Session::new(cwd, FrozenContext::builder().build(), None);
    // Full 提交走持久化事务：绑定内存会话资源夹具。
    let store = MockSessionResources::new();
    let thread_id = store
        .create_thread(ThreadMeta::new_at("/tmp", peri_time::now_wall()))
        .await
        .expect("测试会话创建");
    {
        let transcript = session.transcript();
        let mut transcript = transcript.write();
        *transcript = std::mem::take(&mut *transcript).with_persistence(store, thread_id);
    }

    let probe = Arc::new(SummarySlotProbe::default());
    let turn = session.start_turn();
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_compact_llm(Arc::clone(&probe) as Arc<dyn Model>)
        .with_context_budget(ContextBudget::new(1_000))
        .with_compact_config(CompactConfig::default())
        .build();
    assert!(probe.scope.set(Arc::clone(&ctx.model_view)).is_ok());

    // 长历史 + 窄预算：Micro 计划为空且预算超过 Full 阈值 → 本轮直接 Full。
    ctx.session
        .transcript
        .write()
        .append(human(&"x".repeat(4_000)));

    // 预置同版本缓存，只保留弱引用：该正文是否已释放由探针见证。
    let capabilities = ctx.runtime.llm.provider_capabilities();
    let cached_body = {
        let transcript = ctx.session.transcript.read();
        Arc::downgrade(
            &ctx.model_view
                .view(&transcript, &capabilities)
                .expect("预置视图"),
        )
    };
    assert!(cached_body.upgrade().is_some(), "前提：正文只由缓存槽持有");
    assert_eq!(ctx.model_view.render_count(), 1, "前提：预置视图渲染一次");

    let output = super::compact::run_compact(CompactInput {
        context: ctx.clone(),
        has_tool_calls: false,
    })
    .await
    .expect("Full Compact 应成功提交");

    assert!(output.compacted, "本轮应实际执行压缩");
    assert_eq!(
        probe.calls.load(Ordering::SeqCst),
        1,
        "单次 Full 只发一次摘要请求"
    );
    assert_eq!(
        probe.slot_cached.lock().unwrap().clone(),
        vec![false],
        "摘要请求期间缓存槽必须为空：不得多驻留一份整份历史正文"
    );
    assert!(
        cached_body.upgrade().is_none(),
        "Compact 执行后槽内旧正文必须已释放"
    );
    assert_eq!(
        ctx.model_view.render_count(),
        1,
        "释放不得引入额外渲染：Compact 检查仍复用预置视图"
    );
}

/// 验收：预算未达阈值时 Compact 走 Skip，缓存保留并继续被 Reason 复用。
#[tokio::test]
async fn test_compact_skip_keeps_cached_view_for_reason() {
    let cwd: Arc<str> = Arc::from("/tmp/model-view-compact-skip");
    let session = Session::new(cwd, FrozenContext::builder().build(), None);
    let turn = session.start_turn();
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_context_budget(ContextBudget::new(200_000))
        .with_compact_config(CompactConfig::default())
        .build();
    ctx.session.transcript.write().append(human("hello"));

    ctx.model_view.reset();
    let output = super::compact::run_compact(CompactInput {
        context: ctx.clone(),
        has_tool_calls: false,
    })
    .await
    .expect("Skip 路径不得失败");

    assert!(!output.compacted, "预算充足时不得执行压缩");
    assert_eq!(ctx.model_view.render_count(), 1, "Compact 检查渲染一次");
    assert!(
        ctx.model_view.is_cached(),
        "Skip 路径必须保留缓存槽供 Reason 复用"
    );

    let _ = run_reason(ReasonInput {
        context: ctx.clone(),
        has_tool_calls: false,
    })
    .await;

    assert_eq!(
        ctx.model_view.render_count(),
        1,
        "Skip 之后 Reason 必须复用同一份视图，不得多渲染一次"
    );
    assert!(!ctx.model_view.is_cached(), "取快照后仍须释放缓存槽");
}

/// `before_model` 固定失败的中间件（锁定 hook 错误返回路径的清理）。
struct FailingBeforeModel;

#[async_trait]
impl crate::middleware::Middleware for FailingBeforeModel {
    fn name(&self) -> &str {
        "failing-before-model"
    }

    async fn before_model(
        &self,
        _state: &mut dyn crate::middleware::capabilities::BeforeModelState,
    ) -> crate::error::AgentResult<()> {
        Err(crate::error::AgentError::MiddlewareError {
            middleware: "failing-before-model".to_string(),
            reason: "intentional failure".to_string(),
        })
    }
}

/// 验收：`before_model` 失败返回时，缓存槽不得留整份正文。
///
/// 该返回点在取快照（原先唯一显式 `release` 的位置）之前，靠入口作用域守卫清理。
#[tokio::test]
async fn test_run_reason_releases_view_when_before_model_hook_fails() {
    let cwd: Arc<str> = Arc::from("/tmp/model-view-hook-error");
    let session = Session::new(cwd, FrozenContext::builder().build(), None);
    let mut chain = crate::middleware::MiddlewareChain::new();
    chain.add(Box::new(FailingBeforeModel));
    let turn = session.start_turn();
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_middleware_chain(Arc::new(chain))
        .build();
    ctx.session.transcript.write().append(human("hello"));

    let error = run_reason(ReasonInput {
        context: ctx.clone(),
        has_tool_calls: false,
    })
    .await
    .expect_err("before_model 失败必须终止 Reason");

    assert!(
        matches!(
            &error,
            crate::error::AgentError::MiddlewareError { middleware, reason }
                if middleware == "failing-before-model" && reason == "intentional failure"
        ),
        "必须保留 hook 原始错误，实际 {error:?}"
    );
    assert_eq!(
        ctx.model_view.render_count(),
        1,
        "准备检查在 hook 之前已渲染一份视图"
    );
    assert!(
        !ctx.model_view.is_cached(),
        "hook 错误返回路径不得把整份正文留在缓存槽里"
    );
}

/// 验收：准备阶段遇到取消时，缓存槽不得留整份正文。
///
/// 复现复核的取消用例：取消在准备阶段的第一个 hook 处命中（`refresh` 已填充缓存槽），
/// 该返回点在取快照之前，原先的显式 `release` 覆盖不到。
#[tokio::test]
async fn test_run_reason_releases_view_when_preparation_is_cancelled() {
    let cwd: Arc<str> = Arc::from("/tmp/model-view-prepare-cancel");
    let session = Session::new(cwd, FrozenContext::builder().build(), None);
    let turn = session.start_turn();
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone()).build();
    ctx.session.transcript.write().append(human("hello"));
    ctx.session.turn.cancel_token.cancel();

    let error = run_reason(ReasonInput {
        context: ctx.clone(),
        has_tool_calls: false,
    })
    .await
    .expect_err("取消必须中断 Reason");

    assert!(
        matches!(error, crate::error::AgentError::Interrupted),
        "准备阶段取消必须返回 Interrupted，实际 {error:?}"
    );
    assert_eq!(
        ctx.model_view.render_count(),
        1,
        "准备检查在取消命中前已渲染一份视图"
    );
    assert!(
        !ctx.model_view.is_cached(),
        "提前取消路径不得把整份正文留在缓存槽里"
    );
}
