use super::*;

// ── 用例 8：cron 端到端（§8 第 6 行 / H04 / V 子计划 `[W2 cron-e2e]`）──────────

/// 稀疏 cron 表达式（5 分钟粒度，5 字段）：注册后**不会**在用例时间窗内自行重复到期，
/// 因此「何时触发」完全由用例的 `force_next_fire_to_past` 控制 —— H04 的因果前提
/// （每 1s 到期的表达式会让触发时刻不受控，拒绝/批准两段的计数会被自发触发污染）。
const W2_SPARSE_CRON: &str = "*/5 * * * *";

/// Actual MCP registration/tick requires approval before the current runtime consumes a trigger.
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn cron_register_tick_approval_continuation() {
    const CRON_PROMPT: &str = "w2-cron-e2e-prompt";

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
        // 会话任务管理器同上：pool 只持 Weak，registration 期间必须在世。
        session_tasks: _session_tasks,
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
    assert!(
        cfg.host_task_spawner
            .spawn(
                crate::host::task_scope::HostTaskOwnerKind::Host,
                crate::host::task_scope::HostTaskKind::ContinuationScheduler,
                crate::host::run_cron_continuation_scheduler(
                    cron_cont_rx,
                    crate::host::CronContinuationContext {
                        sessions: shared.clone(),
                        prompt_locks: locks.clone(),
                        cont_tx: Arc::new(tokio::sync::mpsc::unbounded_channel().0),
                        cfg: Arc::clone(&cfg),
                        transport: Arc::clone(&dyn_transport),
                        task_spawner: cfg.host_task_spawner.clone(),
                        shutdown: cfg.host_task_spawner.shutdown_token(),
                    },
                ),
            )
            .is_ok(),
        "continuation scheduler 必须被 task owner 接受"
    );

    assert!(scheduler.lock().force_next_fire_to_past(&task_id));
    assert!(
        wait_until(
            "rejected scheduled approval",
            std::time::Duration::from_secs(15),
            || transport.approvals() == 1
        )
        .await
    );
    assert!(queue.is_empty());
    assert_eq!(turn_model.calls(), 0);
    transport.set_approve(true);
    assert!(scheduler.lock().force_next_fire_to_past(&task_id));
    assert!(
        wait_until(
            "approved trigger queued before prompt lock",
            std::time::Duration::from_secs(15),
            || queue.has_pending_defer(&MessageSource::CronTrigger)
        )
        .await
    );
    assert_eq!(transport.approvals(), 2);
    assert_eq!(transport.requests().len(), 2);
    assert_eq!(turn_model.calls(), 0);
    drop(gate_guard);
    assert!(
        wait_until(
            "approved scheduled turn",
            std::time::Duration::from_secs(15),
            || turn_model.calls() > 0
        )
        .await
    );
    assert!(turn_model
        .requests()
        .iter()
        .any(|request| request.contains(CRON_PROMPT)));
    assert!(transport
        .notifications()
        .iter()
        .all(|method| method != "session/work/available"));
    drop(mcp_owner);
}
