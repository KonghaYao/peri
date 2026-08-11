//! continuation scheduler 的定向测试：eligibility（kind/armed 过滤）、
//! 原子 take、epoch 代际失效（用户新 prompt 清除未运行的续跑）、
//! cancel race 兜底 eligibility、取消续跑不 arm、dispatch 前 Defer 确认。

use peri_acp_types::tasks::BgTaskKind;

use super::{
    cancel_arms_continuation, cancel_should_schedule_continuation, continuation_dispatchable,
    continuation_still_valid, take_continuation_if_armed, SessionState,
};

/// 构造最小 SessionState（仅续跑相关字段有值）。
fn make_session_state(armed: bool, epoch: u64) -> SessionState {
    SessionState {
        session_id: "session-1".to_string(),
        thread_id: "thread-1".to_string(),
        cwd: "/tmp".to_string(),
        history: vec![],
        cancel_token: None,
        frozen: None,
        recall_items: vec![],
        agent_pool: crate::session::agent_pool::AgentPool::new(),
        workflow_middleware: None,
        lsp_pool: None,
        title: None,
        tags: vec![],
        continuation_armed: armed,
        continuation_epoch: epoch,
        continuation_in_flight: false,
        lease: crate::host::lease::WriterLease::acquired("default"),
    }
}

/// 只有 bg agent（kind=Agent）完成才触发：Shell 完成不得消费标记。
#[test]
fn test_take_only_agent_kind_runs_continuation() {
    let mut state = make_session_state(true, 0);

    // shell 完成：即使标记已置位也不触发，标记保持
    assert!(
        take_continuation_if_armed(&mut state, BgTaskKind::Shell).is_none(),
        "bg shell 完成不得触发续跑"
    );
    assert!(state.continuation_armed, "shell 完成不应消费取消标记");

    // agent 完成：原子 take，返回 epoch
    assert_eq!(
        take_continuation_if_armed(&mut state, BgTaskKind::Agent),
        Some(0)
    );
    assert!(!state.continuation_armed, "take 后标记必须清除");

    // 同轮次后续 agent 完成：标记已清除，不再重复续跑（每 session coalesce）
    assert!(
        take_continuation_if_armed(&mut state, BgTaskKind::Agent).is_none(),
        "同一取消轮次只运行一次续跑"
    );
}

/// 未置位（prompt 未被取消 / 新 prompt 已清除）→ 不触发。
#[test]
fn test_take_skips_when_not_armed() {
    let mut state = make_session_state(false, 3);
    assert!(take_continuation_if_armed(&mut state, BgTaskKind::Agent).is_none());
    assert!(!state.continuation_armed);
}

/// epoch 代际：用户显式新 prompt 递增 epoch 后，已排队未运行的续跑失效。
///
/// 场景：cancel → armed；bg 完成 → scheduler take（epoch=0）；用户新 prompt
/// 已执行（epoch=1）；续跑拿到 prompt lock 后校验代际 → 放弃。
#[test]
fn test_epoch_invalidation_clears_queued_continuation() {
    let mut state = make_session_state(true, 0);

    // scheduler 原子 take（记录 epoch=0）
    let epoch = take_continuation_if_armed(&mut state, BgTaskKind::Agent).expect("armed 应可 take");

    // 用户显式新 prompt：清除标记 + 递增代际（dispatch_prompt_turn 的行为）
    state.continuation_armed = false;
    state.continuation_epoch += 1;

    // 续跑执行前校验：代际已变 → 放弃
    assert!(
        !continuation_still_valid(&state, epoch),
        "新 prompt 后已排队的续跑必须失效"
    );

    // 未变化（无新 prompt）：仍有效
    let mut state2 = make_session_state(true, 5);
    let epoch2 = take_continuation_if_armed(&mut state2, BgTaskKind::Agent).unwrap();
    assert!(continuation_still_valid(&state2, epoch2));
}

/// epoch 校验只认当前代际：旧代际请求一律失效。
#[test]
fn test_stale_epoch_never_valid() {
    let state = make_session_state(false, 7);
    assert!(!continuation_still_valid(&state, 6));
    assert!(continuation_still_valid(&state, 7));
}

/// 取消正在执行的 continuation 不置位 armed（防自动链式续跑）。
#[test]
fn test_cancel_does_not_arm_in_flight_continuation() {
    // 续跑执行中（in_flight）：cancel 不 arm
    let mut state = make_session_state(false, 1);
    state.continuation_in_flight = true;
    assert!(
        !cancel_arms_continuation(&state),
        "取消续跑本身不得 arm 自动链式续跑"
    );
    // 模拟 notify cancel 分支：in_flight 时不写 armed
    if cancel_arms_continuation(&state) {
        state.continuation_armed = true;
    }
    assert!(
        !state.continuation_armed,
        "in_flight 时 cancel 不得置位 armed"
    );

    // 普通 prompt 运行中（非续跑）：cancel 正常 arm
    let state2 = make_session_state(false, 1);
    assert!(cancel_arms_continuation(&state2), "普通 prompt 取消应 arm");
}

/// cancel ↔ bg callback race 兜底的 eligibility（纯逻辑）：
/// 仅当"会置位 armed（非 in_flight）且队列有 pending SubAgentComplete Defer"
/// 时，cancel 才补发一次 continuation 请求。
#[test]
fn test_cancel_schedule_race_eligibility() {
    // 典型 race：bg 结果已 route（Defer 在队列），cancel 需补发
    let state = make_session_state(false, 1);
    assert!(
        cancel_should_schedule_continuation(&state, true),
        "cancel 置位前 bg 已完成且 Defer 已入队 → 必须补发"
    );

    // 无 pending Defer：bg 尚未完成，等其完成通知即可，不补发
    assert!(
        !cancel_should_schedule_continuation(&state, false),
        "队列无 Defer 时补发会导致空跑续跑"
    );

    // 取消的是续跑本身（in_flight）：即使有 Defer 也不补发（不链式）
    let mut in_flight = make_session_state(false, 1);
    in_flight.continuation_in_flight = true;
    assert!(
        !cancel_should_schedule_continuation(&in_flight, true),
        "取消续跑不得触发补发"
    );
    assert!(!cancel_should_schedule_continuation(&in_flight, false));
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
