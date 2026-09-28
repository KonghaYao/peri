use super::*;

// ── 用例 8：cron 端到端（§8 第 6 行 / H04 / V 子计划 `[W2 cron-e2e]`）──────────

/// 稀疏 cron 表达式（5 分钟粒度，5 字段）：注册后**不会**在用例时间窗内自行重复到期，
/// 因此「何时触发」完全由用例的 `force_next_fire_to_past` 控制 —— H04 的因果前提
/// （每 1s 到期的表达式会让触发时刻不受控，拒绝/批准两段的计数会被自发触发污染）。
const W2_SPARSE_CRON: &str = "*/5 * * * *";

/// 主 plan §8 第 6 行（H04 / R23 / A1 / A3 / A25）的收口断言：
/// **effective name 注册 → 真实 tick → 订阅事件 → 触发审批 → 队列 → 真实 continuation**。
///
/// 全链只用生产入口，逐段取证：
/// ① `SearchExtraTools` → `ExecuteExtraTool` 调 `mcp__cron__cron_register`（A20 的等价面；
///    工具级审批走 `SessionContext.broker`，判据 = 审批次数 + 审批看到的**effective name**
///    + 逐字入参 + 组合根 scheduler 真的持有该任务 + 工具结果回带 task_id）；
/// ② 装配的 builtin cron 实例那一代的 1s tick（`drive_cron_tick = true`）→ 生产
///    `SessionCronBridge`（`session/new` 的发布边界订阅一次）→ `run_cron_continuation_scheduler`；
/// ③ 调度触发审批走 **transport** 的 `session/request_permission`（与 ① 的 broker 面互不干扰）；
/// ④ 批准 ⇒ `enqueue_cron_trigger` 入队 `QueuedMessage(MessageSource::CronTrigger)`
///    （用 prompt lock 闸门卡住 dispatch，从**队列**读出该事实）；
/// ⑤ 放行 ⇒ 真实 continuation turn（模型来自会话池里预置的脚本化模型，零网络），
///    判据 = 模型请求里出现该 prompt 的 cron reminder 正文 + wire 收到终态事件 + history 增长。
///
/// **拒绝与批准分别判定，且在同一夹具同一条链上**（唯一变量是审批决策）：拒绝段在前，
/// 因此批准段同时是拒绝段的正控（「什么都没发生」不是夹具空转）。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn cron_register_tick_approval_continuation() {
    const CRON_PROMPT: &str = "w2-cron-e2e-prompt";
    /// 拒绝 ⇒ 不入队：给调度分支落地留的有界宽限（审批返回后该分支立即 return）。
    const REJECT_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

    let fixture = AssembledHostFixture::start(FixtureDirs::new(), true).await;
    fixture.assert_all_ready();
    let scheduler = fixture.cron_scheduler();
    let mut register_ctx = fixture.session_context("w2-cron-e2e-register").await;
    let AssembledHostFixture {
        dirs,
        cfg,
        pool: _pool,
        // MCP task owner 必须活到用例结束：builtin 实例的 task 归属在它手上。
        _owner: mcp_owner,
    } = fixture;
    let cfg = Arc::new(cfg);
    // 调度触发审批只在非 Bypass 模式下发生（Bypass 在 `approve_scheduled_trigger` 早退）。
    cfg.permission_mode.store(PermissionMode::Default);

    // ── ① effective name 注册（真实 turn + 工具级审批）────────────────────────
    let register_broker = Arc::new(ToolApprovalBroker::new(true));
    let register_sink = Arc::new(MockEventSink::new());
    let register_model = Arc::new(WireScriptedModel::new(vec![
        ScriptedToolCall::new(
            SEARCH_EXTRA_TOOLS_NAME,
            json!({ "query": "cron register scheduled task", "max_results": 50 }),
        ),
        ScriptedToolCall::new(
            "ExecuteExtraTool",
            json!({
                "tool_name": "mcp__cron__cron_register",
                "params": { "expression": W2_SPARSE_CRON, "prompt": CRON_PROMPT }
            }),
        ),
    ]));
    register_ctx.broker = Arc::clone(&register_broker) as Arc<dyn UserInteractionBroker>;
    register_ctx.permission_mode = SharedPermissionMode::new(PermissionMode::Default);
    let register_turn = run_wire_prompt(register_ctx, &register_sink, &register_model).await;
    assert!(
        register_turn.ok,
        "注册 turn 必须正常收束: {:?}",
        register_turn.failure
    );
    let register_name = declared_effective_names("cron")
        .first()
        .copied()
        .expect("cron 必须声明工具");
    assert_eq!(
        register_name, "mcp__cron__cron_register",
        "注册工具的 effective name 变了：本用例的断言面必须同步"
    );
    assert_eq!(
        register_broker.requests(),
        1,
        "cron_register 是 mutation ⇒ 恰好一次工具级审批（不重试、不批量）"
    );
    let register_seen = register_broker.seen();
    assert_eq!(
        register_seen,
        vec![(
            register_name.to_string(),
            json!({ "expression": W2_SPARSE_CRON, "prompt": CRON_PROMPT })
        )],
        "工具级审批看到的必须是**模型面 effective name** + 逐字入参（A14：事件载荷不归一）"
    );
    let tool_ends = tool_end_events(&register_sink);
    let registered = tool_ends
        .iter()
        .find(|(name, _, _)| name == register_name)
        .unwrap_or_else(|| panic!("工具结果必须落在 effective name 上: {tool_ends:?}"));
    assert!(
        !registered.2,
        "注册必须成功（非 error 结果）: {registered:?}"
    );
    let tasks: Vec<(String, String, String)> = scheduler
        .lock()
        .list_tasks()
        .iter()
        .map(|task| {
            (
                task.id.clone(),
                task.expression.clone(),
                task.prompt.clone(),
            )
        })
        .collect();
    assert_eq!(
        tasks.len(),
        1,
        "注册后组合根 scheduler 必须恰有一条任务（工具面与宿主端口同源）: {tasks:?}"
    );
    assert_eq!(tasks[0].1, W2_SPARSE_CRON, "注册的表达式必须逐字保留");
    assert_eq!(tasks[0].2, CRON_PROMPT, "注册的 prompt 必须逐字保留");
    let task_id = tasks[0].0.clone();
    assert!(
        registered.1.contains(&task_id),
        "工具结果必须回带该 task_id（模型可见面与组合根状态互相印证）: {registered:?}"
    );
    println!(
        "[W2 cron-e2e] register_approval=1 name={register_name} input={:?} → tick_enabled=true 的一代 mounted；组合根任务 task_id={task_id}",
        register_seen[0].1
    );

    // ── ② 生产 session 装配（session/new ⇒ bridge 订阅 + v2 队列）─────────────
    let (cron_cont_tx, cron_cont_rx) = tokio::sync::mpsc::unbounded_channel();
    cfg.session_manager.bind_cron_continuation(cron_cont_tx);
    let transport = Arc::new(ApprovalTransport::new(false));
    let dyn_transport: Arc<dyn AcpTransport> = Arc::clone(&transport) as Arc<dyn AcpTransport>;
    let mut sessions = HashMap::new();
    let created = crate::host::requests::handle_request(
        "session/new",
        &json!({ "cwd": dirs.workspace_str() }),
        &cfg,
        &mut sessions,
        &dyn_transport,
    )
    .await
    .expect("session/new 必须成功");
    let session_id = created["sessionId"]
        .as_str()
        .expect("sessionId")
        .to_string();
    assert!(
        cfg.session_manager
            .get_session(&session_id)
            .is_some_and(|session| session.cron_bridge.is_some()),
        "session 发布边界必须启动 cron bridge（A3 路线 B：对 scheduler `subscribe()` 一次）"
    );
    let queue = cfg
        .session_manager
        .v2_queue_for(&session_id)
        .expect("session 必须有 v2 队列");

    // 会话级脚本化模型：生产 turn 从会话池的 `subagent_llm_cache` 取主模型（零网络）。
    let turn_model = Arc::new(TurnRecordingModel::new());
    let model_fingerprint = crate::session::agent_pool::fingerprint(&cfg.provider.read().clone());
    sessions
        .get_mut(&session_id)
        .expect("session state 必须已注册")
        .agent_pool
        .subagent_llm_cache
        .insert(model_fingerprint, Arc::clone(&turn_model) as Arc<dyn Model>);

    // prompt lock 闸门：测试先持锁 ⇒ 入队事实可见，而 dispatch 阻塞在锁上。
    let gate = Arc::new(tokio::sync::Mutex::new(()));
    let gate_guard = gate.lock().await;
    let shared: SharedSessions = Arc::new(tokio::sync::Mutex::new(sessions));
    let locks: PromptLocks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    locks
        .lock()
        .await
        .insert(session_id.clone(), Arc::clone(&gate));

    // ── ③ 生产 continuation scheduler（与 `run_acp_server` 同款装配）───────────
    let (cont_tx, _cont_rx) = tokio::sync::mpsc::unbounded_channel();
    assert!(
        cfg.host_task_spawner
            .spawn(
                crate::host::task_scope::HostTaskOwnerKind::Host,
                crate::host::task_scope::HostTaskKind::ContinuationScheduler,
                crate::host::run_cron_continuation_scheduler(
                    cron_cont_rx,
                    crate::host::CronContinuationContext {
                        sessions: Arc::clone(&shared),
                        prompt_locks: Arc::clone(&locks),
                        cfg: Arc::clone(&cfg),
                        transport: Arc::clone(&dyn_transport),
                        cont_tx: Arc::new(cont_tx),
                        task_spawner: cfg.host_task_spawner.clone(),
                        shutdown: cfg.host_task_spawner.shutdown_token(),
                    },
                ),
            )
            .is_ok(),
        "continuation scheduler 必须被 task owner 接受"
    );

    // ── ④ 拒绝路径：审批到达即被拒 ⇒ 不入队、无 continuation ───────────────────
    assert!(
        scheduler.lock().force_next_fire_to_past(&task_id),
        "反控前置：任务必须仍在组合根任务表内"
    );
    assert!(
        wait_until("触发审批到达", std::time::Duration::from_secs(15), || {
            transport.approvals() >= 1
        })
        .await,
        "真实 tick 必须在有界等待内驱动出一次触发审批（tick → subscribe 事件 → bridge → scheduler → transport 全链）"
    );
    tokio::time::sleep(REJECT_GRACE).await;
    let reject_records = transport.requests();
    assert_eq!(
        reject_records.len(),
        1,
        "拒绝段必须恰一次触发审批: {reject_records:?}"
    );
    assert_eq!(
        reject_records[0]["sessionId"],
        json!(session_id),
        "触发审批必须指名该 session（不允许广播到别的会话）: {:?}",
        reject_records[0]
    );
    assert_eq!(
        reject_records[0]["toolCall"]["toolCallId"],
        json!(task_id),
        "触发审批载荷必须指名该 task_id: {:?}",
        reject_records[0]
    );
    assert_eq!(
        reject_records[0]["toolCall"]["title"],
        json!("cron_trigger"),
        "触发审批的工具名必须是 cron_trigger: {:?}",
        reject_records[0]
    );
    assert_eq!(
        reject_records[0]["toolCall"]["rawInput"]["prompt"],
        json!(CRON_PROMPT),
        "触发审批必须携带该任务的 prompt: {:?}",
        reject_records[0]
    );
    assert!(
        !queue.has_pending_defer(&MessageSource::CronTrigger),
        "拒绝 ⇒ 不得入队 CronTrigger Defer"
    );
    assert_eq!(queue.len(), 0, "拒绝 ⇒ 队列必须为空");
    assert_eq!(
        turn_model.calls(),
        0,
        "拒绝 ⇒ 不得产生 continuation turn（模型调用 = 0）"
    );
    println!(
        "[W2 cron-e2e] trigger_approval=1(reject) task_id={task_id} → queue=0 len=0 continuation=0（审批调用计数可观测）"
    );

    // ── ⑤ 批准路径（同链正控）：入队可见 → 放行 ⇒ 真实 continuation turn ──────
    transport.set_approve(true);
    assert!(
        scheduler.lock().force_next_fire_to_past(&task_id),
        "批准段前置：任务必须仍在组合根任务表内（实例生命周期不影响组合根状态）"
    );
    assert!(
        wait_until("CronTrigger 入队", std::time::Duration::from_secs(15), || queue
            .has_pending_defer(&MessageSource::CronTrigger))
        .await,
        "触发审批通过 ⇒ 必须入队 QueuedMessage(MessageSource::CronTrigger)（dispatch 被 prompt lock 闸门卡住，队列状态可见）"
    );
    assert_eq!(queue.len(), 1, "入队必须恰一条（CronTrigger Defer）");
    assert_eq!(
        transport.approvals(),
        2,
        "触发审批计数必须可观测：拒绝段 1 次 + 批准段 1 次"
    );
    println!(
        "[W2 cron-e2e] trigger_approval=2(approve) → queue=QueuedMessage(MessageSource::CronTrigger)×1（dispatch 被闸门卡住时读出；len={}）",
        queue.len()
    );

    drop(gate_guard);
    assert!(
        wait_until(
            "continuation turn 开始",
            std::time::Duration::from_secs(30),
            || { turn_model.calls() >= 1 }
        )
        .await,
        "放行闸门后必须发生一次真实 continuation turn（模型被调用）"
    );
    // 「模型被调用」只说明 turn **开始**了：会话历史与终态事件在 turn 收尾时才落定，
    // 因此必须显式等到终态事件，再取会话侧事实（否则负载下会读到中间态）。
    assert!(
        wait_until(
            "continuation turn 收尾（wire 终态事件）",
            std::time::Duration::from_secs(30),
            || {
                let wire = transport.notifications();
                wire.iter().any(|method| method == "session/update")
                    && wire.iter().any(|method| method == "peri/agent_event_done")
            }
        )
        .await,
        "新 turn 必须向 wire 发布会话更新与终态事件（session/update + peri/agent_event_done）: {:?}",
        transport.notifications()
    );
    let history_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let history_len = loop {
        let observed = shared
            .lock()
            .await
            .get(&session_id)
            .map(|state| state.history.len());
        if observed == Some(1) || tokio::time::Instant::now() >= history_deadline {
            break observed;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    let requests = turn_model.requests();
    assert_eq!(requests.len(), 1, "continuation 必须恰一次模型请求");
    // reminder 正文在生产渲染里是 XML 转义过的（trusted reminder 的 `<goal-message>` 体），
    // 因此断言取**正文内容**这个稳定子串 + 归属属性，而不是转义形态本身。
    let reminder_body = format!("Cron task {task_id} triggered: {CRON_PROMPT}");
    let reminder_open = requests[0]
        .split("<system-reminder")
        .nth(1)
        .map(|tail| format!("<system-reminder{tail}"))
        .unwrap_or_else(|| "<无 system-reminder>".to_string());
    let tasks_after = scheduler
        .lock()
        .list_tasks()
        .iter()
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    assert!(
        requests[0].contains(&reminder_body),
        "模型请求必须包含该 cron 触发的 reminder 正文（队列 Defer 真的进了上下文）: 期望含 `{reminder_body}`，实际 {reminder_open}；scheduler 现有任务 = {tasks_after:?}"
    );
    assert!(
        requests[0].contains("source=\"cron\"") && requests[0].contains("kind=\"triggered\""),
        "reminder 必须标注 source=cron / kind=triggered（生产 reminder 工厂的投影）: {reminder_open}"
    );
    assert!(
        queue.is_empty(),
        "真实 continuation 必须消费掉该 Defer（队列清空）"
    );
    let wire = transport.notifications();
    assert!(
        wire.iter().any(|method| method == "session/update"),
        "新 turn 必须向 wire 发布会话更新: {wire:?}"
    );
    assert_eq!(
        history_len,
        Some(1),
        "新 turn 必须写入会话历史（一条 assistant 收尾）: {history_len:?}"
    );
    drop(mcp_owner);
    println!(
        "[W2 cron-e2e] dispatch=真实 continuation turn（model_calls=1，请求含 cron reminder 正文 source=cron）；wire 收到 session/update + peri/agent_event_done；会话 history={history_len:?}；queue 已消费=空"
    );
}
