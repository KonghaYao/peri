use super::*;

// ─── S3.1 注册门控 + S3.2 取消收尾（issue 2026-08-05）────────────────────

/// 构造一个已注册状态的 bg 任务（预置 registry 占用额度用）
fn make_registered_bg_task(id: &str) -> peri_agent::agent::async_tasks::BackgroundTask {
    use peri_agent::agent::async_tasks::{
        BackgroundTask, BackgroundTaskStatus, BgCancelHandle, BgTaskKind,
    };
    let handle = tokio::runtime::Handle::current().spawn(async {});
    BackgroundTask {
        id: id.to_string(),
        agent_name: "pre-seeded".to_string(),
        prompt_summary: "pre-seeded task".to_string(),
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::Utc::now(),
        kind: BgTaskKind::Agent,
        cancel_handle: BgCancelHandle::Abort(handle),
        cancel_token: None,
        pid: None,
        output_preview: None,
    }
}

/// [回归测试] S3.1 幽灵任务：注册失败（并发撞 kind 上限）的任务必须不执行。
///
/// 预检（total ≥ 3）与注册（per-kind 上限）之间的竞态无法单测自然触发，
/// 用 barrier 确定性制造：预置 2 个 Agent 任务（total=2），4 个并发 invoke 都
/// 通过 total 预检后同步汇合在 llm_factory，放行后串行注册——agent kind 上限 3
/// 只容 1 个成功，其余 3 个必须：
/// - invoke 返回 "Failed to register" 错误（如实）
/// - 不执行 run_react_loop（零 LLM 调用）
/// - 不 emit 任何事件（无 SubagentStarted → 无配对问题）
/// - 不注册 register_runtime（无需 deregister）
///
/// 历史 bug（issue 2026-08-05）：注册失败仅 return Err，任务已 spawn 继续跑，
/// 幽灵执行 + double 泄漏（register_runtime 无配对 deregister）。
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_bg_register_failure_does_not_execute_task() {
    use peri_agent::agent::events::ExecutorEvent;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Barrier;
    use tokio::sync::mpsc;

    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("gate-agent.md"),
        "---\nname: gate-agent\ndescription: Gate test\n---\n\nYou are gated.\n",
    )
    .unwrap();

    // 预置 2 个 Agent 任务（total=2）：4 个并发 invoke 都能通过 total 预检
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    for i in 0..2 {
        registry
            .register_with_kind(make_registered_bg_task(&format!("bg-pre-{}", i)))
            .unwrap();
    }
    assert_eq!(registry.active_count(), 2);

    // barrier：4 个 invoke 都通过预检并到达 llm_factory 后放行（确定性竞态窗口）
    let gate = Arc::new(Barrier::new(4));
    let llm_calls = Arc::new(AtomicUsize::new(0));
    let llm_calls_clone = Arc::clone(&llm_calls);
    let gate_clone = Arc::clone(&gate);
    // 成功注册的任务阻塞在 LLM 调用（保持 kind 额度占用，防止任务快速完成
    // 触发 complete 移除条目后额度回落、后续注册"假成功"）
    let llm_gate = Arc::new(tokio::sync::Notify::new());
    let llm_gate_clone = Arc::clone(&llm_gate);

    struct GateLLM {
        calls: Arc<AtomicUsize>,
        block: Arc<tokio::sync::Notify>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for GateLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            // 阻塞成功任务：注册窗口内 kind 额度不释放
            self.block.notified().await;
            Ok(Reasoning::with_answer("", "bg gate done"))
        }
    }

    let llm_factory: Arc<dyn Fn(Option<&str>) -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
        Arc::new(move |_: Option<&str>| {
            // 4 个 invoke 在此同步汇合（保证全部通过预检后才放行注册）
            gate_clone.wait();
            Box::new(GateLLM {
                calls: Arc::clone(&llm_calls_clone),
                block: Arc::clone(&llm_gate_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        });

    // register_runtime / deregister_runtime mock：记录调用
    let registered: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let deregistered: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let registered_clone = registered.clone();
    let deregistered_clone = deregistered.clone();
    let register_cb: Arc<dyn Fn(String, AgentCancellationToken, String) + Send + Sync> =
        Arc::new(move |tid, _tok, _pol| {
            registered_clone.lock().unwrap().push(tid);
        });
    let deregister_cb: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |tid| {
        deregistered_clone.lock().unwrap().push(tid.to_string());
    });

    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<ExecutorEvent>();
    let tool = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        llm_factory,
        dir.path().to_str().unwrap().to_string(),
    )
    .with_task_manager(Arc::clone(&registry))
    .with_bg_event_sender(bg_tx)
    .with_register_runtime(register_cb)
    .with_deregister_runtime(deregister_cb);

    // 4 个并发 invoke——必须各自 tokio::spawn（llm_factory 内的 Barrier::wait()
    // 是同步阻塞：若在 join_all 单任务内逐个 poll，第一个 future 会卡死当前
    // worker，其余 3 个永远不被 poll，barrier 凑不齐 4 个参与者而死锁）。
    let tool = Arc::new(tool);
    let mut handles = Vec::new();
    for _ in 0..4 {
        let tool = Arc::clone(&tool);
        let cwd = dir.path().to_str().unwrap().to_string();
        handles.push(tokio::spawn(async move {
            tool.invoke(
                serde_json::json!({
                    "subagent_type": "gate-agent",
                    "run_in_background": true,
                    "prompt": "parallel bg task",
                    "cwd": cwd,
                }),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await
        }));
    }
    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.expect("invoke 任务不应 panic"))
        .collect();

    // 恰好 1 个注册成功，3 个注册失败（错误信息如实返回）
    let oks = results.iter().filter(|r| r.is_ok()).count();
    let errs = results.iter().filter(|r| r.is_err()).count();
    assert_eq!(oks, 1, "恰好 1 个并发任务注册成功，实际 {}", oks);
    assert_eq!(errs, 3, "其余 3 个必须注册失败，实际 {}", errs);
    for r in &results {
        if let Err(e) = r {
            assert!(
                e.to_string().contains("Failed to register"),
                "注册失败错误应如实返回: {}",
                e
            );
        }
    }

    // 等待成功任务 emit SubagentStarted（只有注册成功的任务实际执行）
    let mut started = 0usize;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, bg_rx.recv()).await {
            Ok(Some(ExecutorEvent::SubagentStarted { .. })) => {
                started += 1;
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => break,
        }
    }
    assert_eq!(
        started, 1,
        "只有注册成功的任务 emit SubagentStarted，实际 {}",
        started
    );

    // 成功任务阻塞在 LLM 调用：等待小窗口后断言无任何完成事件（失败任务零事件）
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let mut stopped = 0usize;
    let mut completed = 0usize;
    while let Ok(ev) = bg_rx.try_recv() {
        match ev {
            ExecutorEvent::SubagentStopped { .. } => stopped += 1,
            ExecutorEvent::BackgroundTaskCompleted(_) => completed += 1,
            _ => {}
        }
    }
    assert_eq!(
        stopped, 0,
        "注册失败的任务不得 emit SubagentStopped（无幽灵完成）"
    );
    assert_eq!(completed, 0, "注册失败的任务不得产生完成事件（无幽灵完成）");
    assert_eq!(
        llm_calls.load(Ordering::SeqCst),
        1,
        "注册失败的任务不得执行 run_react_loop（LLM 仅被成功任务调用一次），实际 {}",
        llm_calls.load(Ordering::SeqCst)
    );
    // register_runtime 只在注册成功后执行（失败任务零注册 → 无需 deregister）
    assert_eq!(
        registered.lock().unwrap().len(),
        1,
        "仅注册成功的任务进入 active_agents"
    );
    assert_eq!(
        deregistered.lock().unwrap().len(),
        0,
        "任务仍在运行（阻塞），不得提前 deregister"
    );
    // registry 无幽灵条目：2 预置 + 1 成功注册（任务阻塞未完成）→ 3
    assert_eq!(registry.active_count(), 3, "registry 不应有幽灵条目");
}

/// [回归测试] S3.2 取消收尾：cancel() 先 token.cancel()，任务响应取消链走
/// 完整收尾——SubagentStopped 配对（subagent_depth 归零）、active_agents
/// deregister（任务内同步 guard）、registry 层无幽灵 Completed 事件。
///
/// 历史 bug（issue 2026-08-05）：取消仅 abort，收尾全部跳过（active_agents
/// 泄漏 + depth 错乱 + thread 状态停留 running）。
#[tokio::test]
async fn test_bg_cancel_trigger_token_and_cleanup() {
    use peri_agent::agent::events::ExecutorEvent;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc;

    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("blocking-agent.md"),
        "---\nname: blocking-agent\ndescription: Blocks\n---\n\nYou block.\n",
    )
    .unwrap();

    // LLM 在 generate_reasoning 中阻塞（模拟长时间运行的 bg agent；
    // reason 阶段的 biased select 会在 cancel 后 drop 本 future 并返回 Interrupted）
    let gate = Arc::new(tokio::sync::Notify::new());
    let llm_calls = Arc::new(AtomicUsize::new(0));
    let llm_calls_clone = Arc::clone(&llm_calls);
    let gate_clone = Arc::clone(&gate);

    struct BlockingLLM {
        gate: Arc<tokio::sync::Notify>,
        calls: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for BlockingLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            // 阻塞直到被取消（select 放弃本 future）
            self.gate.notified().await;
            Ok(Reasoning::with_answer("", "never"))
        }
    }

    let llm_factory: Arc<dyn Fn(Option<&str>) -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
        Arc::new(move |_: Option<&str>| {
            Box::new(BlockingLLM {
                gate: Arc::clone(&gate_clone),
                calls: Arc::clone(&llm_calls_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        });

    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<ExecutorEvent>();
    let (reg_events_tx, mut reg_events_rx) =
        mpsc::unbounded_channel::<peri_agent::agent::async_tasks::BgRegistryEvent>();
    registry.set_event_sender(reg_events_tx, "sess-cancel".to_string());

    let deregistered: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let deregistered_clone = deregistered.clone();
    let deregister_cb: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |tid| {
        deregistered_clone.lock().unwrap().push(tid.to_string());
    });

    let tool = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        llm_factory,
        dir.path().to_str().unwrap().to_string(),
    )
    .with_task_manager(Arc::clone(&registry))
    .with_bg_event_sender(bg_tx)
    .with_deregister_runtime(deregister_cb);

    let msg = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "blocking-agent",
                "run_in_background": true,
                "prompt": "block forever",
                "cwd": dir.path().to_str().unwrap(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("bg task should start");
    assert!(msg.contains("Background task"));

    // 等待 LLM 进入阻塞（任务真正运行中，位于 reason 的 select 内）
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while llm_calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("LLM 应被调用（任务运行中）");

    // 取消：token.cancel() 应让任务响应并走完整收尾
    let tasks = registry.list_tasks();
    let (task_id, _, _) = tasks.into_iter().next().expect("任务应已注册");
    registry.cancel(&task_id).unwrap();
    assert_eq!(registry.active_count(), 0, "取消后条目已移除");

    // 事件流：SubagentStopped 必须到达（与 SubagentStarted 配对，depth 归零）
    let mut started = 0usize;
    let mut stopped = 0usize;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, bg_rx.recv()).await {
            Ok(Some(ExecutorEvent::SubagentStarted { .. })) => started += 1,
            Ok(Some(ExecutorEvent::SubagentStopped { .. })) => stopped += 1,
            Ok(Some(ExecutorEvent::BackgroundTaskCompleted(res))) => {
                assert!(!res.success, "取消后任务结果应为失败（interrupted）");
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => break,
        }
    }
    assert_eq!(started, 1);
    assert_eq!(
        stopped, 1,
        "取消后任务应 emit SubagentStopped（与 Started 配对）"
    );

    // active_agents 注销（任务内同步收尾 guard）：complete 后闭包结束触发 drop
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while deregistered.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("取消后任务收尾应 deregister active_agents");
    assert_eq!(deregistered.lock().unwrap().len(), 1);

    // registry 层无幽灵 Completed 事件（complete 对已移除条目返回 false 不推事件）
    let mut saw_completed = false;
    while let Ok(ev) = reg_events_rx.try_recv() {
        if matches!(
            ev,
            peri_agent::agent::async_tasks::BgRegistryEvent::Completed { .. }
        ) {
            saw_completed = true;
        }
    }
    assert!(!saw_completed, "取消后不得推幽灵 Completed 事件");
}
