//! 暂停（Stop/Pause/失败）后的恢复语义：暂停期间提交的新输入按新任务发布并自动 Resume，
//! 暂停之前入队的旧待办不取得恢复授权。

use super::staging_tests::{dispatch, input, load, mailbox};
use super::*;
use crate::session::test_resources::TestSession;

#[tokio::test]
async fn explicit_selection_after_pause_resumes_atomically_without_fabricating_attempt() {
    use peri_acp_types::session_resources::ControlStatus;
    let fixture = TestSession::open().await;
    let (mailbox, inbox) = mailbox(&fixture, fixture.resources());
    // 运行中入队的待办在暂停后保持 Queued，等待显式发送带动恢复。
    let ticket = mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let request = input(&mailbox, "STOP_PRESERVED");
    assert_eq!(
        mailbox.enqueue_durable(&request).await.unwrap().results[0].state,
        UserInputState::Queued
    );
    assert!(load(&fixture).await.state.deliveries.is_empty());
    let paused = pause_session(&fixture, &mailbox).await;
    mailbox.finish_attempt(&ticket, UserInputAttemptOutcome::Interrupted);
    let selection = dispatch(&mailbox, vec![request.input_id]);
    mailbox.dispatch_durable(&selection).await.unwrap();
    let resumed = load(&fixture).await;
    assert_eq!(resumed.control.status, ControlStatus::Active);
    assert!(resumed.control.attempt.is_none());
    assert!(resumed.control.control_generation > paused.control_generation);
    assert_eq!(resumed.state.deliveries.len(), 1);
    assert_eq!(inbox.queue().drain_all().len(), 1);
    mailbox.dispatch_durable(&selection).await.unwrap();
    assert_eq!(load(&fixture).await.control, resumed.control);
    assert!(inbox.queue().drain_all().is_empty());
}

/// 模拟 TUI 的 Ctrl+C：控制状态进入 Paused，mailbox 记录暂停。
async fn pause_session(
    fixture: &TestSession,
    mailbox: &UserInputMailbox,
) -> peri_acp_types::session_resources::ControlState {
    use peri_acp_types::session_resources::{ControlAction, ControlCommand, ControlDecision};
    let snapshot = load(fixture).await;
    let paused = fixture
        .resources()
        .apply_session_control(&ControlCommand {
            session_id: fixture.thread_id(),
            command_id: uuid::Uuid::now_v7().to_string(),
            expected_lifecycle: snapshot.control.lifecycle,
            expected_revision: snapshot.control.revision,
            expected_control_generation: snapshot.control.control_generation,
            action: ControlAction::Pause,
        })
        .await
        .unwrap();
    assert_eq!(paused.decision, ControlDecision::Accepted);
    mailbox.stop();
    paused.state
}

/// 停止之后用户的新提交建立新任务，并由发布事务自动解除暂停。
#[tokio::test]
async fn enqueue_after_stop_publishes_new_task_and_resumes() {
    use peri_acp_types::session_resources::ControlStatus;
    let fixture = TestSession::open().await;
    let (mailbox, inbox) = mailbox(&fixture, fixture.resources());
    pause_session(&fixture, &mailbox).await;
    let request = input(&mailbox, "AFTER_STOP");
    let receipt = mailbox.enqueue_durable(&request).await.unwrap();
    assert_ne!(
        receipt.results[0].state,
        UserInputState::Queued,
        "停止后的新提交不能只停在待发区"
    );
    let resumed = load(&fixture).await;
    assert_eq!(resumed.control.status, ControlStatus::Active);
    assert_eq!(resumed.state.deliveries.len(), 1);
    assert_eq!(inbox.queue().drain_all().len(), 1);
}

/// 停止收尾期间到达的新提交先排队，收尾完成后按新任务恢复发布。
#[tokio::test]
async fn enqueue_during_stop_tail_publishes_after_attempt_finishes() {
    use peri_acp_types::session_resources::ControlStatus;
    let fixture = TestSession::open().await;
    let (mailbox, inbox) = mailbox(&fixture, fixture.resources());
    let ticket = mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    pause_session(&fixture, &mailbox).await;
    let request = input(&mailbox, "AFTER_STOP_TAIL");
    let receipt = mailbox.enqueue_durable(&request).await.unwrap();
    assert_eq!(receipt.results[0].state, UserInputState::Queued);
    assert!(inbox.queue().is_empty());
    mailbox.finish_attempt(&ticket, UserInputAttemptOutcome::Interrupted);
    assert!(mailbox.publish_next_durable().await.unwrap());
    let resumed = load(&fixture).await;
    assert_eq!(resumed.control.status, ControlStatus::Active);
    assert_eq!(resumed.state.deliveries.len(), 1);
    assert_eq!(inbox.queue().drain_all().len(), 1);
}

/// 暂停之前入队的旧待办在停止后不被自动带动，保持等待显式发送。
#[tokio::test]
async fn queued_before_stop_is_not_auto_published() {
    let fixture = TestSession::open().await;
    let (mailbox, inbox) = mailbox(&fixture, fixture.resources());
    let ticket = mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let request = input(&mailbox, "BEFORE_STOP");
    let receipt = mailbox.enqueue_durable(&request).await.unwrap();
    assert_eq!(receipt.results[0].state, UserInputState::Queued);
    pause_session(&fixture, &mailbox).await;
    mailbox.finish_attempt(&ticket, UserInputAttemptOutcome::Interrupted);
    assert!(
        !mailbox.publish_next_durable().await.unwrap(),
        "旧待办不得借暂停后的恢复授权自动发布"
    );
    assert!(inbox.queue().is_empty());
    assert!(load(&fixture).await.state.deliveries.is_empty());

    // 恢复授权只属于暂停之后提交的输入：新输入发布，旧待办保持 Queued 等待逐条调度。
    let fresh = input(&mailbox, "AFTER_STOP");
    assert_ne!(
        mailbox.enqueue_durable(&fresh).await.unwrap().results[0].state,
        UserInputState::Queued
    );
    let items = mailbox.snapshot().items;
    let state_of = |id: &str| {
        items
            .iter()
            .find(|item| item.input_id == id)
            .map(|item| item.state)
    };
    assert_eq!(state_of(&request.input_id), Some(UserInputState::Queued));
    assert_ne!(state_of(&fresh.input_id), Some(UserInputState::Queued));
}

/// 恢复授权随首次发布消耗：恢复之后的新输入按普通待办处理，不再带动新任务。
#[tokio::test]
async fn resume_authorization_is_consumed_by_first_publication() {
    use peri_acp_types::session_resources::ControlStatus;
    let fixture = TestSession::open().await;
    let (mailbox, inbox) = mailbox(&fixture, fixture.resources());
    pause_session(&fixture, &mailbox).await;
    let first = input(&mailbox, "RESUME_FIRST");
    mailbox.enqueue_durable(&first).await.unwrap();
    assert_eq!(load(&fixture).await.control.status, ControlStatus::Active);
    assert_eq!(inbox.queue().drain_all().len(), 1);
    let baseline = load(&fixture).await.state.deliveries.len();

    let ticket = mailbox
        .attach_external_attempt(CancellationToken::new(), false)
        .unwrap();
    let later = input(&mailbox, "AFTER_RESUME");
    let receipt = mailbox.enqueue_durable(&later).await.unwrap();
    assert_eq!(receipt.results[0].state, UserInputState::Queued);
    mailbox.finish_attempt(&ticket, UserInputAttemptOutcome::Interrupted);
    assert!(
        !mailbox.publish_next_durable().await.unwrap(),
        "恢复之后入队的待办不得沿用上一轮恢复授权"
    );
    assert!(inbox.queue().is_empty());
    assert_eq!(load(&fixture).await.state.deliveries.len(), baseline);
}
