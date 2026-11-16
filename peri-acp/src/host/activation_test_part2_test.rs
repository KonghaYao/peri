/// dispatch 二次资格检查遇到瞬时查询错误：预算内重查恢复后同一轮处理提醒，
/// 不把已消费的唤醒当作「无工作」。
#[tokio::test]
#[serial]
async fn dispatch_query_error_is_retried_within_budget_and_reminder_is_processed() {
    let harness = ActivationHarness::new().await;
    let queue = harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap();
    harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .allow();
    queue
        .push_async(QueuedMessage::defer(
            MessageSource::WorkflowComplete,
            BaseMessage::human("workflow finished"),
        ))
        .await;
    let epoch = harness
        .sessions
        .lock()
        .await
        .get(&harness.session_id)
        .unwrap()
        .continuation_epoch;

    // 停在准入闸门：此前的调度查询已经成功，故障只落在 dispatch 的二次检查上。
    let gate = crate::host::prompt_dispatch::admission_gate::install(&format!(
        "{}:admitted",
        harness.session_id
    ));
    let logs = LogBuffer::default();
    let _subscriber = tracing::subscriber::set_default(logs.warn_subscriber());
    let params = json!({
        "sessionId": harness.session_id,
        "message": {"role": "user", "content": []},
    });
    let dispatch = crate::host::dispatch_prompt_turn(
        params,
        crate::host::PromptOrigin::Continuation {
            mq_steering: true,
            pending_result: false,
            recovery: false,
        },
        Some(epoch),
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    );
    let db_path = harness.db_path();
    let logs_for_fault = logs.clone();
    let fault = async {
        gate.entered.notified().await;
        rename_mq_table(&db_path, "thread_mq", "thread_mq_offline").await;
        gate.release.notify_one();
        // 等 dispatch 至少失败一次并进入退避后再恢复存储，不依赖固定睡眠竞态。
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if logs_for_fault
                    .text()
                    .contains("continuation dispatch query failed")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("dispatch 必须进入查询重查");
        rename_mq_table(&db_path, "thread_mq_offline", "thread_mq").await;
    };
    let (response, ()) = tokio::join!(dispatch, fault);
    let response = response.unwrap();
    assert_eq!(response["stopReason"], "end_turn");
    assert!(queue.is_empty().await.expect("test MQ len"));
    let logs = logs.text();
    assert!(
        logs.contains("retry_after_ms"),
        "缺少重查退避的可观察 warn：{logs}"
    );
    assert!(
        logs.contains("store is unavailable"),
        "缺少失败原因：{logs}"
    );
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

/// 重查预算耗尽：本轮不得静默当作「无工作」完成，必须唤醒 listener 重查，
/// 且提醒仍留在 MQ 等待处理。
#[tokio::test]
#[serial]
async fn dispatch_query_error_after_budget_requests_listener_recheck() {
    let harness = ActivationHarness::new().await;
    let queue = harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap();
    harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .allow();
    queue
        .push_async(QueuedMessage::defer(
            MessageSource::WorkflowComplete,
            BaseMessage::human("workflow finished"),
        ))
        .await;
    let epoch = harness
        .sessions
        .lock()
        .await
        .get(&harness.session_id)
        .unwrap()
        .continuation_epoch;
    let mut wake = queue.subscribe_wake();
    wake.borrow_and_update();

    let gate = crate::host::prompt_dispatch::admission_gate::install(&format!(
        "{}:admitted",
        harness.session_id
    ));
    let logs = LogBuffer::default();
    let _subscriber = tracing::subscriber::set_default(logs.warn_subscriber());
    let params = json!({
        "sessionId": harness.session_id,
        "message": {"role": "user", "content": []},
    });
    let dispatch = crate::host::dispatch_prompt_turn(
        params,
        crate::host::PromptOrigin::Continuation {
            mq_steering: true,
            pending_result: false,
            recovery: false,
        },
        Some(epoch),
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    );
    let db_path = harness.db_path();
    let logs_for_fault = logs.clone();
    let fault = async {
        gate.entered.notified().await;
        rename_mq_table(&db_path, "thread_mq", "thread_mq_offline").await;
        gate.release.notify_one();
        // 故障保持到 dispatch 用完重查预算并交还 listener，避免与恢复竞态。
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if logs_for_fault
                    .text()
                    .contains("requesting listener recheck")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("dispatch 必须耗尽预算并请求重查");
        rename_mq_table(&db_path, "thread_mq_offline", "thread_mq").await;
    };
    let (response, ()) = tokio::join!(dispatch, fault);
    assert!(response.unwrap().is_null(), "查询失败不得当作空工作完成");
    let logs = logs.text();
    assert!(
        logs.contains("retry_after_ms") && logs.contains("requesting listener recheck"),
        "预算耗尽必须留下可观察 warn：{logs}"
    );
    assert!(
        wake.has_changed().expect("test wake receiver"),
        "必须请求 listener 重查"
    );
    assert_eq!(
        queue
            .query(peri_acp_types::session::MqFilter::default())
            .await
            .unwrap()
            .len(),
        1,
        "提醒必须留在 MQ 等待重查"
    );
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

/// P1 回归：取消后的唯一一次 armed 续跑，若 dispatch 二次查询耗尽预算，
/// 必须把资格还给 listener；不能等用户下一次输入才消费滞留的 Defer。
#[tokio::test]
#[serial]
async fn armed_continuation_query_failure_restores_eligibility_for_listener() {
    let mut harness = ActivationHarness::new().await;
    let queue = harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap();
    harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .allow();
    // 用户取消前台 prompt：epoch++、suppress、arm（与 Stop 同路径）。
    {
        let mut sessions = harness.sessions.lock().await;
        sessions.get_mut(&harness.session_id).unwrap().cancel_token =
            Some(CancellationToken::new());
        let _ = crate::host::notify::handle_notification(
            "session/cancel",
            &json!({"sessionId": harness.session_id}),
            &mut sessions,
            &harness.cfg,
        )
        .await;
        sessions.get_mut(&harness.session_id).unwrap().cancel_token = None;
    }
    harness.complete_child("armed-child");
    harness.listen();
    let first = tokio::time::timeout(
        Duration::from_secs(2),
        harness.receiver.as_mut().unwrap().recv(),
    )
    .await
    .expect("listener 必须发出 armed 续跑请求")
    .expect("continuation channel");
    assert!(!first.mq_steering, "armed 路径不得走 mq steering");
    assert!(!first.pending_result);
    let epoch = {
        let mut sessions = harness.sessions.lock().await;
        crate::host::continuation::take_continuation_for_request(
            sessions.get_mut(&harness.session_id).unwrap(),
            &first,
        )
        .expect("armed 请求必须消费一次性资格")
    };

    let gate = crate::host::prompt_dispatch::admission_gate::install(&format!(
        "{}:admitted",
        harness.session_id
    ));
    let logs = LogBuffer::default();
    let _subscriber = tracing::subscriber::set_default(logs.warn_subscriber());
    let params = json!({
        "sessionId": harness.session_id,
        "message": {"role": "user", "content": []},
    });
    let origin = crate::host::PromptOrigin::Continuation {
        mq_steering: false,
        pending_result: false,
        recovery: false,
    };
    let dispatch = crate::host::dispatch_prompt_turn(
        params.clone(),
        origin,
        Some(epoch),
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    );
    let db_path = harness.db_path();
    let logs_for_fault = logs.clone();
    let fault = async {
        gate.entered.notified().await;
        rename_mq_table(&db_path, "thread_mq", "thread_mq_offline").await;
        gate.release.notify_one();
        // 恢复 armed 的 warn 发生在 request_recheck 之前：见到它即可恢复存储。
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if logs_for_fault.text().contains("restored=true") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("查询失败必须恢复 armed 资格");
        rename_mq_table(&db_path, "thread_mq_offline", "thread_mq").await;
    };
    let (response, ()) = tokio::join!(dispatch, fault);
    assert!(response.unwrap().is_null(), "查询失败不得当作空工作完成");
    let logs = logs.text();
    assert!(
        logs.contains("restored=true") && logs.contains("requesting listener recheck"),
        "预算耗尽必须恢复 armed 并请求 listener 重查：{logs}"
    );

    // 无需用户输入：listener 重查同一 Defer，必须再次发出 armed 请求。
    let second = tokio::time::timeout(
        Duration::from_secs(2),
        harness.receiver.as_mut().unwrap().recv(),
    )
    .await
    .expect("恢复资格后 listener 必须自动重查")
    .expect("continuation channel");
    assert!(!second.mq_steering);
    assert!(!second.pending_result);
    let retaken = {
        let mut sessions = harness.sessions.lock().await;
        crate::host::continuation::take_continuation_for_request(
            sessions.get_mut(&harness.session_id).unwrap(),
            &second,
        )
    };
    assert_eq!(retaken, Some(epoch), "重查必须拿回同一代际的资格");

    let response = crate::host::dispatch_prompt_turn(
        params,
        origin,
        retaken,
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    )
    .await
    .unwrap();
    assert_eq!(response["stopReason"], "end_turn");
    assert!(
        queue.is_empty().await.expect("test MQ len"),
        "续跑必须消费滞留的子任务结果"
    );
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

/// 恢复的 armed 资格属于当前代际：用户新 prompt 后 listener 不得再发旧代际请求。
#[tokio::test]
#[serial]
async fn restored_armed_is_not_reactivated_after_new_prompt() {
    let mut harness = ActivationHarness::new().await;
    let queue = harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap();
    harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .suppress();
    {
        let mut sessions = harness.sessions.lock().await;
        let state = sessions.get_mut(&harness.session_id).unwrap();
        state.continuation_epoch = 3;
        assert!(crate::host::continuation::restore_continuation_armed(
            state, 3
        ));
        // 用户新 prompt：清除 armed 并递增代际。
        state.continuation_armed = false;
        state.continuation_epoch += 1;
    }
    harness.complete_child("stale-generation-child");
    harness.listen();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        harness.receiver.as_mut().unwrap().try_recv().is_err(),
        "新 prompt 后不得复活旧代际的 armed 请求"
    );
    assert_eq!(
        queue
            .query(peri_acp_types::session::MqFilter::default())
            .await
            .unwrap()
            .len(),
        1,
        "Defer 留在 MQ 等新 prompt 消费"
    );
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

#[tokio::test]
#[serial]
async fn closing_session_does_not_activate_late_work() {
    let mut harness = ActivationHarness::new().await;
    harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .allow();
    let sessions = harness.sessions.clone();
    let _held_session_lock = sessions.lock().await;
    harness.listen();
    tokio::task::yield_now().await;
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
    let queue = harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap();
    queue
        .push_async(QueuedMessage::defer(
            MessageSource::SubAgentComplete,
            BaseMessage::human("late"),
        ))
        .await;
    tokio::task::yield_now().await;
    assert!(harness.receiver.as_mut().unwrap().try_recv().is_err());
    let owner = Arc::get_mut(&mut harness.cfg)
        .unwrap()
        .host_task_owner
        .as_mut()
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), owner.shutdown())
            .await
            .unwrap(),
        crate::host::task_scope::HostShutdownReport::Complete,
    );
}

#[tokio::test]
#[serial]
async fn approved_schedule_runs_without_reenabling_suppressed_passive_results() {
    let harness = ActivationHarness::new().await;
    let queue = harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap();
    queue
        .push_async(QueuedMessage::defer(
            MessageSource::CronTrigger,
            BaseMessage::human("approved schedule"),
        ))
        .await;
    harness
        .sessions
        .lock()
        .await
        .get_mut(&harness.session_id)
        .unwrap()
        .continuation_mq_steering_pending = true;
    let params =
        json!({"sessionId": harness.session_id, "message": {"role": "user", "content": []}});
    let suppressed = crate::host::dispatch_prompt_turn(
        params.clone(),
        crate::host::PromptOrigin::Continuation {
            mq_steering: true,
            pending_result: false,
            recovery: false,
        },
        Some(0),
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    )
    .await
    .unwrap();
    assert!(suppressed.is_null());
    assert!(queue.has_ensure_processing().await.expect("test MQ policy"));
    let response = crate::host::dispatch_prompt_turn(
        params,
        crate::host::PromptOrigin::Scheduled,
        Some(0),
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    )
    .await
    .unwrap();
    assert_eq!(response["stopReason"], "end_turn");
    assert!(queue.is_empty().await.expect("test MQ len"));
    assert!(!harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .is_allowed());
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

/// 模型配置变化会让旧 token 快照失效（设计 §2.2）。
#[tokio::test]
#[serial]
async fn model_config_update_invalidates_saved_context_usage() {
    let harness = ActivationHarness::new().await;
    let alias = harness.cfg.peri_config.read().config.active_alias.clone();
    let mut sessions = harness.sessions.lock().await;
    let outcome = crate::host::notify::handle_notification(
        "session/config_update",
        &json!({
            "sessionId": harness.session_id,
            "configId": "model",
            "value": alias,
        }),
        &mut sessions,
        &harness.cfg,
    )
    .await;
    assert_eq!(
        outcome.context_usage_invalidations,
        vec![ThreadId::from(harness.session_id.clone())]
    );
    drop(sessions);
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}

/// 定时调度是已批准的调度，但被动回调不能解除用户显式 Stop（设计 §2.1/§5.2）。
#[tokio::test]
#[serial]
async fn scheduled_continuation_does_not_override_user_stop() {
    let harness = ActivationHarness::new().await;
    let queue = harness
        .cfg
        .session_manager
        .v2_queue_for(&harness.session_id)
        .unwrap();
    queue
        .push_async(QueuedMessage::defer(
            MessageSource::CronTrigger,
            BaseMessage::human("approved schedule"),
        ))
        .await;
    harness
        .cfg
        .session_manager
        .get_session(&harness.session_id)
        .unwrap()
        .activation
        .suppress();
    let thread = ThreadId::from(harness.session_id.clone());
    harness
        .cfg
        .session_manager
        .session_resources()
        .set_work_state(&thread, WorkState::Stopped)
        .await
        .unwrap();
    harness
        .sessions
        .lock()
        .await
        .get_mut(&harness.session_id)
        .unwrap()
        .continuation_mq_steering_pending = true;
    let params =
        json!({"sessionId": harness.session_id, "message": {"role": "user", "content": []}});
    let response = crate::host::dispatch_prompt_turn(
        params,
        crate::host::PromptOrigin::Scheduled,
        Some(0),
        &harness.sessions,
        &harness.locks,
        &harness.transport,
        &harness.cfg,
        &harness.sender,
    )
    .await
    .unwrap();
    assert!(response.is_null(), "用户 Stop 后不得自动运行定时调度");
    assert!(queue.has_ensure_processing().await.expect("test MQ policy"));
    assert_eq!(
        harness
            .cfg
            .session_manager
            .session_resources()
            .load_session_meta(&thread)
            .await
            .unwrap()
            .work_state,
        WorkState::Stopped,
        "定时调度不得把 stopped 改回 working"
    );
    harness
        .cfg
        .session_manager
        .pre_close_session(&harness.session_id);
}
