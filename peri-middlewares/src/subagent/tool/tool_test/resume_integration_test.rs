use super::*;

// ─── Slice 7:集成测试(中断 → 恢复 → 完成 / 跨实例 / 多次恢复 / 事件配对) ─────

/// 前 `interrupt_rounds` 次 LLM 调用返回 `AgentError::Interrupted`（模拟中断），
/// 之后回显最后一条消息（模拟正常完成）。共享计数跨 tool 实例 / 跨恢复生效
/// ——每次 subagent 执行都会经 llm_factory 创建新实例，计数保持连续。
struct InterruptThenEchoLLM {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    interrupt_rounds: usize,
}

#[async_trait::async_trait]
impl ReactLLM for InterruptThenEchoLLM {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> peri_agent::error::AgentResult<Reasoning> {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < self.interrupt_rounds {
            return Err(peri_agent::error::AgentError::Interrupted);
        }
        let last = messages.last().map(|m| m.content()).unwrap_or_default();
        Ok(Reasoning::with_answer("", format!("echo: {}", last)))
    }
}

/// 构造带「前 N 次 Interrupted、之后回显」LLM 的 SubAgentTool（无 bridge/parent）
fn make_interrupt_tool(
    calls: Arc<std::sync::atomic::AtomicUsize>,
    interrupt_rounds: usize,
) -> SubAgentTool {
    SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(InterruptThenEchoLLM {
                calls: Arc::clone(&calls),
                interrupt_rounds,
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
}

/// 从工具返回/错误文本提取 child_thread_id
fn extract_child_thread_id(text: &str) -> String {
    text.split("child_thread_id: ")
        .nth(1)
        .and_then(|s| s.lines().next())
        .expect("文本应包含 child_thread_id")
        .to_string()
}

/// 统计 transcript 中 SkillTool 工具调用消息数（SkillPreload 注入的
/// Ai[ToolUse{SkillTool}] block 计数；R-H1 断言用）
fn count_skilltool_calls(msgs: &[BaseMessage]) -> usize {
    msgs.iter()
        .flat_map(|m| m.content_blocks())
        .filter(|b| {
            matches!(
                b,
                peri_agent::messages::ContentBlock::ToolUse { name, .. }
                    if name.as_str() == "SkillTool"
            )
        })
        .count()
}

/// 轮询等待 bridge 收到至少 n 对 Start/Stop（forwarder 异步消费，
/// 内容事件可能先到，不能只按数量等待）
async fn wait_for_observe_pairs(
    bridge: &Arc<RecordingBridge>,
    n: usize,
    timeout_ms: u64,
) -> Vec<ObserveEvent> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        let evs = bridge.observes.lock().unwrap().clone();
        let starts = evs
            .iter()
            .filter(|e| matches!(e, ObserveEvent::SubagentStart { .. }))
            .count();
        let stops = evs
            .iter()
            .filter(|e| matches!(e, ObserveEvent::SubagentStop { .. }))
            .count();
        if starts >= n && stops >= n {
            return evs;
        }
        if std::time::Instant::now() > deadline {
            panic!(
                "等待 {} 对 SubagentStart/Stop 超时（{}ms）：当前事件：{:?}",
                n, timeout_ms, evs
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// 轮询等待 transcript 异步 writer 落盘（transcript.rs 批量窗口 ≤100ms；
/// 执行返回后新消息可能仍在 writer 通道中）
async fn wait_for_messages(
    store: &Arc<peri_agent::thread::FilesystemThreadStore>,
    id: &str,
    n: usize,
    timeout_ms: u64,
) -> Vec<BaseMessage> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        let msgs = store.load_messages(&id.to_string()).await.unwrap();
        if msgs.len() >= n {
            return msgs;
        }
        if std::time::Instant::now() > deadline {
            panic!(
                "等待 transcript 落盘超时（{}ms）：期望 >= {} 条，实际 {} 条",
                timeout_ms,
                n,
                msgs.len()
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// 核心链路：agent-def 路径调用 Agent 工具 → LLM 返回 Interrupted（错误文本
/// 带 child_thread_id 前缀，主 agent 凭此恢复）→ 新 tool 实例（同一 dir /
/// 同 store / 同父 session thread_id，模拟主 agent 下一 turn 或进程重启）
/// 调用 resume_thread_id → 完成文本含结果。
/// R-L3：进程重启后凭 child_thread_id 恢复（thread_id 即凭证，父链仅作落盘记录）。
#[tokio::test]
async fn test_resume_interrupted_then_resumed_across_instances() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("interrupt-agent.md"),
        "---\nname: interrupt-agent\ndescription: Interrupt test agent\n---\n\nYou get interrupted.\n",
    )
    .unwrap();

    let store = make_fs_store(&dir);
    // 主 agent 会话 thread_id 固定（进程重启后 session_id 不变，R-L3）
    let work = dir.path().to_str().unwrap();
    let parent = peri_agent::session::Session::new(
        Arc::from(work),
        peri_agent::session::FrozenContext::builder().build(),
        Some("parent-uuid".into()),
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    // 实例 A：spawn → LLM 首轮 Interrupted → Err 文本带 child_thread_id 前缀
    let t_a = make_interrupt_tool(Arc::clone(&calls), 1)
        .with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>)
        .with_parent_session(parent.clone());
    let err = t_a
        .invoke(
            serde_json::json!({
                "subagent_type": "interrupt-agent",
                "cwd": dir.path().to_str().unwrap(),
                "prompt": "first task"
            }),
            peri_agent::tools::ToolContext::new(&[], work),
        )
        .await
        .expect_err("首次执行应返回（Interrupted 错误）");
    let err = err.to_string();
    assert!(
        err.contains("execution failed") && err.contains("child_thread_id:"),
        "LLM 返回 Interrupted → 错误文本须带 child_thread_id 前缀（可恢复）: {}",
        err
    );
    let id = extract_child_thread_id(&err);

    // 实例 B（同 store dir、同父 session thread_id）：resume → 完成
    let t_b = make_interrupt_tool(Arc::clone(&calls), 1)
        .with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>)
        .with_parent_session(parent);
    let result = t_b
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "cwd": work,
            }),
            peri_agent::tools::ToolContext::new(&[], work),
        )
        .await
        .expect("resume 应成功完成（thread_id 即恢复凭证）");
    assert!(
        result.contains(&format!("child_thread_id: {}", id)),
        "完成文本应带 child_thread_id: {}",
        result
    );
    assert!(
        result.contains("echo:"),
        "完成文本应含执行结果（隐式 continue 后的回显）: {}",
        result
    );

    // R-L3：子线程父链 = 父 session thread_id（跨实例复用，仅作父子链落盘记录）
    let meta = store.load_meta(&id).await.unwrap();
    assert_eq!(
        meta.parent_thread_id.as_deref(),
        Some("parent-uuid"),
        "子线程父链必须指向父 session thread_id（跨实例复用）"
    );
}

/// 跨实例重载（进程重启）：实例 A spawn 并中断（LLM 首轮返回 Interrupted →
/// 错误文本，带 child_thread_id 前缀）→ 丢弃实例 A → 新建实例 B（同
/// FilesystemThreadStore dir）resume → 完成；断言 transcript 重放正确
/// （消息数/顺序：spawn prompt → 隐式 continue → 新 AI）。
#[tokio::test]
async fn test_resume_across_instances_replays_transcript_in_order() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let store = make_fs_store(&dir);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let id = {
        // 实例 A：spawn → LLM 首轮 Interrupted（thread 与 transcript 已落盘）
        let t_a = make_interrupt_tool(Arc::clone(&calls), 1)
            .with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>);
        let err = t_a
            .invoke(
                serde_json::json!({
                    "subagent_type": "test-agent",
                    "cwd": dir.path().to_str().unwrap(),
                    "prompt": "first task"
                }),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await
            .expect_err("首次执行应返回（Interrupted 错误）");
        let err = err.to_string();
        assert!(
            err.contains("execution failed") && err.contains("child_thread_id:"),
            "LLM 返回 Interrupted → 错误文本须带 child_thread_id 前缀: {}",
            err
        );
        extract_child_thread_id(&err)
    }; // 丢弃实例 A（模拟进程重启，仅剩磁盘现场）

    // 实例 B：同 store dir → resume（缺省 prompt → 隐式 continue）→ 完成
    let t_b = make_interrupt_tool(Arc::clone(&calls), 1)
        .with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>);
    let result = t_b
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "cwd": dir.path().to_str().unwrap(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains(&format!("child_thread_id: {}", id)),
        "完成文本应带 child_thread_id: {}",
        result
    );
    assert!(result.contains("echo:"), "resume 应完成: {}", result);

    // transcript 重放：旧消息（spawn prompt）→ 隐式 continue → 新 AI（顺序不变）
    let msgs = wait_for_messages(&store, &id, 3, 3000).await;
    let contents: Vec<String> = msgs.iter().map(|m| m.content()).collect();
    assert_eq!(
        contents,
        vec![
            "first task",
            "Continue your previous task where you left off.",
            "echo: Continue your previous task where you left off.",
        ],
        "transcript 必须按 旧消息 → continue → 新 AI 顺序重放"
    );
}

/// 多次恢复：中断 → 恢复 → 再中断 → 再恢复（cancel 前置 → Ok 中断文本，
/// 含 `resume with Agent(resume_thread_id:)` 提示）；断言 thread_id 不变、
/// 最终完成、磁盘 status 收尾 done。
#[tokio::test]
async fn test_resume_multiple_times_keeps_thread_id_and_completes() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let store = make_fs_store(&dir);
    let cwd = dir.path().to_str().unwrap().to_string();

    // 构造带「已取消 cancel token」的实例（parent 缺席时注入的 cancel 生效 →
    // run_react_loop 返回 LoopResult::Interrupted → Ok 中断文本）
    let mk_cancelled = || {
        let cancel = AgentCancellationToken::new();
        cancel.cancel();
        make_subagent_tool(vec![])
            .with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>)
            .with_cancel(cancel)
    };

    // 1) spawn → 中断 #1（文本含 child_thread_id + resume 提示）
    let t1 = mk_cancelled();
    let r1 = t1
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "cwd": cwd.clone(),
                "prompt": "task"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("第一次应返回（中断文本）");
    assert!(
        r1.contains("was interrupted") && r1.contains("resume with Agent(resume_thread_id:"),
        "第一次应中断且带 resume 提示: {}",
        r1
    );
    let id1 = extract_child_thread_id(&r1);

    // 2) resume → 中断 #2（同一 thread_id）
    let t2 = mk_cancelled();
    let r2 = t2
        .invoke(
            serde_json::json!({
                "resume_thread_id": id1.clone(),
                "cwd": cwd.clone(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("第二次应返回（中断文本）");
    assert!(
        r2.contains("was interrupted") && r2.contains("resume with Agent(resume_thread_id:"),
        "第二次应再次中断且带 resume 提示: {}",
        r2
    );
    let id2 = extract_child_thread_id(&r2);
    assert_eq!(id1, id2, "多次恢复 thread_id 必须不变");

    // 3) resume → 完成（无 cancel 的新实例）
    let t3 =
        make_subagent_tool(vec![]).with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>);
    let r3 = t3
        .invoke(
            serde_json::json!({
                "resume_thread_id": id1.clone(),
                "cwd": cwd,
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        r3.contains(&format!("child_thread_id: {}", id1)),
        "完成文本应带 child_thread_id: {}",
        r3
    );
    assert!(r3.contains("echo:"), "最终应完成: {}", r3);

    let meta = store.load_meta(&id1).await.unwrap();
    assert_eq!(
        meta.agent_status,
        peri_agent::thread::AgentStatus::Done,
        "多次恢复后最终 status 应为 done"
    );
}

/// Start/Stop 配对：每次执行（首次 + 每次恢复）触发新 Start/Stop 配对，
/// 配对顺序正确（Start→Stop→Start→Stop）、child_agent_id 恒同（同一 thread）。
#[tokio::test]
async fn test_resume_emits_new_start_stop_pair_per_execution() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let store = make_fs_store(&dir);
    let bridge = Arc::new(RecordingBridge {
        observes: Arc::new(std::sync::Mutex::new(Vec::new())),
    });
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls_clone = Arc::clone(&calls);
    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(InterruptThenEchoLLM {
                calls: Arc::clone(&calls_clone),
                interrupt_rounds: 1,
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
    .with_parent_agent_id(Arc::new(RwLock::new(Some(AgentId::new()))))
    .with_langfuse_bridge(Arc::clone(&bridge) as Arc<dyn peri_agent::agent::LangfuseBridgeLike>)
    .with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>);
    let cwd = dir.path().to_str().unwrap().to_string();

    // 首次执行 → 中断（第 1 对 Start/Stop；LLM 返回 Interrupted → Err）
    let err = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "cwd": cwd.clone(),
                "prompt": "task"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect_err("首次执行应返回（Interrupted 错误）");
    let err = err.to_string();
    assert!(
        err.contains("execution failed") && err.contains("child_thread_id:"),
        "首次应中断: {}",
        err
    );
    let id = extract_child_thread_id(&err);
    let evs = wait_for_observe_pairs(&bridge, 1, 3000).await;
    assert_start_stop_pair(&evs, "test-agent", false);

    // 恢复 → 完成（第 2 对 Start/Stop）
    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "cwd": cwd,
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("echo:"), "resume 应完成: {}", result);
    let evs = wait_for_observe_pairs(&bridge, 2, 5000).await;

    // 恰好 2 对；事件顺序 Start→Stop→Start→Stop；child_agent_id 恒同
    let starts: Vec<&ObserveEvent> = evs
        .iter()
        .filter(|e| matches!(e, ObserveEvent::SubagentStart { .. }))
        .collect();
    let stops: Vec<&ObserveEvent> = evs
        .iter()
        .filter(|e| matches!(e, ObserveEvent::SubagentStop { .. }))
        .collect();
    assert_eq!(starts.len(), 2, "每次执行必须恰好一个 Start: {:?}", evs);
    assert_eq!(stops.len(), 2, "每次执行必须恰好一个 Stop: {:?}", evs);
    let seq: Vec<&str> = evs
        .iter()
        .filter_map(|e| match e {
            ObserveEvent::SubagentStart { .. } => Some("S"),
            ObserveEvent::SubagentStop { .. } => Some("T"),
            _ => None,
        })
        .collect();
    assert_eq!(
        seq,
        vec!["S", "T", "S", "T"],
        "配对顺序必须为 Start→Stop→Start→Stop"
    );

    let child_of = |e: &ObserveEvent| match e {
        ObserveEvent::SubagentStart { child_agent_id, .. } => *child_agent_id,
        ObserveEvent::SubagentStop { child_agent_id, .. } => *child_agent_id,
        _ => unreachable!(),
    };
    let first_child = child_of(starts[0]);
    assert_eq!(child_of(stops[0]), first_child, "第 1 对 child 必须配对");
    assert_eq!(
        child_of(starts[1]),
        first_child,
        "恢复必须复用同一 child_agent_id"
    );
    assert_eq!(child_of(stops[1]), first_child, "第 2 对 child 必须配对");
    assert_eq!(
        first_child.to_string(),
        id,
        "child_agent_id 必须等于 child_thread_id（身份统一）"
    );
}

/// R-H1：agent-def 声明 skills 非空 → 首次执行注入一套 SkillTool 对 → 中断 →
/// resume（恢复路径 skill_names 恒空 → 不重复注入）→ transcript 中 SkillTool
/// 工具调用消息恒为一套。
#[tokio::test]
async fn test_resume_skill_preload_not_duplicated() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    let skills_dir = dir.path().join(".claude").join("skills").join("test-skill");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
        agents_dir.join("skill-user.md"),
        "---\nname: skill-user\ndescription: Uses skills\nskills:\n  - test-skill\n---\n\nYou use skills.\n",
    )
    .unwrap();
    std::fs::write(
        skills_dir.join("SKILL.md"),
        "---\nname: 'test-skill'\ndescription: 'A test skill'\n---\n\n# Test Skill\n\nThis is the test skill content.\n",
    )
    .unwrap();

    let store = make_fs_store(&dir);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let t = make_interrupt_tool(Arc::clone(&calls), 1)
        .with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>);
    let cwd = dir.path().to_str().unwrap().to_string();

    // 首次执行（agent-def 路径）→ SkillPreload 注入一套 → 中断（LLM 返回 Interrupted）
    let err = t
        .invoke(
            serde_json::json!({
                "subagent_type": "skill-user",
                "cwd": cwd.clone(),
                "prompt": "test task"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect_err("首次执行应返回（Interrupted 错误）");
    let err = err.to_string();
    assert!(
        err.contains("execution failed") && err.contains("child_thread_id:"),
        "首次应中断: {}",
        err
    );
    let id = extract_child_thread_id(&err);

    // resume（隐式 continue，无 /skill token → 自动检测分支不触发）→ 完成
    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "cwd": cwd,
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("echo:"), "resume 应完成: {}", result);

    // transcript 中 SkillTool 工具调用必须只有首轮注入的一套（R-H1）
    let msgs = wait_for_messages(&store, &id, 5, 3000).await;
    assert_eq!(
        count_skilltool_calls(&msgs),
        1,
        "resume 不得重复注入 agent 定义声明的 skill（transcript: {:?}）",
        msgs.iter().map(|m| m.content()).collect::<Vec<_>>()
    );
}

/// R2-MID-1：构造「已完成工具轮次（末条 = Tool 结果）后中断」的 thread →
/// resume → 已完成 Ai+Tool 对保留、无重复执行（LLM 输入不含重复工具结果；
/// transcript 仅追加 continue + 新 AI，已完成轮次原样保留）。
#[tokio::test]
async fn test_resume_keeps_completed_tool_round_no_duplicate_execution() {
    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let store = make_fs_store(&dir);
    let id = uuid::Uuid::now_v7().to_string();
    // 完整工具轮次：Ai[ToolUse] + Tool[result] 配对，末条 = Tool → 不 pop
    preset_resumable_thread(
        &store,
        &id,
        "test-agent",
        None,
        vec![
            BaseMessage::human("task"),
            BaseMessage::ai_from_blocks(vec![peri_agent::messages::ContentBlock::tool_use(
                "call-1",
                "Read",
                serde_json::json!({}),
            )]),
            BaseMessage::tool_result("call-1", "tool-result-1"),
        ],
    )
    .await;

    let seen = Arc::new(std::sync::Mutex::new(0usize));
    let seen_clone = Arc::clone(&seen);
    struct ToolRoundCheckLLM {
        seen: Arc<std::sync::Mutex<usize>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for ToolRoundCheckLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            let n = messages
                .iter()
                .filter(|m| m.content().contains("tool-result-1"))
                .count();
            *self.seen.lock().unwrap() = n;
            Ok(Reasoning::with_answer("", "round-preserved"))
        }
    }
    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>| {
            Box::new(ToolRoundCheckLLM {
                seen: Arc::clone(&seen_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
    .with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>);

    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "cwd": dir.path().to_str().unwrap(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("round-preserved"),
        "resume 应完成: {}",
        result
    );
    assert_eq!(
        *seen.lock().unwrap(),
        1,
        "已完成工具轮次的 Tool 结果不得重复出现在 LLM 输入"
    );

    // transcript：3 旧消息 + 隐式 continue + 新 AI = 5 条；Ai+Tool 对保留
    let msgs = wait_for_messages(&store, &id, 5, 3000).await;
    let contents: Vec<String> = msgs.iter().map(|m| m.content()).collect();
    assert_eq!(msgs.len(), 5, "3 旧 + continue + 新 AI: {:?}", contents);
    assert!(contents[0].contains("task"), "旧 Human 应保留");
    assert!(
        contents[2].contains("tool-result-1"),
        "旧 Tool 结果应保留（已完成轮次不重放不删除）"
    );
    assert!(
        contents[3].contains("Continue your previous task"),
        "隐式 continue 应追加"
    );
    assert!(
        contents[4].contains("round-preserved"),
        "新 AI 应追加（无重复执行）"
    );
}

/// R2-LOW-1（实际行为比计划假设更安全）：resume prompt 含 /skill-name token
/// 也**不会**再次注入——resume 链装配时 skill_names 恒空（R-H1），
/// build_subagent_middlewares 的挂载条件 `!skill_names.is_empty()` 使
/// SkillPreloadMiddleware 在恢复路径根本不注册，自动检测分支（主 Agent 路径）
/// 无从触发。锁定该行为：SkillTool 计数恒为 1（首轮声明注入的一套）。
#[tokio::test]
async fn test_resume_skill_token_in_prompt_reinjects_once() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    let skills_dir = dir.path().join(".claude").join("skills").join("test-skill");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
        agents_dir.join("skill-user.md"),
        "---\nname: skill-user\ndescription: Uses skills\nskills:\n  - test-skill\n---\n\nYou use skills.\n",
    )
    .unwrap();
    std::fs::write(
        skills_dir.join("SKILL.md"),
        "---\nname: 'test-skill'\ndescription: 'A test skill'\n---\n\n# Test Skill\n\nThis is the test skill content.\n",
    )
    .unwrap();

    let store = make_fs_store(&dir);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let t = make_interrupt_tool(Arc::clone(&calls), 1)
        .with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>);
    let cwd = dir.path().to_str().unwrap().to_string();

    // 首次执行（显式声明 skills）→ 注入一套 → 中断（LLM 返回 Interrupted）
    let err = t
        .invoke(
            serde_json::json!({
                "subagent_type": "skill-user",
                "cwd": cwd.clone(),
                "prompt": "test task"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect_err("首次执行应返回（Interrupted 错误）");
    let err = err.to_string();
    assert!(
        err.contains("execution failed") && err.contains("child_thread_id:"),
        "首次应中断: {}",
        err
    );
    let id = extract_child_thread_id(&err);

    // resume prompt 含 /test-skill token → 自动检测分支不存在（链未挂 SkillPreload）
    let result = t
        .invoke(
            serde_json::json!({
                "resume_thread_id": id.clone(),
                "cwd": cwd,
                "prompt": "/test-skill continue",
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("echo:"), "resume 应完成: {}", result);

    // 断言前等待全部落盘：3 旧 + Human(/test-skill continue) + 新 AI = 5 条
    let msgs = wait_for_messages(&store, &id, 5, 3000).await;
    assert_eq!(
        count_skilltool_calls(&msgs),
        1,
        "resume 链装配不挂 SkillPreloadMiddleware（R-H1）→ resume prompt 含 \
         /skill-name 也不重复注入（R2-LOW-1 窗口不存在，比计划假设更安全）"
    );
}
