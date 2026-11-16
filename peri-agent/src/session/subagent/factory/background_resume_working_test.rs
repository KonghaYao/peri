// Responsibility-specific cases share the original test namespace and fixtures.

/// 显式 resume 前的真实持久状态：`Done + Stopped`，与旧 bug 留下的
/// `Active + Stopped` 可区分。
async fn stopped_fixture(
    fail_work_state: bool,
) -> (
    Arc<ClaimResources>,
    Arc<WorkStateGate>,
    Arc<crate::agent::async_tasks::TaskManager>,
) {
    let work_gate = Arc::new(WorkStateGate {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Semaphore::new(0),
        fail: fail_work_state,
        writes: AtomicUsize::new(0),
        pause_after_commit: std::sync::atomic::AtomicBool::new(false),
        committed: tokio::sync::Notify::new(),
        post_release: tokio::sync::Semaphore::new(0),
    });
    let mut store = ClaimResources::new();
    Arc::get_mut(&mut store)
        .expect("Arc get mut fixture must contain the requested entry")
        .work_state_gate = Some(work_gate.clone());
    let mut meta = ThreadMeta::new_at("/tmp/work", peri_time::now_wall());
    meta.id = CHILD.into();
    meta.agent_status = AgentStatus::Done;
    meta.work_state = peri_acp_types::thread::WorkState::Stopped;
    store
        .inner
        .create_resumable_thread(meta)
        .await
        .expect("the create resumable thread fixture result is required for this assertion");
    (
        store,
        work_gate,
        Arc::new(crate::agent::async_tasks::TaskManager::new()),
    )
}

/// Working 写未完成前不得报告后台启动成功；成功返回时 Working 已落库。
#[tokio::test]
async fn resume_persists_working_before_reporting_background_success() {
    let (store, work_gate, manager) = stopped_fixture(false).await;
    let mut config = config(store.clone(), manager.clone());
    // 模型挂起，避免后台 turn 在断言前收尾把 Working 改回终态。
    config.llm = crate::session::test_resources::mock::model::fixture_source(
        Arc::new(HangingModel),
        "fixture",
    );
    let resume = tokio::spawn(async move { SessionFactory::resume_subagent(None, config).await });
    work_gate.entered.notified().await;
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    assert!(!resume.is_finished(), "后台启动成功必须等待 Working 写完成");
    assert_eq!(
        store
            .inner
            .load_session_meta(&CHILD.into())
            .await
            .expect("the into fixture result is required for this assertion")
            .work_state,
        peri_acp_types::thread::WorkState::Stopped,
        "写未放行前磁盘仍是认领前的 Stopped"
    );
    work_gate.release.add_permits(1);
    resume.await.expect("the resume fixture result is required for this assertion").expect("resume persists working before reporting background success: the completed fixture operation must provide its result payload");
    assert_eq!(
        store
            .inner
            .load_session_meta(&CHILD.into())
            .await
            .expect("the into fixture result is required for this assertion")
            .work_state,
        peri_acp_types::thread::WorkState::Working,
        "成功返回时 Working 已提交"
    );
    assert_eq!(
        work_gate.writes.load(Ordering::SeqCst),
        1,
        "resume 只写一次 Working，不再依赖 worker 补写"
    );
    assert_eq!(manager.active_count(), 1);
}

/// Working 写失败必须拒绝启动：返回错误、认领状态回滚、不留下可恢复的 Working。
#[tokio::test]
async fn resume_working_write_failure_rejects_background_start() {
    let (store, work_gate, manager) = stopped_fixture(true).await;
    let starts = Arc::new(AtomicUsize::new(0));
    let mut config = config(store.clone(), manager.clone());
    let observed = starts.clone();
    config.on_subagent_start = Some(Arc::new(move |_, _, _| {
        observed.fetch_add(1, Ordering::SeqCst);
    }));
    let resume = tokio::spawn(async move { SessionFactory::resume_subagent(None, config).await });
    work_gate.entered.notified().await;
    work_gate.release.add_permits(1);
    let error = resume
        .await
        .expect("the resume fixture result is required for this assertion")
        .expect_err("Working 写失败不得报告后台启动成功");
    assert!(
        error.to_string().contains("resume work state write failed"),
        "{error}"
    );
    let meta = store
        .inner
        .load_session_meta(&CHILD.into())
        .await
        .expect("the load session meta fixture result is required for this assertion");
    assert_eq!(
        meta.agent_status,
        AgentStatus::Done,
        "认领补偿必须恢复到认领前的状态"
    );
    assert_eq!(
        meta.work_state,
        peri_acp_types::thread::WorkState::Stopped,
        "失败不得留下 Working"
    );
    idle(&manager).await;
    assert_eq!(manager.active_count(), 0);
    assert_eq!(starts.load(Ordering::SeqCst), 0, "不得启动执行");
}

/// [回归] Working 已提交、调用在返回前被取消：认领 worker 必须同时恢复工作意图，
/// 不能留下 `Done + Working` 让下一次进入把已停止的 child 当成待继续。
#[tokio::test]
async fn cancel_after_working_commit_restores_work_state() {
    let (store, work_gate, manager) = stopped_fixture(false).await;
    work_gate
        .pause_after_commit
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let starts = Arc::new(AtomicUsize::new(0));
    let mut config = config(store.clone(), manager.clone());
    let observed = starts.clone();
    config.on_subagent_start = Some(Arc::new(move |_, _, _| {
        observed.fetch_add(1, Ordering::SeqCst);
    }));
    // 后台 turn 若被错误放行会挂起，不自行收尾掩盖残留。
    config.llm = crate::session::test_resources::mock::model::fixture_source(
        Arc::new(HangingModel),
        "fixture",
    );
    let resume = tokio::spawn(async move { SessionFactory::resume_subagent(None, config).await });
    work_gate.entered.notified().await;
    work_gate.release.add_permits(1);
    work_gate.committed.notified().await;
    assert_eq!(
        store
            .inner
            .load_session_meta(&CHILD.into())
            .await
            .expect("the into fixture result is required for this assertion")
            .work_state,
        peri_acp_types::thread::WorkState::Working,
        "前置条件：Working 已提交"
    );
    resume.abort();
    assert!(resume
        .await
        .expect_err(
            "cancel_after_working_commit_restores_work_state must reject the invalid fixture"
        )
        .is_cancelled());
    idle(&manager).await;
    let settled = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let meta = store
                .inner
                .load_session_meta(&CHILD.into())
                .await
                .expect("the load session meta fixture result is required for this assertion");
            if meta.agent_status == AgentStatus::Done
                && meta.work_state == peri_acp_types::thread::WorkState::Stopped
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    if settled.is_err() {
        let meta = store
            .inner
            .load_session_meta(&CHILD.into())
            .await
            .expect("the load session meta fixture result is required for this assertion");
        panic!("取消补偿必须恢复工作意图，实际状态为 {meta:?}");
    }
    assert_eq!(starts.load(Ordering::SeqCst), 0, "后台不得启动执行");
    assert_eq!(manager.active_count(), 0);
}

/// [回归] 旧认领的迟到补偿不得覆盖新 resume 的 `Active + Working`。
///
/// 旧补偿被延迟到新 run 已经进入 `Active + claimed + Working` 之后才结清。这个状态
/// 与旧认领写入的中间态完全相同，按记录值判断归属会把新 run 误认为自己的记录，
/// 从而回滚成 `Done + Stopped`；归属只能由进程内认领世代区分。
#[tokio::test]
async fn stale_resume_rollback_preserves_new_working_run() {
    use peri_acp_types::thread::WorkState;
    let (mut store, work_gate, manager) = stopped_fixture(false).await;
    let rollback_gate = Arc::new(WorkRollbackGate::default());
    Arc::get_mut(&mut store)
        .expect("Arc get mut fixture must contain the requested entry")
        .rollback_gate = Some(rollback_gate.clone());

    // 旧认领：旧 run 已工作并写入终态（状态部分已结清），工作意图回滚仍被延迟。
    let old = store
        .claim_child_resume(&CHILD.into(), &CHILD.into())
        .await
        .expect("the claim child resume fixture result is required for this assertion");
    old.hand_off_to_background()
        .await
        .expect("the hand off to background fixture result is required for this assertion");
    store
        .inner
        .set_work_state(&CHILD.into(), WorkState::Working)
        .await
        .expect("the set work state fixture result is required for this assertion");
    store
        .update_session_meta(
            &CHILD.into(),
            &SessionMetaPatch {
                status: Some(AgentStatus::Done),
                ..Default::default()
            },
        )
        .await
        .expect("the update session meta fixture result is required for this assertion");
    let stale = store.last_claim();
    let stale_settle = tokio::spawn(async move { stale.mark_failed().await });
    rollback_gate.entered.notified().await;

    // 旧记录已是非 active：新 run 正常认领并进入模型。
    work_gate.release.add_permits(1);
    let requests = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let mut new_config = config(store.clone(), manager.clone());
    new_config.llm = crate::session::test_resources::mock::model::fixture_source(
        Arc::new(ObservedHangingModel {
            requests: requests.clone(),
            started: started.clone(),
        }),
        "fixture",
    );
    let new_resume = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        SessionFactory::resume_subagent(None, new_config),
    )
    .await
    .expect("the timeout fixture result is required for this assertion")
    .expect("new resume should be accepted after the old run reached a terminal state");
    tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
        .await
        .expect("the timeout fixture result is required for this assertion");
    let before = store
        .inner
        .load_session_meta(&CHILD.into())
        .await
        .expect("the load session meta fixture result is required for this assertion");
    assert_eq!(before.agent_status, AgentStatus::Active);
    assert_eq!(before.work_state, WorkState::Working);
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "new run reached its model"
    );
    assert_eq!(manager.active_count(), 1);

    // 释放迟到的旧补偿：归属校验必须看到新认领世代并放弃回滚。
    rollback_gate.release.add_permits(1);
    stale_settle.await.expect("the stale settle fixture result is required for this assertion").expect("stale resume rollback preserves new working run: the completed fixture operation must provide its result payload");
    let after = store
        .inner
        .load_session_meta(&CHILD.into())
        .await
        .expect("the load session meta fixture result is required for this assertion");
    assert_eq!(after.agent_status, AgentStatus::Active);
    assert_eq!(
        after.work_state,
        WorkState::Working,
        "a stale rollback must not persist Stopped while a newer accepted run is executing"
    );
    new_resume.cancel_token.cancel();
    manager.cancel_all();
    rollback_gate.release.add_permits(10);
}

/// 记录请求数并在第一次请求上挂起的模型：断言新 run 已真正进入执行。
#[derive(Clone)]
struct ObservedHangingModel {
    requests: Arc<AtomicUsize>,
    started: Arc<tokio::sync::Notify>,
}

impl ObservedHangingModel {
    async fn respond(
        &self,
        _: peri_model::ModelRequest,
        _: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        std::future::pending::<()>().await;
        Vec::new()
    }
}
crate::fixture_model_impl!(ObservedHangingModel);

/// 用于阻挡后台 turn 立即收尾：只在运行时被丢弃时结束。
#[derive(Clone)]
struct HangingModel;

impl HangingModel {
    async fn respond(
        &self,
        _: peri_model::ModelRequest,
        _: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        std::future::pending::<()>().await;
        Vec::new()
    }
}
crate::fixture_model_impl!(HangingModel);
