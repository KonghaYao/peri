//! 请求协调原语的确定性单测：纯同步，不使用时钟、tick 或全局状态。
//!
//! 面板侧的行为证据（切会话、关闭重开、Esc 取消等真实任务路径）留在各面板测试里；
//! 这里只固定原语自身的契约。

use super::gate::CompletionGate;
use super::outcome::{OperationOutcome, OutcomeUnknownReason};
use super::scope::OperationScope;

fn scope(session_id: &str, reset: u64) -> OperationScope {
    OperationScope::new(session_id, reset)
}

#[test]
fn accepted_ticket_completes_once_and_clears_pending() {
    let mut gate = CompletionGate::default();
    let ticket = gate.begin(scope("session-a", 0));
    assert!(gate.is_pending());

    assert!(gate.accept(&ticket), "当前在途票必须被接受");
    assert!(!gate.is_pending());

    // 同一次请求只发布一次：重复完成不得再次写入面板状态。
    assert!(!gate.accept(&ticket), "同一张票不能重复通过完成门");
}

#[test]
fn stale_ticket_from_a_superseded_request_is_rejected() {
    let mut gate = CompletionGate::default();
    let first = gate.begin(scope("session-a", 0));
    let second = gate.begin(scope("session-a", 0));

    assert!(second.generation() > first.generation(), "取票必须推进代次");
    assert!(
        first.is_cancelled(),
        "被替代的请求必须收到取消，异步侧才能退出等待"
    );
    assert!(!gate.accept(&first), "旧代次的晚到结果不得落地");
    assert!(gate.accept(&second), "被替代后，当前代次仍然可以完成");
}

#[test]
fn ticket_from_another_scope_cannot_publish_into_the_current_one() {
    let mut gate = CompletionGate::default();
    let in_a = gate.begin(scope("session-a", 0));

    // 会话边界变化：A 的在途请求不再属于当前作用域。
    assert!(gate.reset_scope(scope("session-b", 0)));
    assert!(!gate.is_pending(), "切会话必须丢弃在途等待");
    let in_b = gate.begin(scope("session-b", 0));

    assert!(gate.scope_matches(&scope("session-b", 0)));
    assert!(!gate.scope_matches(&scope("session-a", 0)));
    assert!(!gate.accept(&in_a), "A 会话的响应不得写入 B 会话状态");
    assert!(gate.accept(&in_b));
}

#[test]
fn same_scope_is_a_no_op_so_a_pending_request_survives_projection_frames() {
    let mut gate = CompletionGate::default();
    let ticket = gate.begin(scope("session-a", 7));

    assert!(
        !gate.reset_scope(scope("session-a", 7)),
        "作用域未变化不该报告变化"
    );
    assert!(gate.is_pending(), "重复投影同一会话不得取消在途请求");
    assert!(gate.accept(&ticket), "未受干扰的在途票仍可完成");
}

#[test]
fn reset_boundary_change_is_its_own_scope() {
    let mut gate = CompletionGate::default();
    let ticket = gate.begin(scope("session-a", 3));

    // 同会话重载（前端重置代次变化）与切会话同等对待：旧投影不得复活。
    assert!(gate.reset_scope(scope("session-a", 4)));
    assert!(!gate.accept(&ticket));
}

#[test]
fn cancel_drops_pending_and_cancels_the_ticket() {
    let mut gate = CompletionGate::default();
    let ticket = gate.begin(scope("session-a", 0));

    gate.cancel();

    assert!(!gate.is_pending());
    assert!(ticket.is_cancelled(), "取消必须传递到异步侧");
    assert!(!gate.accept(&ticket), "停止等待后的结果不得当作完成");
}

#[test]
fn rejected_ticket_leaves_the_pending_state_untouched() {
    let mut gate = CompletionGate::default();
    let stale = gate.begin(scope("session-a", 0));
    let current = gate.begin(scope("session-a", 0));

    assert!(!gate.accept(&stale));

    assert!(gate.is_pending(), "拒收旧票不得清掉在途状态");
    assert_eq!(
        gate.pending_generation(),
        Some(current.generation()),
        "在途的仍是当前那一次请求"
    );
    assert!(gate.accept(&current));
}

#[test]
fn dropping_the_gate_invalidates_the_pending_ticket() {
    let mut gate = CompletionGate::default();
    let ticket = gate.begin(scope("session-a", 0));

    drop(gate);

    assert!(
        ticket.is_cancelled(),
        "面板关闭后旧票必须失效，重开的面板不能接收它"
    );
}

#[test]
fn cancelled_wait_is_unknown_outcome_not_success() {
    // 取消只说明本视图不再等待；契约要求它表达成「结果未知」，而不是成功，
    // 也不是「已撤销」。判定依据是枚举，不依赖任何字符串文案。
    let cancelled = OperationOutcome::OutcomeUnknown {
        reason: OutcomeUnknownReason::WaitCancelled,
    };
    assert_ne!(cancelled, OperationOutcome::Succeeded);
    assert_eq!(
        cancelled,
        OperationOutcome::OutcomeUnknown {
            reason: OutcomeUnknownReason::WaitCancelled
        }
    );
    assert_ne!(
        OutcomeUnknownReason::WaitCancelled,
        OutcomeUnknownReason::ObserverGone
    );
    assert_ne!(
        OutcomeUnknownReason::ObserverGone,
        OutcomeUnknownReason::TimedOut
    );
}

#[test]
fn timed_out_outcome_keeps_its_own_reason_and_is_not_a_failure() {
    let timed_out = OperationOutcome::OutcomeUnknown {
        reason: OutcomeUnknownReason::TimedOut,
    };
    assert_ne!(
        timed_out,
        OperationOutcome::Failed {
            reason: "session RPC outcome unknown: timed out".into()
        },
        "超时表示未确认，不等于服务端明确拒绝"
    );
    assert_ne!(
        timed_out,
        OperationOutcome::OutcomeUnknown {
            reason: OutcomeUnknownReason::ObserverGone
        },
        "来源必须可区分，便于展示与后续重查"
    );
}
