//! 空闲会话首次提交的投递与展示回归测试。
//!
//! 构造器复用父模块 `tests`；单独成文件以控制单文件规模。

use super::tests::{make_command, make_idle_state, make_input, make_snapshot};
use super::*;

#[test]
fn test_steer_idle_submission_timeout_becomes_visible() {
    let mut state = make_idle_state();
    let command = make_command(SteerCommandKind::Enqueue(make_input("b")));
    state.begin(command.clone());
    state.reject(&command, false);
    assert_eq!(state.rows("s")[0].id, "b", "未知回执须保留可见输入");
    assert!(
        state.pending_command("s", "c").is_some(),
        "重试必须保留原命令"
    );
    assert!(state.recover("s", 1, true).is_none(), "不能生成重复提交稿");
}

#[test]
fn test_steer_idle_submission_rejected_recovers_draft() {
    let mut state = make_idle_state();
    let command = make_command(SteerCommandKind::Enqueue(make_input("b")));
    state.begin(command.clone());
    state.reject(&command, true);
    assert_eq!(state.recover("s", 1, true).unwrap().input_id, "b");
    assert!(state.rows("s").is_empty(), "恢复后不留下队列残影");
}

#[test]
fn test_steer_idle_submission_snapshot_without_receipt_stays_direct() {
    let mut state = make_idle_state();
    let command = make_command(SteerCommandKind::Enqueue(make_input("a")));
    state.begin(command.clone());
    state.accept_snapshot(make_snapshot(3), 1, false);
    assert!(
        state.rows("s").is_empty(),
        "暂存通知不是排队回执，空闲提交不得闪过待发送区"
    );
    state.reject(&command, false);
    assert_eq!(
        state.rows("s")[0].state,
        SteerItemState::Submitting,
        "回执超时后才展示未决输入，且不能撤回或重发"
    );
}

#[test]
fn test_steer_idle_submission_queued_receipt_exposes_real_queue() {
    let mut state = make_idle_state();
    let command = make_command(SteerCommandKind::Enqueue(make_input("a")));
    state.begin(command.clone());
    state.accept_snapshot(make_snapshot(3), 1, false);
    state.settle(
        &command,
        UserInputQueueReceipt {
            snapshot: make_snapshot(3),
            results: Vec::new(),
            taken_back: None,
        },
    );
    assert_eq!(state.rows("s")[0].state, SteerItemState::Queued);
    assert!(state.pending_command("s", "c").is_none());
}

#[test]
fn test_steer_idle_submission_staged_snapshot_then_dispatch_skips_queue() {
    let mut state = make_idle_state();
    let command = make_command(SteerCommandKind::Enqueue(make_input("a")));
    state.begin(command.clone());
    state.accept_snapshot(make_snapshot(3), 1, false);
    assert!(state.rows("s").is_empty());
    let mut dispatched = make_snapshot(4);
    dispatched.items[0].state = UserInputState::Dispatching;
    dispatched.active_request_id = Some("run".into());
    state.accept_snapshot(dispatched, 1, false);
    state.settle(
        &command,
        UserInputQueueReceipt {
            snapshot: make_snapshot(3),
            results: Vec::new(),
            taken_back: None,
        },
    );
    assert!(state.rows("s").is_empty(), "旧回执不得恢复暂存行");
    assert!(state.claim_delivery("s", "a"));
    assert!(!state.claim_delivery("s", "a"));
}

#[test]
fn test_steer_second_submission_waits_while_first_is_unconfirmed() {
    let mut state = make_idle_state();
    state.begin(make_command(SteerCommandKind::Enqueue(make_input("a"))));
    let mut second = make_command(SteerCommandKind::Enqueue(make_input("b")));
    second.command_id = "second".into();
    state.begin(second);
    assert_eq!(state.rows("s").len(), 1);
    assert_eq!(state.rows("s")[0].id, "b", "连续输入仍展示等待中的第二条");
}

#[test]
fn test_steer_busy_submission_remains_visible() {
    let mut state = make_idle_state();
    let mut snapshot = state.snapshot("s", 1).unwrap().clone();
    snapshot.revision += 1;
    snapshot.active_request_id = Some("running".into());
    state.accept_snapshot(snapshot, 1, false);
    state.begin(make_command(SteerCommandKind::Enqueue(make_input("b"))));
    assert_eq!(
        state.rows("s")[0].id,
        "b",
        "运行中追加输入应立即显示在待发送区"
    );
}

#[test]
fn test_steer_initial_submission_keeps_direct_projection_after_session_binding() {
    let mut state = SteerState::default();
    let mut command = make_command(SteerCommandKind::Enqueue(make_input("b")));
    command.session_id.clear();
    command.generation = None;
    state.begin(command.clone());
    assert!(state.rows("").is_empty(), "首次创建会话也不应闪过队列");
    state.reset_session("s", 2);
    state.rebind_initial(&command, "s", 2);
    assert!(state.rows("s").is_empty(), "身份绑定不改变直接发送的展示");
}

#[test]
fn test_steer_idle_submission_reload_exposes_unconfirmed_input() {
    let mut state = make_idle_state();
    state.begin(make_command(SteerCommandKind::Enqueue(make_input("b"))));
    state.reset_session("s", 2);
    state.accept_snapshot(make_snapshot(1), 2, true);
    assert_eq!(state.resume_pending("s", 2).len(), 1);
    assert!(
        state.rows("s").iter().any(|row| row.id == "b"),
        "重载后未知输入应可见"
    );
}
