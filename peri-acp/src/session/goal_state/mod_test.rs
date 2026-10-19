use super::*;
use peri_acp_types::goal::InMemoryGoalStore;
use peri_acp_types::session::{
    MessageKind, MessageQueue, MessageSource, QueuedMessage, QueuedPayload,
};
use std::sync::Arc;
use std::time::Duration;

fn make_state() -> GoalState {
    GoalState::new(
        Arc::new(InMemoryGoalStore::new()),
        "test-thread".to_string(),
    )
}

fn make_recovery_state(interval: Duration) -> (GoalState, MessageQueue) {
    let queue = MessageQueue::new();
    let state = GoalState::with_recovery(
        Arc::new(InMemoryGoalStore::new()),
        "recovery-thread".to_string(),
        queue.clone(),
        interval,
    );
    (state, queue)
}

/// 等待调度器投递下一条 goal recovery 消息（测试用短间隔）。
async fn next_recovery(queue: &MessageQueue) -> QueuedMessage {
    peri_time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(message) = queue
                .drain_all()
                .into_iter()
                .find(|message| message.source == MessageSource::GoalSteering)
            {
                return message;
            }
            peri_time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("等待 blocked goal 恢复提醒超时")
}

#[tokio::test]
async fn test_set_goal_写入_store_并触发_objective_updated() {
    let state = make_state();
    state
        .set_goal("完成模块重构".to_string(), Some(200_000))
        .await
        .unwrap();

    let snap = state.snapshot();
    assert_eq!(snap.objective.as_deref(), Some("完成模块重构"));
    assert_eq!(snap.token_budget, Some(200_000));
    assert_eq!(snap.status, Some(GoalStatus::Active));
    assert!(snap.objective_just_updated);
}

#[tokio::test]
async fn test_clear_清空_goal() {
    let state = make_state();
    state.set_goal("临时目标".to_string(), None).await.unwrap();
    state.clear().await.unwrap();

    let snap = state.snapshot();
    assert!(snap.objective.is_none());
    assert!(!snap.objective_just_updated);
}

#[tokio::test]
async fn test_set_goal_覆盖旧_goal_生成新_goal_id() {
    let state = make_state();
    state.set_goal("目标 A".to_string(), None).await.unwrap();
    let id_a = state.snapshot().goal_id.clone().unwrap();

    state.set_goal("目标 B".to_string(), None).await.unwrap();
    let id_b = state.snapshot().goal_id.clone().unwrap();

    assert_ne!(id_a, id_b);
    assert_eq!(state.snapshot().objective.as_deref(), Some("目标 B"));
}

#[tokio::test]
async fn test_store_写入失败_内存镜像仍可读() {
    use async_trait::async_trait;
    use peri_acp_types::goal::{GoalStore, GoalStoreError, ThreadGoal};

    struct FailingStore;
    #[async_trait]
    impl GoalStore for FailingStore {
        async fn save(&self, _: &str, _: ThreadGoal) -> Result<(), GoalStoreError> {
            Err(GoalStoreError::Io("simulated".to_string()))
        }
        async fn load(&self, _: &str) -> Result<Option<ThreadGoal>, GoalStoreError> {
            Err(GoalStoreError::Io("simulated".to_string()))
        }
        async fn delete(&self, _: &str) -> Result<(), GoalStoreError> {
            Err(GoalStoreError::Io("simulated".to_string()))
        }
    }

    let state = GoalState::new(Arc::new(FailingStore), "test-thread".to_string());
    // set_goal 即使 store 失败也不 panic（内存镜像更新成功）
    let result = state.set_goal("fallback".to_string(), None).await;
    // store 失败返回 Err，但内存镜像已更新
    assert!(result.is_err());
    assert_eq!(state.snapshot().objective.as_deref(), Some("fallback"));
}

#[tokio::test]
async fn test_set_status_合法转换_active_to_blocked() {
    let state = make_state();
    state.set_goal("测试".to_string(), None).await.unwrap();
    assert_eq!(state.snapshot().status, Some(GoalStatus::Active));

    state
        .set_status_with_reason(GoalStatus::Blocked, "缺少依赖".to_string())
        .await
        .unwrap();
    assert_eq!(state.snapshot().status, Some(GoalStatus::Blocked));
}

#[tokio::test]
async fn test_set_status_非法转换_complete_to_active_返回错误() {
    let state = make_state();
    state.set_goal("测试".to_string(), None).await.unwrap();
    state.set_status(GoalStatus::Complete).await.unwrap();

    let result = state.set_status(GoalStatus::Active).await;
    assert!(result.is_err());
    // 状态未改变
    assert_eq!(state.snapshot().status, Some(GoalStatus::Complete));
}

#[tokio::test]
async fn test_set_status_blocked_必须附带_reason() {
    let state = make_state();
    state.set_goal("测试".to_string(), None).await.unwrap();

    let result = state.set_status(GoalStatus::Blocked).await;
    assert!(result.is_err(), "Blocked 必须附带 reason");

    state
        .set_status_with_reason(GoalStatus::Blocked, "缺少依赖".to_string())
        .await
        .unwrap();
    assert_eq!(state.snapshot().status, Some(GoalStatus::Blocked));
}

#[tokio::test]
async fn test_set_status_无_goal_返回错误() {
    let state = make_state();
    let result = state.set_status(GoalStatus::Complete).await;
    assert!(result.is_err(), "无 goal 时 set_status 应失败");
}

#[tokio::test]
async fn test_resume_from_complete_返回错误() {
    let state = make_state();
    state.set_goal("测试".to_string(), None).await.unwrap();
    state.set_status(GoalStatus::Complete).await.unwrap();

    // Complete 是终态，不能 resume
    let result = state.set_status(GoalStatus::Active).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_put_pending_user_message_覆盖旧值() {
    let state = make_state();
    state.set_goal("测试".to_string(), None).await.unwrap();

    state.put_pending_user_message("第一条".to_string());
    state.put_pending_user_message("第二条".to_string());

    let taken = state.take_pending_user_message();
    assert_eq!(taken.as_deref(), Some("第二条"));
    // take 后清空
    assert!(state.take_pending_user_message().is_none());
}

#[tokio::test]
async fn test_clear_goal_清零_pending_user_message() {
    let state = make_state();
    state.set_goal("测试".to_string(), None).await.unwrap();
    state.put_pending_user_message("待清空".to_string());

    state.clear().await.unwrap();
    assert!(state.take_pending_user_message().is_none());
}

#[tokio::test]
async fn test_set_status_complete_清零_pending_user_message() {
    let state = make_state();
    state.set_goal("测试".to_string(), None).await.unwrap();
    state.put_pending_user_message("待清空".to_string());

    state.set_status(GoalStatus::Complete).await.unwrap();
    assert!(state.take_pending_user_message().is_none());
}

#[tokio::test]
async fn test_set_status_blocked_清零_pending_user_message() {
    let state = make_state();
    state.set_goal("测试".to_string(), None).await.unwrap();
    state.put_pending_user_message("待清空".to_string());

    state
        .set_status_with_reason(GoalStatus::Blocked, "阻塞原因".to_string())
        .await
        .unwrap();
    assert!(state.take_pending_user_message().is_none());
}

#[tokio::test]
async fn test_record_token_usage_累积到_pending() {
    let state = make_state();
    state
        .set_goal("测试".to_string(), Some(200_000))
        .await
        .unwrap();

    state.record_token_usage(1000);
    state.record_token_usage(500);

    // pending 累积 1500，但 snapshot 还没 flush
    // snapshot 读取的是已 flush 的值，所以仍是 0
    assert_eq!(state.snapshot().tokens_used, 0);
}

#[tokio::test]
async fn test_flush_progress_写入_goal_accounting() {
    let state = make_state();
    state
        .set_goal("测试".to_string(), Some(200_000))
        .await
        .unwrap();

    state.record_token_usage(1500);
    state.flush_progress().await.unwrap();

    assert_eq!(state.snapshot().tokens_used, 1500);
}

#[tokio::test]
async fn test_flush_progress_多次累加() {
    let state = make_state();
    state
        .set_goal("测试".to_string(), Some(200_000))
        .await
        .unwrap();

    state.record_token_usage(1000);
    state.flush_progress().await.unwrap();
    state.record_token_usage(500);
    state.flush_progress().await.unwrap();

    assert_eq!(state.snapshot().tokens_used, 1500);
}

#[tokio::test]
async fn test_record_time_usage_累积并_flush() {
    let state = make_state();
    state.set_goal("测试".to_string(), None).await.unwrap();

    state.record_time_usage(30);
    state.record_time_usage(15);
    state.flush_progress().await.unwrap();

    assert_eq!(state.snapshot().time_used_seconds, 45);
}

#[tokio::test]
async fn test_usage_pct_基于_flushed_值() {
    let state = make_state();
    state
        .set_goal("测试".to_string(), Some(200_000))
        .await
        .unwrap();

    state.record_token_usage(160_000);
    state.flush_progress().await.unwrap();

    let snap = state.snapshot();
    assert!((snap.usage_pct().unwrap() - 0.8).abs() < 0.01);
}

#[tokio::test]
async fn test_increment_continuation_updates_snapshot() {
    let state = make_state();
    state.set_goal("测试".to_string(), None).await.unwrap();

    GoalController::increment_continuation(&state)
        .await
        .unwrap();
    GoalController::increment_continuation(&state)
        .await
        .unwrap();

    let snapshot = state.snapshot();
    assert_eq!(snapshot.continuation_count, 2);
}

// ---- GoalController trait 实现测试 ----

use peri_acp_types::goal::{GoalController, GoalStateView, GoalStore};

#[tokio::test]
async fn test_goal_controller_create_duplicate_errors() {
    let store = Arc::new(InMemoryGoalStore::new()) as Arc<dyn GoalStore>;
    let state = GoalState::new(store, "thread-1".to_string());

    // 第一次 create 成功
    state.create_goal("测试目标".to_string()).await.unwrap();

    // 第二次 create 报错
    let result = state.create_goal("另一个目标".to_string()).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("已存在"));
}

#[tokio::test]
async fn test_goal_controller_complete() {
    let store = Arc::new(InMemoryGoalStore::new()) as Arc<dyn GoalStore>;
    let state = GoalState::new(store, "thread-1".to_string());

    state.create_goal("测试目标".to_string()).await.unwrap();
    state.complete_goal().await.unwrap();

    let snap = state.snapshot();
    assert_eq!(snap.status, Some(GoalStatus::Complete));
}

#[tokio::test]
async fn test_goal_controller_block_requires_reason() {
    let store = Arc::new(InMemoryGoalStore::new()) as Arc<dyn GoalStore>;
    let state = GoalState::new(store, "thread-1".to_string());

    state.create_goal("测试目标".to_string()).await.unwrap();
    state.block_goal("缺权限".to_string()).await.unwrap();

    let snap = state.snapshot();
    assert_eq!(snap.status, Some(GoalStatus::Blocked));
}

#[tokio::test]
async fn test_goal_controller_terminal_cannot_transition() {
    let store = Arc::new(InMemoryGoalStore::new()) as Arc<dyn GoalStore>;
    let state = GoalState::new(store, "thread-1".to_string());

    state.create_goal("测试目标".to_string()).await.unwrap();
    state.complete_goal().await.unwrap();

    // 终态不能再转
    let result = state.complete_goal().await;
    assert!(result.is_err());
}

// ---- blocked goal 周期恢复 ----

#[tokio::test]
async fn test_blocked_goal_reopens_and_enqueues_recovery_reminder() {
    let (state, queue) = make_recovery_state(Duration::from_millis(30));
    state.create_goal("恢复网络任务".to_string()).await.unwrap();
    state
        .block_goal("network outage".to_string())
        .await
        .unwrap();
    assert_eq!(state.snapshot().status, Some(GoalStatus::Blocked));

    let message = next_recovery(&queue).await;

    let snap = state.snapshot();
    assert_eq!(snap.status, Some(GoalStatus::Active), "恢复尝试应重开 goal");
    assert!(snap.blocked_reason.is_none(), "重开后不应残留阻塞原因");
    assert_eq!(message.kind, MessageKind::Defer);
    assert!(
        message.policy.ensures_processing(),
        "恢复提醒必须可唤醒会话执行"
    );
    let QueuedPayload::SystemReminder(reminder) = &message.payload else {
        panic!("恢复提醒必须是 canonical system reminder");
    };
    let reminder = reminder.as_reminder();
    assert_eq!(reminder.kind, "recovery");
    assert!(reminder.body.contains("network outage"));
    assert!(reminder.body.contains("恢复网络任务"));
    assert_eq!(reminder.metadata["recovery_attempt"], 1);
}

#[tokio::test]
async fn test_recovery_repeats_until_acknowledged() {
    let (state, queue) = make_recovery_state(Duration::from_millis(30));
    state.create_goal("持续阻断".to_string()).await.unwrap();
    state.block_goal("provider down".to_string()).await.unwrap();

    let first = next_recovery(&queue).await;
    // 第一轮唤醒未被确认（模拟模型调用仍失败）：下一周期必须投递新消息。
    let second = next_recovery(&queue).await;
    let first = match &first.payload {
        QueuedPayload::SystemReminder(reminder) => reminder.as_reminder().metadata
            ["recovery_attempt"]
            .as_u64()
            .unwrap(),
        _ => panic!("expected reminder"),
    };
    let second = match &second.payload {
        QueuedPayload::SystemReminder(reminder) => reminder.as_reminder().metadata
            ["recovery_attempt"]
            .as_u64()
            .unwrap(),
        _ => panic!("expected reminder"),
    };
    assert_eq!(first, 1);
    assert_eq!(second, 2, "未确认时必须继续按周期重试");

    GoalController::acknowledge_recovery(&state).await;
    peri_time::sleep(Duration::from_millis(120)).await;
    assert!(queue.is_empty(), "确认恢复后不应再投递提醒");
    assert_eq!(state.snapshot().status, Some(GoalStatus::Active));
}

#[tokio::test]
async fn test_clear_stops_recovery_schedule() {
    let (state, queue) = make_recovery_state(Duration::from_millis(20));
    state.create_goal("放弃目标".to_string()).await.unwrap();
    state.block_goal("缺权限".to_string()).await.unwrap();
    state.clear().await.unwrap();

    peri_time::sleep(Duration::from_millis(100)).await;
    assert!(queue.is_empty(), "clear 后不得继续恢复");
    assert!(state.snapshot().objective.is_none());
}

#[tokio::test]
async fn test_complete_after_recovery_stops_schedule() {
    let (state, queue) = make_recovery_state(Duration::from_millis(20));
    state.create_goal("完成任务".to_string()).await.unwrap();
    state.block_goal("网络中断".to_string()).await.unwrap();
    next_recovery(&queue).await;
    state.complete_goal().await.unwrap();

    peri_time::sleep(Duration::from_millis(100)).await;
    assert!(queue.is_empty(), "complete 后不得继续恢复");
    assert_eq!(state.snapshot().status, Some(GoalStatus::Complete));
}

#[tokio::test]
async fn test_blocked_goal_still_rejects_public_transition_to_active() {
    let state = make_state();
    state.create_goal("终态不变".to_string()).await.unwrap();
    state.block_goal("阻塞".to_string()).await.unwrap();

    // 公开状态机仍把 Blocked 视为终态；只有恢复调度器可重开。
    assert!(state.set_status(GoalStatus::Active).await.is_err());
    assert!(!state.snapshot().has_active_goal());
}

#[tokio::test]
async fn test_active_goal_error_schedules_recovery_after_interval() {
    let (state, queue) = make_recovery_state(Duration::from_millis(30));
    state
        .create_goal("从模型错误恢复".to_string())
        .await
        .unwrap();

    GoalController::recover_after_error(&state, "model_error").await;
    let message = next_recovery(&queue).await;

    assert_eq!(state.snapshot().status, Some(GoalStatus::Active));
    let QueuedPayload::SystemReminder(reminder) = &message.payload else {
        panic!("恢复提醒必须是 canonical system reminder");
    };
    let reminder = reminder.as_reminder();
    assert_eq!(reminder.metadata["trigger"], "turn_error");
    assert_eq!(reminder.metadata["trigger_reason"], "model_error");
    assert!(reminder.body.contains("previous turn ended with an error"));
    assert!(reminder.body.contains("从模型错误恢复"));
}

#[tokio::test]
async fn test_error_recovery_is_idempotent_within_one_episode() {
    let (state, queue) = make_recovery_state(Duration::from_millis(30));
    state.create_goal("幂等恢复".to_string()).await.unwrap();

    // run_on_error 与终态分类可能对同一失败各调用一次；不得重置周期。
    GoalController::recover_after_error(&state, "model_error").await;
    GoalController::recover_after_error(&state, "model_error").await;

    let message = next_recovery(&queue).await;
    let QueuedPayload::SystemReminder(reminder) = &message.payload else {
        panic!("expected reminder");
    };
    assert_eq!(reminder.as_reminder().metadata["recovery_attempt"], 1);
}

#[tokio::test]
async fn test_error_recovery_not_armed_for_terminal_goal() {
    let (state, queue) = make_recovery_state(Duration::from_millis(20));
    state.create_goal("已完成".to_string()).await.unwrap();
    state.complete_goal().await.unwrap();

    GoalController::recover_after_error(&state, "model_error").await;
    peri_time::sleep(Duration::from_millis(80)).await;
    assert!(queue.is_empty(), "终态 goal 不应安排恢复");
}

#[tokio::test]
async fn test_acknowledge_stops_error_recovery() {
    let (state, queue) = make_recovery_state(Duration::from_millis(20));
    state.create_goal("错误后恢复".to_string()).await.unwrap();
    GoalController::recover_after_error(&state, "model_error").await;
    next_recovery(&queue).await;

    GoalController::acknowledge_recovery(&state).await;
    peri_time::sleep(Duration::from_millis(100)).await;
    assert!(queue.is_empty(), "成功 turn 确认后不得继续错误重试");
}

#[tokio::test]
async fn test_long_block_reason_is_bounded_in_recovery_reminder() {
    let (state, queue) = make_recovery_state(Duration::from_millis(20));
    state.create_goal("长原因目标".to_string()).await.unwrap();
    state.block_goal("x".repeat(20_000)).await.unwrap();

    let message = next_recovery(&queue).await;
    let QueuedPayload::SystemReminder(reminder) = &message.payload else {
        panic!("expected reminder");
    };
    let body = &reminder.as_reminder().body;
    assert!(body.chars().count() < 6_000, "恢复提醒必须限制原因长度");
    assert!(body.contains('…'), "截断必须显式标记");
}

// ---- complete_goal_if_current：身份 CAS ----

/// clear + create B 后，A 的 identity CAS 必须失败且 B 保持 Active。
#[tokio::test]
async fn test_complete_goal_if_current_rejects_stale_id_after_replace() {
    let state = make_state();
    state.create_goal("目标 A".to_string()).await.unwrap();
    let id_a = state.snapshot().goal_id.clone().unwrap();

    state.clear().await.unwrap();
    state.create_goal("目标 B".to_string()).await.unwrap();
    let id_b = state.snapshot().goal_id.clone().unwrap();
    assert_ne!(id_a, id_b, "create 必须生成新身份");

    let err = state
        .complete_goal_if_current(&id_a)
        .await
        .expect_err("stale id 必须被拒绝");
    assert!(
        err.contains("已被替换或清除"),
        "错误须说明身份已变化，实际：{err}"
    );

    // 契约视图必须暴露身份（工具层据此做 CAS）。
    let view = GoalStateView::snapshot(&state);
    assert_eq!(view.goal_id.as_deref(), Some(id_b.as_str()));
    assert_eq!(view.objective.as_deref(), Some("目标 B"));
    assert_eq!(view.status, Some(GoalStatus::Active));

    // 当前身份仍可正常完成。
    state.complete_goal_if_current(&id_b).await.unwrap();
    assert_eq!(state.snapshot().status, Some(GoalStatus::Complete));
}

/// 无活跃 goal 时 identity CAS 返回 Err，不 panic。
#[tokio::test]
async fn test_complete_goal_if_current_without_goal_errors() {
    let state = make_state();
    let err = state
        .complete_goal_if_current("goal-x")
        .await
        .expect_err("无 goal 时必须失败");
    assert!(err.contains("无活跃 goal"), "实际：{err}");
}

/// 同一身份已是终态时再次完成返回 Err，状态保持 Complete。
#[tokio::test]
async fn test_complete_goal_if_current_terminal_errors() {
    let state = make_state();
    state.create_goal("已完成目标".to_string()).await.unwrap();
    let goal_id = state.snapshot().goal_id.clone().unwrap();
    state.complete_goal().await.unwrap();

    let err = state
        .complete_goal_if_current(&goal_id)
        .await
        .expect_err("终态不可再次转换");
    assert!(err.contains("非法状态转换"), "实际：{err}");
    assert_eq!(state.snapshot().status, Some(GoalStatus::Complete));
}
