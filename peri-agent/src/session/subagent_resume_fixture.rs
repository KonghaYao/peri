// Responsibility-specific cases share the original test namespace and fixtures.

// ─── resume_subagent 用例（slice 4/5 重建 + 执行） ─────────────────────────

/// 构造最小 resume config（默认：EchoLLM / 后台执行 / 新 TaskManager / 无 cancel_token）
fn resume_config(
    session_resources: Arc<MockSessionResources>,
    thread_id: String,
) -> SubagentResumeConfig {
    resume_config_with(
        session_resources,
        thread_id,
        crate::session::test_resources::mock::model::fixture_source(
            std::sync::Arc::new(EchoLLM),
            "fixture-scripted",
        ),
        Some(Arc::new(TaskManager::new())),
        None,
    )
}

/// 构造带自定义装配/运行参数的 resume config
#[allow(clippy::too_many_arguments)]
fn resume_config_with(
    session_resources: Arc<dyn peri_acp_types::session_resources::SessionResources>,
    thread_id: String,
    llm: SubagentLlmSource,
    task_manager: Option<Arc<TaskManager>>,
    cancel_token: Option<CancellationToken>,
) -> SubagentResumeConfig {
    SubagentResumeConfig {
        thread_id,
        prompt: None,
        agent_name: None,
        max_iterations: 200,
        llm,
        chain_assembler: Arc::new(EmptyChainAssembler),
        tools: Vec::new(),
        tool_filter: Arc::new(|_| true),
        tool_invocation_resolver: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        session_resources: Arc::clone(&session_resources)
            as Arc<dyn peri_acp_types::session_resources::SessionResources>,
        event_handler: None,
        bg_event_sender: None,
        task_manager,
        on_bg_complete: None,
        langfuse_bridge: None,
        on_subagent_start: None,
        on_subagent_stop: None,
        register_runtime: None,
        deregister_runtime: None,
        parent_agent_id: None,
        parent_tool_call_id: None,
        cancel_token,
        cwd: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_date: None,
    }
}

/// 记录型 mock LLM：记录每次收到的消息列表并返回固定答案
/// （断言 resume 重放的 transcript 内容 / 末条截断行为）
#[derive(Clone)]
struct RecordingLLM {
    received: Arc<RwLock<Vec<Vec<BaseMessage>>>>,
    answer: String,
}

impl RecordingLLM {
    fn new() -> Self {
        Self {
            received: Arc::new(RwLock::new(Vec::new())),
            answer: "recorded-answer".to_string(),
        }
    }
}

impl RecordingLLM {
    async fn respond(
        &self,
        request: peri_model::ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        let _ = cancellation;
        use crate::session::test_resources::mock::model as fixture;
        self.received.write().push(fixture::base_messages(&request));
        fixture::text_events(self.answer.clone())
    }
}
crate::fixture_model_impl!(RecordingLLM);

/// 门控 mock LLM：首次 generate_reasoning 阻塞，直到测试侧 `release_tx.send(())`
/// 放行。oneshot 有信号缓冲——即使 send 先于 LLM 的 await 发生也不会丢失唤醒。
/// 用于让 resume 执行进入稳定挂起状态（并发互斥 / bg 注册断言）。
#[derive(Clone)]
struct GateLLM {
    /// 首次调用等待的放行接收端（首次调用 take 后为 None）
    gate: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
    /// 已调用次数（测试侧轮询确认挂起生效）
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl GateLLM {
    fn new() -> (Self, tokio::sync::oneshot::Sender<()>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        (
            Self {
                gate: Arc::new(std::sync::Mutex::new(Some(rx))),
                calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            },
            tx,
        )
    }

    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl GateLLM {
    async fn respond(
        &self,
        _request: peri_model::ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        let _ = cancellation;
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            let rx = {
                let mut guard = self.gate.lock().expect("gate mutex poisoned");
                guard.take()
            };
            if let Some(rx) = rx {
                let _ = rx.await;
            }
        }
        crate::session::test_resources::mock::model::text_events("gated-answer")
    }
}
crate::fixture_model_impl!(GateLLM);

#[derive(Clone)]
struct CancelGateLLM {
    entered: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

impl CancelGateLLM {
    fn new() -> (Self, tokio::sync::oneshot::Receiver<()>) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        (
            Self {
                entered: Arc::new(std::sync::Mutex::new(Some(entered_tx))),
            },
            entered_rx,
        )
    }

    async fn respond(
        &self,
        _request: peri_model::ModelRequest,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        if let Some(entered) = self
            .entered
            .lock()
            .expect("self entered mutex must remain unpoisoned throughout the test")
            .take()
        {
            let _ = entered.send(());
        }
        // 挂起直到 bridge 因取消 abort 该流（future drop）。
        std::future::pending::<Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>>>().await
    }
}
crate::fixture_model_impl!(CancelGateLLM);

/// 预置可恢复 thread：创建 + 置非 active（status "done"）。
/// 消息由各测试按需 append。
async fn preset_resumable_thread(
    store: &MockSessionResources,
    thread_id: &str,
    parent_thread_id: Option<&str>,
) {
    let mut meta = ThreadMeta::new_at("/tmp/work", peri_time::now_wall());
    meta.id = thread_id.to_string();
    meta.parent_thread_id = parent_thread_id.map(|s| s.to_string());
    store
        .create_resumable_thread(meta)
        .await
        .expect("the create resumable thread fixture result is required for this assertion");
    store
        .update_thread_status(&thread_id.to_string(), "done")
        .await
        .expect("the update thread status fixture result is required for this assertion");
}

/// 断言 resume_subagent 返回 Err 并取回错误文本（SubagentSpawned 无 Debug，
/// 不能直接用 unwrap_err）
async fn resume_err(parent: Option<&Arc<Session>>, config: SubagentResumeConfig) -> String {
    match SessionFactory::resume_subagent(parent, config).await {
        Err(e) => e.to_string(),
        Ok(_) => panic!("resume_subagent 应返回 Err（校验失败或重建失败）"),
    }
}

/// 等待后台 resume 提交终态：状态离开 Active 即本次执行已收尾。
///
/// resume 返回时认领已写入 Active，因此这里的等待不会误判「尚未开始」。
async fn wait_resume_settled(store: &MockSessionResources, thread_id: &str) {
    let thread_id = thread_id.to_string();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let changed = store.status_changed.notified();
            if !store
                .load_meta(&thread_id)
                .await
                .expect("the load meta fixture result is required for this assertion")
                .agent_status
                .is_active()
            {
                break;
            }
            changed.await;
        }
    })
    .await
    .expect("background resume must settle the persisted status");
}

/// 等待后台 spawn/resume 的 TaskManager 执行账本清空（终态先于完成提交）。
async fn wait_background_idle(manager: &TaskManager) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !peri_acp_types::tasks::TaskManager::is_execution_idle(manager) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("background execution must settle");
}

/// 等待任意存储后端上的 child 离开 Active（后台执行已提交终态）。
///
/// 后台 spawn/resume 在返回后才执行；以持久状态为完成信号，避免按请求数或
/// 队列长度判断时把「仍在跑」误当成「已收尾」。
async fn wait_status_settled(
    store: &dyn peri_acp_types::session_resources::SessionResources,
    thread_id: &str,
) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status = store
                .load_session_meta(&thread_id.to_string())
                .await
                .expect("the load session meta fixture result is required for this assertion")
                .agent_status;
            if !status.is_active() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("background child must settle its persisted status");
}
