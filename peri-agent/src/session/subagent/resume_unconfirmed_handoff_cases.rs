// Responsibility-specific cases share the original test namespace and fixtures.

#[derive(Clone)]
struct FailWithUnwritableStore(Arc<MockSessionResources>);

impl FailWithUnwritableStore {
    async fn respond(
        &self,
        _request: peri_model::ModelRequest,
        _cancellation: CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        self.0.restrict_to_history_read_only();
        vec![Err(peri_model::ModelError::http_status(
            400,
            "fixture",
            None::<&str>,
        ))]
    }
}
crate::fixture_model_impl!(FailWithUnwritableStore);

async fn test_unconfirmed_handoff_blocks_factory_resume() {
    use crate::agent::async_tasks::BgTaskKind;
    use crate::session::subagent::resource_owner::ParkedChildOwner;
    use crate::session::{FrozenContext, MessageQueue, Session};
    use peri_acp_types::tasks::BgTaskRegistration;
    use peri_acp_types::tasks::TaskManager as _;
    use std::time::Duration;

    let store = MockSessionResources::new();
    let child_id = uuid::Uuid::now_v7().to_string();
    let mut meta = ThreadMeta::new_at("/tmp", peri_time::now_wall());
    meta.id = child_id.clone();
    meta.parent_thread_id = Some("parent-thread".into());
    store
        .create_resumable_thread(meta)
        .await
        .expect("the create resumable thread fixture result is required for this assertion");
    store
        .update_thread_status(&child_id, "error")
        .await
        .expect("the update thread status fixture result is required for this assertion");
    let parent_manager = Arc::new(TaskManager::new());
    let child_manager = Arc::new(TaskManager::new());
    child_manager
        .register(BgTaskRegistration {
            task_id: "unconfirmed-shell".into(),
            kind: BgTaskKind::Shell,
            summary: "测试中的后台任务".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        })
        .expect("the register fixture result is required for this assertion");
    let owner = ParkedChildOwner::new(
        child_id.clone(),
        "parent-thread".into(),
        child_manager,
        MessageQueue::new(),
        Some(store.clone()),
        None,
    );
    parent_manager
        .register_child_owner(owner.clone())
        .expect("the register child owner fixture result is required for this assertion");
    let parent = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some("parent-thread".into()),
    );
    parent.set_subagent_host(SubagentHost {
        task_manager: Some(parent_manager.clone()),
        ..Default::default()
    });
    let mut config = crate::session::subagent::tests::resume_config_with(
        store.clone(),
        child_id.clone(),
        crate::session::test_resources::mock::model::fixture_source(
            Arc::new(FailWithUnwritableStore(store.clone())),
            "fixture-scripted",
        ),
        Some(parent_manager),
        None,
    );
    let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
    let finished_tx = parking_lot::Mutex::new(Some(finished_tx));
    config.deregister_runtime = Some(Arc::new(move |_| {
        if let Some(sender) = finished_tx.lock().take() {
            let _ = sender.send(());
        }
    }));
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let result_tx = parking_lot::Mutex::new(Some(result_tx));
    config.on_bg_complete = Some(Arc::new(move |result, _| {
        if let Some(sender) = result_tx.lock().take() {
            let _ = sender.send(result.clone());
        }
        Ok(())
    }));
    let first = SessionFactory::resume_subagent(Some(&parent), config).await;
    first.expect("后台执行须成功启动");
    let result = tokio::time::timeout(Duration::from_secs(10), result_rx)
        .await
        .expect("后台失败必须完成")
        .expect("the expect fixture result is required for this assertion");
    assert!(!result.success);
    tokio::time::timeout(Duration::from_secs(10), finished_rx)
        .await
        .expect("旧执行必须完成")
        .expect("the expect fixture result is required for this assertion");
    assert!(owner.pending_receipt().is_some(), "未确认交接身份必须保留");
    assert!(owner.receipt().is_none());
    let error = SessionFactory::resume_subagent(
        Some(&parent),
        crate::session::subagent::tests::resume_config(store, child_id),
    )
    .await
    .expect_err("未确认交接必须阻止 resume")
    .to_string();
    assert!(error.contains("handoff is unconfirmed"), "{error}");
}

/// [回归测试] 后台交接真实写入失败时，factory 明确拒绝恢复。
#[tokio::test(start_paused = true)]
async fn test_background_unconfirmed_handoff_blocks_factory_resume() {
    test_unconfirmed_handoff_blocks_factory_resume().await;
}
