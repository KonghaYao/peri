//! 工作意图准入：写入失败必须在模型调用之前拒绝本轮。
//!
//! 用 SQLite trigger 让 `work_state` 更新失败，验证 fail-closed 与内存回滚。

use super::*;
use crate::host::{dispatch_prompt_turn, PromptOrigin};

/// 读取 durable 工作意图（不依赖 Host 内存态）。
async fn durable_work_state(harness: &RecoveryHarness, thread_id: &str) -> WorkState {
    harness
        .cfg
        .session_resources
        .load_session_meta(&thread_id.to_owned())
        .await
        .unwrap()
        .work_state
}

/// 以 `ServerLoop::dispatch_notification` 同序处理 Stop：先锁内捕获目标，再锁外
/// 持久化。返回捕获的目标与持久化任务，测试负责在需要的屏障处等待/放行。
async fn capture_and_spawn_stop(
    harness: &RecoveryHarness,
    thread_id: &str,
) -> (
    Vec<crate::host::notify::IntentTarget>,
    tokio::task::JoinHandle<()>,
) {
    let targets = {
        let mut states = harness.sessions.lock().await;
        crate::host::notify::handle_notification(
            "session/cancel",
            &json!({"sessionId": thread_id}),
            &mut states,
            &harness.cfg,
        )
        .await
        .stop_targets
    };
    let cfg = Arc::clone(&harness.cfg);
    let sessions = Arc::clone(&harness.sessions);
    let locks = Arc::clone(&harness.locks);
    let persist_targets = targets.clone();
    let persist = tokio::spawn(async move {
        crate::host::notify::persist_notification_effects(
            "session/cancel",
            &cfg,
            &sessions,
            &locks,
            persist_targets,
            Vec::new(),
        )
        .await;
    });
    (targets, persist)
}

#[tokio::test]
async fn work_state_commit_failure_rejects_prompt_before_model_call() {
    let harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    use sqlx::Connection as _;
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(&harness.db_path),
    )
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER fail_work_state BEFORE UPDATE OF work_state ON threads
         BEGIN SELECT RAISE(ABORT, 'work_state blocked'); END;",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    drop(connection);

    let error = dispatch_prompt_turn(
        json!({
            "sessionId": thread_id,
            "prompt": [{"type": "text", "text": "hello"}],
        }),
        PromptOrigin::User,
        None,
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    )
    .await
    .expect_err("工作意图写入失败必须拒绝 prompt");

    assert_eq!(error.code, -32603, "{error:?}");
    assert_eq!(
        error.data.as_ref().unwrap()["reason"],
        "work_intent_commit_failed"
    );
    assert!(!harness._mock.matched(), "拒绝必须发生在模型调用之前");
    assert_eq!(
        harness
            .cfg
            .session_resources
            .load_session_meta(&thread_id)
            .await
            .unwrap()
            .work_state,
        WorkState::Idle,
        "写入失败不得留下 working"
    );
    assert!(
        harness
            .sessions
            .lock()
            .await
            .get(&thread_id)
            .is_some_and(|state| state.cancel_token.is_none()),
        "准入失败必须清除本次发布的令牌"
    );
}

/// 准入回滚只撤销本次准入发布的令牌：后续轮次已替换的令牌不得被误清（#196 P2 复审）。
///
/// 在准入闸门处人为固定「后续轮次已替换令牌」的时序，再让工作意图写入失败，
/// 验证错误提前返回的清理按令牌身份判断。
#[tokio::test]
async fn admission_rollback_only_clears_its_own_token() {
    let harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    use sqlx::Connection as _;
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(&harness.db_path),
    )
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER fail_work_state BEFORE UPDATE OF work_state ON threads
         BEGIN SELECT RAISE(ABORT, 'work_state blocked'); END;",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    drop(connection);

    let gate =
        crate::host::prompt_dispatch::admission_gate::install(&format!("{thread_id}:admitted"));
    let prompt = {
        let sessions = Arc::clone(&harness.sessions);
        let locks = Arc::clone(&harness.locks);
        let transport = Arc::clone(&harness.transport);
        let cfg = Arc::clone(&harness.cfg);
        let sender = Arc::clone(&harness.sender);
        let thread_id = thread_id.clone();
        tokio::spawn(async move {
            dispatch_prompt_turn(
                json!({
                    "sessionId": thread_id,
                    "prompt": [{"type": "text", "text": "hello"}],
                }),
                PromptOrigin::User,
                None,
                &sessions,
                &locks,
                &transport,
                &cfg,
                &sender,
            )
            .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), gate.entered.notified())
        .await
        .expect("prompt 必须到达准入后的暂停点");

    // 模拟后续轮次已经发布自己的令牌：本次准入只能撤销自己持有的那一个。
    let later = tokio_util::sync::CancellationToken::new();
    harness
        .sessions
        .lock()
        .await
        .get_mut(&thread_id)
        .expect("会话存在")
        .cancel_token = Some(later.clone());
    gate.release.notify_one();

    let error = prompt
        .await
        .expect("prompt 任务不得 panic")
        .expect_err("工作意图写入失败必须拒绝 prompt");
    assert_eq!(error.code, -32603, "{error:?}");
    let states = harness.sessions.lock().await;
    assert_eq!(
        states[&thread_id].cancel_token.as_ref(),
        Some(&later),
        "回滚不得清除后续轮次发布的令牌"
    );
}

/// P2 复审：Stop 落在 `working` 已提交、代次尚未推进的窗口时，通知捕获旧代次，
/// 锁外持久化会被代次守卫跳过；取消的队列轮次必须由 dispatch 自己落 `stopped`。
///
/// 真实 `session/cancel` 处理 + 锁外持久化任务 + 队列输入准入；模型端点只允许
/// 零次调用（mock 的 `matched()` 必须为 false）。
#[tokio::test]
async fn stop_between_working_commit_and_epoch_advance_settles_stopped() {
    let harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    let mailbox =
        crate::host::user_input::ensure_mailbox(&thread_id, &harness.cfg, &harness.transport)
            .await
            .expect("会话收件箱必须可建立");
    mailbox
        .enqueue(&peri_acp_types::session::EnqueueUserInputRequest {
            session_id: thread_id.clone(),
            generation: mailbox.generation().to_owned(),
            command_id: "enqueue-stop-race".to_owned(),
            input_id: "00000000-0000-0000-0000-000000000001".to_owned(),
            content: peri_acp_types::messages::MessageContent::Text("queued".to_owned()),
            original_draft: "queued".to_owned(),
        })
        .await
        .expect("队列输入必须入队");
    let ticket = mailbox.reserve_run().expect("队列输入必须能预留一次执行");
    let run = crate::host::user_input::UserInputRun::new(ticket);

    let gate = crate::host::prompt_dispatch::admission_gate::install(&format!(
        "{thread_id}:working-committed"
    ));
    let prompt = {
        let sessions = Arc::clone(&harness.sessions);
        let locks = Arc::clone(&harness.locks);
        let transport = Arc::clone(&harness.transport);
        let cfg = Arc::clone(&harness.cfg);
        let sender = Arc::clone(&harness.sender);
        let thread_id = thread_id.clone();
        tokio::spawn(async move {
            crate::host::prompt_dispatch::dispatch_prompt_turn_with_input(
                json!({
                    "sessionId": thread_id,
                    "requestId": "queued-stop-race",
                    "message": {"role": "user", "content": []},
                }),
                PromptOrigin::QueuedUser,
                None,
                &sessions,
                &locks,
                &transport,
                &cfg,
                &sender,
                Some(run),
            )
            .await
        })
    };
    tokio::time::timeout(WAIT_TIMEOUT, gate.entered.notified())
        .await
        .expect("prompt 必须停在 working 提交之后、代次推进之前");

    let (targets, persist) = capture_and_spawn_stop(&harness, &thread_id).await;
    assert_eq!(targets.len(), 1);
    assert_eq!(
        targets[0].epoch, 0,
        "Stop 必须读到代次推进前的值（复现迟到持久化的条件）"
    );

    gate.release.notify_one();
    let result = tokio::time::timeout(WAIT_TIMEOUT, prompt)
        .await
        .expect("prompt 不得挂起")
        .expect("prompt 任务不得 panic");
    assert!(
        matches!(result, Ok(Value::Null)),
        "attach 被 Stop 拒绝后应返回空操作: {result:?}"
    );
    tokio::time::timeout(WAIT_TIMEOUT, persist)
        .await
        .expect("Stop 持久化不得挂起")
        .expect("Stop 持久化任务不得 panic");

    assert!(!harness._mock.matched(), "被 Stop 的轮次不得进入模型调用");
    assert!(
        harness
            .sessions
            .lock()
            .await
            .get(&thread_id)
            .is_some_and(|state| state.cancel_token.is_none()),
        "本轮准入令牌必须被清除"
    );
    assert_eq!(
        durable_work_state(&harness, &thread_id).await,
        WorkState::Stopped,
        "取消的准入必须 durable 落 stopped，不能残留 working"
    );
}

/// P2 复审：close → load 发布新 `SessionState` 并把 `work_intent_epoch` 归零。
/// 旧生命周期捕获的 Stop 在新一轮写到相同 epoch 时不得覆盖新轮次的 `working`。
///
/// 旧 Stop worker 用测试屏障固定在 `gate_for` 之后、写锁之前；新 prompt 用真实
/// 准入停在 `working` 提交后，确保比较发生在同一 epoch 数值上。
#[tokio::test]
async fn stale_stop_across_close_load_cannot_override_new_working() {
    let mut harness = RecoveryHarness::new().await;
    let thread_id = harness.thread_id.clone();
    harness.set_working(&thread_id).await;
    let old_generation = {
        let mut states = harness.sessions.lock().await;
        let state = states.get_mut(&thread_id).expect("会话存在");
        state.work_intent_epoch = 1;
        state.session_generation
    };

    // 旧生命周期的 Stop：捕获代次 1，worker 暂停在 gate_for 之后。
    let stop_gate =
        crate::host::prompt_dispatch::admission_gate::install(&format!("{thread_id}:stop-persist"));
    let (targets, persist) = capture_and_spawn_stop(&harness, &thread_id).await;
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].epoch, 1);
    assert_eq!(targets[0].session_generation, Some(old_generation));
    tokio::time::timeout(WAIT_TIMEOUT, stop_gate.entered.notified())
        .await
        .expect("旧 Stop worker 必须停在 gate_for 之后");

    // 真实 close → load：新实例代次 + 代次归零。
    {
        let mut states = harness.sessions.lock().await;
        crate::host::requests::session_lifecycle::handle_close(
            &json!({"sessionId": thread_id}),
            &harness.cfg,
            &mut states,
        )
        .await
        .expect("session/close 必须成功");
        assert!(!states.contains_key(&thread_id), "close 必须移除 live 实例");
    }
    harness.load_session(&thread_id).await;
    let new_generation = {
        let states = harness.sessions.lock().await;
        assert_eq!(states[&thread_id].work_intent_epoch, 0);
        states[&thread_id].session_generation
    };
    assert_ne!(
        old_generation, new_generation,
        "load 必须发布新的生命周期代次"
    );

    // 新 prompt：真实准入把 working 写回并把 epoch 推进到与旧目标相同的 1。
    let prompt_gate = crate::host::prompt_dispatch::admission_gate::install(&format!(
        "{thread_id}:epoch-advanced"
    ));
    let prompt = {
        let sessions = Arc::clone(&harness.sessions);
        let locks = Arc::clone(&harness.locks);
        let transport = Arc::clone(&harness.transport);
        let cfg = Arc::clone(&harness.cfg);
        let sender = Arc::clone(&harness.sender);
        let thread_id = thread_id.clone();
        tokio::spawn(async move {
            dispatch_prompt_turn(
                json!({
                    "sessionId": thread_id,
                    "prompt": [{"type": "text", "text": "after reload"}],
                }),
                PromptOrigin::User,
                None,
                &sessions,
                &locks,
                &transport,
                &cfg,
                &sender,
            )
            .await
        })
    };
    tokio::time::timeout(WAIT_TIMEOUT, prompt_gate.entered.notified())
        .await
        .expect("新 prompt 必须停在 working 提交之后");
    assert_eq!(
        harness.sessions.lock().await[&thread_id].work_intent_epoch,
        1,
        "新轮次代次与旧 Stop 目标数值相同"
    );

    // 放行旧生命周期 worker：必须按实例代次跳过。
    stop_gate.release.notify_one();
    tokio::time::timeout(WAIT_TIMEOUT, persist)
        .await
        .expect("旧 Stop 持久化不得挂起")
        .expect("旧 Stop 持久化任务不得 panic");
    assert_eq!(
        durable_work_state(&harness, &thread_id).await,
        WorkState::Working,
        "旧生命周期的 Stop 不得覆盖新轮次的 working"
    );

    prompt_gate.release.notify_one();
    let result = tokio::time::timeout(WAIT_TIMEOUT, prompt)
        .await
        .expect("新 prompt 不得挂起")
        .expect("新 prompt 任务不得 panic");
    assert!(result.is_ok(), "新轮次应正常完成: {result:?}");
}
