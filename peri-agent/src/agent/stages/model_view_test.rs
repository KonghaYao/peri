//! 模型视图复用（OPT-03）行为锁定。
//!
//! 目标：同一次 Reason 准备范围内，transcript 修订未变时只渲染一次模型视图；
//! 任何内容变更（消息追加、flag、投影、实例替换、provider 能力变化）都必须重建。

use super::model_view::ModelViewScope;
use super::reason::run_reason;
use super::{ReasonInput, StageContext};

use std::sync::Arc;

use crate::agent::compact_v2::projection::ProviderCapabilities;
use crate::messages::{BaseMessage, MessageContent};
use crate::session::store::FrozenContext;
use crate::session::transcript::MessageTranscript;
use crate::session::Session;

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
