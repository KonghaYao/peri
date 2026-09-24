use super::*;

// ─── S3.1 注册门控 + S3.2 取消收尾（issue 2026-08-05）────────────────────

/// 构造一个已注册状态的 bg 任务（预置 registry 占位用；kind = Agent，
/// Agent 类无并发上限）
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
        agent_inbox: None,
    }
}

/// [回归测试] S3.1 幽灵任务：spawn 前失败（session execution scope 已关闭）的
/// 任务必须不执行。
///
/// 触发点：`TaskManager::shutdown()` 关闭 scope 后，后台挂载 admission 被拒——
/// 4 个并发 invoke 在 llm_factory 同步汇合（barrier 制造确定性并发窗口）后全部
/// 失败，每个必须：
/// - invoke 如实返回错误（带关闭原因）
/// - 不执行 run_react_loop（零 LLM 推理调用）
/// - 不 emit 任何事件（无 SubagentStarted → 无配对问题）
/// - 不注册 register_runtime（无需 deregister）
///
/// 历史 bug（issue 2026-08-05）：注册失败仅 return Err，任务已 spawn 继续跑，
/// 幽灵执行 + double 泄漏（register_runtime 无配对 deregister）。
///
/// 触发点变更：原以 agent per-kind 上限（AGENT_LIMIT=3）制造注册失败；Agent 类
/// 后台任务取消并发上限后（放开并行委派），该类注册失败只剩 scope 关闭一类，
/// 且该失败在挂载（spawn_owned）处即被拒——「失败 ⇒ 零执行零事件」的不变量
/// 本用例仍完整覆盖；注册后失败的门控（background.rs 的 oneshot 放行）现在只在
/// shutdown 竞态窗口内可达，不再有确定性触发点。
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

    // 关闭 session execution scope：4 个并发 invoke 的挂载/注册 admission 全部被拒
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    assert_eq!(
        peri_acp_types::tasks::TaskManager::shutdown(registry.as_ref()).await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
    assert_eq!(registry.active_count(), 0);

    // barrier：4 个 invoke 全部到达 llm_factory 后放行（确定性并发窗口）
    let gate = Arc::new(Barrier::new(4));
    let llm_calls = Arc::new(AtomicUsize::new(0));
    let llm_calls_clone = Arc::clone(&llm_calls);
    let gate_clone = Arc::clone(&gate);

    struct GateLLM {
        calls: Arc<AtomicUsize>,
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
            Ok(Reasoning::with_answer("", "bg gate done"))
        }
    }

    let llm_factory: Arc<dyn Fn(Option<&str>) -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
        Arc::new(move |_: Option<&str>| {
            // 4 个 invoke 在此同步汇合（全部进入装配窗口后才放行）
            gate_clone.wait();
            Box::new(GateLLM {
                calls: Arc::clone(&llm_calls_clone),
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

    // 4 个全部失败（错误信息如实返回，带 scope 关闭原因）
    let oks = results.iter().filter(|r| r.is_ok()).count();
    let errs = results.iter().filter(|r| r.is_err()).count();
    assert_eq!(oks, 0, "scope 关闭后不得有任务启动成功，实际 {}", oks);
    assert_eq!(errs, 4, "4 个并发任务必须全部如实报错，实际 {}", errs);
    for r in &results {
        if let Err(e) = r {
            assert!(
                e.to_string().contains("closing"),
                "执行前失败错误应如实返回，got: {}",
                e
            );
        }
    }

    // 失败任务零事件：等待小窗口后断言无 Started / Stopped / 完成事件
    let mut started = 0usize;
    let mut stopped = 0usize;
    let mut completed = 0usize;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, bg_rx.recv()).await {
            Ok(Some(ExecutorEvent::SubagentStarted { .. })) => started += 1,
            Ok(Some(ExecutorEvent::SubagentStopped { .. })) => stopped += 1,
            Ok(Some(ExecutorEvent::BackgroundTaskCompleted(_))) => completed += 1,
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => break,
        }
    }
    assert_eq!(started, 0, "失败任务不得 emit SubagentStarted");
    assert_eq!(
        stopped, 0,
        "执行前失败的任务不得 emit SubagentStopped（无幽灵完成）"
    );
    assert_eq!(
        completed, 0,
        "执行前失败的任务不得产生完成事件（无幽灵完成）"
    );
    assert_eq!(
        llm_calls.load(Ordering::SeqCst),
        0,
        "失败任务不得执行 run_react_loop，实际调用 {}",
        llm_calls.load(Ordering::SeqCst)
    );
    // register_runtime 只在注册成功后执行（失败任务零注册 → 无需 deregister）
    assert_eq!(
        registered.lock().unwrap().len(),
        0,
        "失败任务不得进入 active_agents"
    );
    assert_eq!(
        deregistered.lock().unwrap().len(),
        0,
        "无注册即无 deregister"
    );
    // registry 无幽灵条目
    assert_eq!(registry.active_count(), 0, "registry 不应有幽灵条目");
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

// ─── 后台 sub-agent 并发不设上限（原 "Maximum 3" 上限已移除）────────────────

/// 后台 sub-agent 并发上限已移除：registry 已有 4 个在跑 Agent 任务时，
/// Agent 工具（run_in_background）仍可继续启动第 5..10 个；6 个任务的
/// 启动 → 完成通知 → 取消 → 清理四条路径必须全部完整。
///
/// 覆盖：
/// - 启动：超过 3 个全部成功（旧实现在工具入口以 `active_count() >= 3` 拒绝）
/// - 完成通知：3 个放行后各 emit SubagentStopped + BackgroundTaskCompleted(success)
/// - 取消：3 个取消后各 emit SubagentStopped + BackgroundTaskCompleted(!success)
/// - 清理：6 个任务全部 deregister active_agents、registry 收敛回占位条目
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_bg_more_than_three_concurrent_tasks_start_complete_cancel() {
    use peri_agent::agent::events::ExecutorEvent;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc;

    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("bulk-agent.md"),
        "---\nname: bulk-agent\ndescription: Bulk test\n---\n\nYou bulk.\n",
    )
    .unwrap();

    // 占位任务 4 个（>3）：证明第 N(>3) 个后台 sub-agent 不被拒绝
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    for i in 0..4 {
        registry
            .register_with_kind(make_registered_bg_task(&format!("placeholder-{}", i)))
            .unwrap();
    }
    assert_eq!(registry.active_count(), 4);

    // LLM 阻塞在信号量上：任务停留在运行中，直到放行（取消经 select 放弃 future）
    let llm_calls = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let llm_calls_clone = Arc::clone(&llm_calls);
    let release_clone = Arc::clone(&release);

    struct BulkLLM {
        calls: Arc<AtomicUsize>,
        release: Arc<tokio::sync::Semaphore>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for BulkLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let _ = self.release.acquire().await;
            Ok(Reasoning::with_answer("", "bulk done"))
        }
    }

    let llm_factory: Arc<dyn Fn(Option<&str>) -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
        Arc::new(move |_: Option<&str>| {
            Box::new(BulkLLM {
                calls: Arc::clone(&llm_calls_clone),
                release: Arc::clone(&release_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        });

    let deregistered: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let deregistered_clone = deregistered.clone();
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
    .with_deregister_runtime(deregister_cb);

    // 启动 6 个后台任务：全部必须成功返回（不再有并发上限拦截）
    let mut task_ids = Vec::new();
    for i in 0..6 {
        let msg = tool
            .invoke(
                serde_json::json!({
                    "subagent_type": "bulk-agent",
                    "run_in_background": true,
                    "prompt": format!("bulk task {}", i),
                    "cwd": dir.path().to_str().unwrap(),
                }),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await
            .unwrap_or_else(|e| panic!("第 {} 个后台任务不应被拒绝: {}", i + 1, e));
        assert!(msg.contains("Background task"), "启动回执格式: {}", msg);
    }
    assert_eq!(registry.active_count(), 10, "4 占位 + 6 后台任务");

    // 全部进入 LLM 调用（6 个并发运行中）
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while llm_calls.load(Ordering::SeqCst) < 6 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("6 个后台任务都应进入运行");

    // 取消 3 个：取消链走完整收尾（Stopped + 失败完成通知），各自从 registry 移除
    for (id, _, _) in registry.list_tasks() {
        if id.starts_with("bg-") && task_ids.len() < 3 {
            task_ids.push(id);
        }
    }
    assert_eq!(task_ids.len(), 3, "应有 3 个可取消的运行中任务");
    let mut started = 0usize;
    let mut stopped = 0usize;
    let mut cancelled_completed = 0usize;
    for id in &task_ids {
        registry.cancel(id).unwrap();
    }
    assert_eq!(
        registry.active_count(),
        7,
        "取消 3 个后仅剩 4 占位 + 3 运行中"
    );
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while stopped < 3 || cancelled_completed < 3 {
            match bg_rx.recv().await {
                Some(ExecutorEvent::SubagentStarted { .. }) => started += 1,
                Some(ExecutorEvent::SubagentStopped { .. }) => stopped += 1,
                Some(ExecutorEvent::BackgroundTaskCompleted(res)) => {
                    assert!(!res.success, "取消的任务应以失败结果收尾");
                    cancelled_completed += 1;
                }
                Some(_) => {}
                None => break,
            }
        }
    })
    .await
    .expect("取消路径必须 emit SubagentStopped + 失败完成通知（通知不丢失）");

    // 放行剩余 3 个：完成通知与清理必须完整（无任务泄漏）
    release.add_permits(6);
    let mut succeeded = 0usize;
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while succeeded < 3 {
            match bg_rx.recv().await {
                Some(ExecutorEvent::SubagentStarted { .. }) => started += 1,
                Some(ExecutorEvent::SubagentStopped { .. }) => stopped += 1,
                Some(ExecutorEvent::BackgroundTaskCompleted(res)) => {
                    assert!(res.success, "放行后的任务应以成功结果收尾");
                    succeeded += 1;
                }
                Some(_) => {}
                None => break,
            }
        }
    })
    .await
    .expect("放行后 3 个任务必须完成并发出通知");

    assert_eq!(started, 6, "6 个后台任务各 emit 一次 SubagentStarted");
    assert_eq!(stopped, 6, "6 个后台任务各 emit 一次 SubagentStopped");
    // 事件到达 ≠ 状态已清理：生产顺序是 SubagentStopped → BackgroundTaskCompleted
    // → registry.complete()（移除条目）→ cleanup guard 的 deregister。因此收到
    // 完成通知后必须等清理收敛，才能断言 registry 状态。
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while deregistered.lock().unwrap().len() < 6 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("6 个任务收尾都应 deregister active_agents");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while registry.active_count() != 4 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("全部收尾后 registry 应收敛回 4 个占位任务");
    assert_eq!(registry.active_count(), 4, "全部收尾后仅剩 4 占位任务");
    assert!(
        registry
            .list_tasks()
            .iter()
            .all(|(id, _, _)| id.starts_with("placeholder-")),
        "占位条目不得被改写，运行中任务不得残留"
    );
}
