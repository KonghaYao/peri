use super::*;
use peri_acp_types::messages::MessageContent;

#[test]
fn test_steer_projection_rejects_old_revision_and_generation() {
    let mut state = make_state();
    let mut other_generation = make_snapshot(8);
    other_generation.generation = "old-instance".to_owned();
    assert!(
        !state.accept_snapshot(make_snapshot(0), 1, false),
        "旧 revision 不得覆盖当前投影"
    );
    assert!(
        !state.accept_snapshot(other_generation, 1, true),
        "不同实例回执不得覆盖当前投影"
    );
    assert_eq!(
        state.snapshot("s", 1).unwrap().revision,
        1,
        "当前 revision 应保留"
    );
}

#[test]
fn test_steer_projection_rejects_same_revision_with_different_content() {
    let mut state = make_state();
    let mut stale = make_snapshot(1);
    stale.items[0].content = MessageContent::text("旧查询内容");
    stale.items[0].original_draft = "旧查询内容".to_owned();
    assert!(
        !state.accept_snapshot(stale, 1, false),
        "同 revision 的不同内容不能覆盖当前投影"
    );
    assert_eq!(
        state.snapshot("s", 1).unwrap().items[0].content,
        make_snapshot(1).items[0].content,
        "当前内容必须保留"
    );
    assert!(
        state.accept_snapshot(make_snapshot(1), 1, false),
        "同 revision 的同一投影重复到达是幂等的"
    );
    assert!(
        !state.accept_snapshot(make_snapshot(0), 1, false),
        "严格更旧仍须拒绝"
    );
}

#[test]
fn test_steer_projection_session_reset_rejects_old_epoch() {
    let mut state = make_state();
    state.reset_session("s", 2);
    assert!(
        !state.accept_snapshot(make_snapshot(99), 1, true),
        "相同 sessionId 重载仍要拒绝旧 epoch"
    );
    assert!(state.snapshot("s", 2).is_none(), "旧回执不得重新建立实例");
}

#[test]
fn test_steer_projection_notifications_need_established_generation() {
    let mut state = SteerState::default();
    state.reset_session("s", 1);
    assert!(
        !state.accept_snapshot(make_snapshot(4), 1, false),
        "首个通知不能代替可信 snapshot 查询"
    );
    assert!(
        state.accept_snapshot(make_snapshot(5), 1, true),
        "明确查询结果可建立实例"
    );
}
