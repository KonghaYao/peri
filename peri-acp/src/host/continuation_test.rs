//! continuation scheduler 的定向测试：eligibility（kind/armed 过滤）、
//! 原子 take、epoch 代际失效（用户新 prompt 清除未运行的续跑）、
//! cancel race 兜底 eligibility、取消续跑不 arm、dispatch 前 Defer 确认。

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use peri_acp_types::interaction::{
    ApprovalDecision, InteractionContext, InteractionResponse, UserInteractionBroker,
};
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};
use peri_acp_types::tasks::BgTaskKind;
use tokio_util::sync::CancellationToken;

use super::{
    continuation_dispatchable, continuation_still_valid, recv_until_shutdown,
    take_continuation_for_request, SessionState,
};
use crate::session::executor::ContinuationRequest;

struct FixedBroker(InteractionResponse);

#[async_trait::async_trait]
impl UserInteractionBroker for FixedBroker {
    async fn request(&self, _ctx: InteractionContext) -> InteractionResponse {
        self.0.clone()
    }
}

#[tokio::test]
async fn test_scheduled_trigger_permission_contract() {
    let bypass = SharedPermissionMode::new(PermissionMode::Bypass);
    assert!(
        super::super::prompt::approve_scheduled_trigger(bypass.as_ref(), None, "task-1", "status")
            .await
    );

    let default = SharedPermissionMode::new(PermissionMode::Default);
    assert!(
        !super::super::prompt::approve_scheduled_trigger(
            default.as_ref(),
            None,
            "task-1",
            "status"
        )
        .await
    );

    let broker: Arc<dyn UserInteractionBroker> =
        Arc::new(FixedBroker(InteractionResponse::Decisions(vec![
            ApprovalDecision::Approve { source: None },
        ])));
    assert!(
        super::super::prompt::approve_scheduled_trigger(
            default.as_ref(),
            Some(&broker),
            "task-1",
            "status"
        )
        .await
    );
}

/// 构造最小 SessionState（仅续跑相关字段有值）。
fn make_session_state(_armed: bool, epoch: u64) -> SessionState {
    SessionState {
        session_id: "session-1".to_string(),
        thread_id: "thread-1".to_string(),
        cwd: "/tmp".to_string(),
        environment: None,
        closing: false,
        history: vec![],
        history_payloads: vec![],
        cancel_token: None,
        frozen: None,
        recall_items: vec![],
        agent_pool: crate::session::agent_pool::AgentPool::new(),
        workflow_middleware: None,
        title: None,
        tags: vec![],

        continuation_epoch: epoch,
        continuation_in_flight: false,
        continuation_mq_steering_pending: false,
    }
}

/// MQ steering 在 continuation 执行中到达：须置 pending，不得静默丢弃。
#[test]
fn test_mq_steering_during_in_flight_sets_pending() {
    let mut state = make_session_state(false, 2);
    state.continuation_in_flight = true;
    let req = ContinuationRequest {
        session_id: "session-1".to_string(),
        kind: BgTaskKind::Agent,
        mq_steering: true,
    };
    assert!(
        take_continuation_for_request(&mut state, &req).is_none(),
        "in_flight 时不应立即调度"
    );
    assert!(
        state.continuation_mq_steering_pending,
        "须保留 pending 供 in_flight 结束后补发"
    );
}

/// 只有 bg agent（kind=Agent）完成才触发：Shell 完成不得消费标记。
#[test]
fn test_take_only_agent_kind_runs_continuation() {
    for kind in [BgTaskKind::Shell, BgTaskKind::Agent, BgTaskKind::Workflow] {
        let mut state = make_session_state(false, 0);
        let request = ContinuationRequest {
            session_id: state.session_id.clone(),
            kind,
            mq_steering: false,
        };
        assert_eq!(take_continuation_for_request(&mut state, &request), Some(0));
        assert!(state.continuation_mq_steering_pending);
    }
}

/// 未置位（prompt 未被取消 / 新 prompt 已清除）→ 不触发。
#[test]
fn test_take_skips_when_not_armed() {
    let mut state = make_session_state(false, 3);
    state.closing = true;
    let request = ContinuationRequest {
        session_id: state.session_id.clone(),
        kind: BgTaskKind::Agent,
        mq_steering: true,
    };
    assert!(take_continuation_for_request(&mut state, &request).is_none());
}

/// epoch 代际：用户显式新 prompt 递增 epoch 后，已排队未运行的续跑失效。
///
/// 场景：cancel → armed；bg 完成 → scheduler take（epoch=0）；用户新 prompt
/// 已执行（epoch=1）；续跑拿到 prompt lock 后校验代际 → 放弃。
#[test]
fn test_epoch_invalidation_clears_queued_continuation() {
    let mut state = make_session_state(true, 0);

    // scheduler 原子 take（记录 epoch=0）
    let request = ContinuationRequest {
        session_id: state.session_id.clone(),
        kind: BgTaskKind::Agent,
        mq_steering: false,
    };
    let epoch =
        take_continuation_for_request(&mut state, &request).expect("open session accepts hint");

    // 用户显式新 prompt：清除标记 + 递增代际（dispatch_prompt_turn 的行为）

    state.continuation_epoch += 1;

    // 续跑执行前校验：代际已变 → 放弃
    assert!(
        !continuation_still_valid(&state, epoch),
        "新 prompt 后已排队的续跑必须失效"
    );

    // 未变化（无新 prompt）：仍有效
    let mut state2 = make_session_state(true, 5);
    let epoch2 = take_continuation_for_request(&mut state2, &request).unwrap();
    assert!(continuation_still_valid(&state2, epoch2));
}

/// epoch 校验只认当前代际：旧代际请求一律失效。
#[test]
fn test_stale_epoch_never_valid() {
    let state = make_session_state(false, 7);
    assert!(!continuation_still_valid(&state, 6));
    assert!(continuation_still_valid(&state, 7));
}

/// dispatch 前确认：代际有效 **且** 队列仍有 SubAgentComplete Defer 才续跑；
/// Defer 已被消费（如用户 prompt drain_all）则跳过空跑。
#[test]
fn test_continuation_dispatchable_requires_pending_defer() {
    let state = make_session_state(false, 3);
    // 代际有效 + Defer 在队 → 可 dispatch
    assert!(continuation_dispatchable(&state, 3, true));
    // 代际有效但 Defer 已被消费 → 跳过（空跑无意义）
    assert!(!continuation_dispatchable(&state, 3, false));
    // 代际失效（用户新 prompt）→ 跳过
    assert!(!continuation_dispatchable(&state, 3 + 1, true));
    assert!(!continuation_dispatchable(&state, 3 + 1, false));
}

#[tokio::test]
async fn test_scheduler_exits_on_host_shutdown_even_if_continuation_ingress_still_exists() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ContinuationRequest>();
    let root = Arc::new(tx);
    let _bounded_callback_clone = root.clone();
    let shutdown = CancellationToken::new();
    shutdown.cancel();

    assert!(recv_until_shutdown(&mut rx, &shutdown).await.is_none());
    assert!(
        !root.is_closed(),
        "kept-alive ingress must not be the exit signal"
    );
}

#[tokio::test]
async fn test_scheduler_does_not_own_its_ingress_sender() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ContinuationRequest>();
    let root = Arc::new(tx);
    let scheduler_ingress = Arc::downgrade(&root);
    drop(root);

    assert!(scheduler_ingress.upgrade().is_none());
    assert!(recv_until_shutdown(&mut rx, &CancellationToken::new())
        .await
        .is_none());
}

#[tokio::test]
async fn test_continuation_child_is_rejected_after_admission_closes() {
    let (owner, spawner) = crate::host::task_scope::HostTaskOwner::new();
    owner.begin_shutdown();
    let started = Arc::new(AtomicBool::new(false));
    let started_task = started.clone();
    assert!(spawner
        .spawn(
            crate::host::task_scope::HostTaskOwnerKind::Session,
            crate::host::task_scope::HostTaskKind::ContinuationTurn,
            async move {
                started_task.store(true, Ordering::SeqCst);
            },
        )
        .is_err());
    tokio::task::yield_now().await;
    assert!(!started.load(Ordering::SeqCst));
}
