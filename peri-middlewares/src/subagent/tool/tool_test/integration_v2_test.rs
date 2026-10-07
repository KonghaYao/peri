use super::*;

// ─── SubAgent v2 集成测试（4 场景）──────────────────────────────────────
//
// 对应 docs/refactor/pending-fix-plan-2026-07-06.md [INTEGRATION-TESTS]。
// 依赖 BUG-A / BUG-B / BUG-C 修复（已完成）。
// 场景 3（Sync Cascade cancel）已由 `test_cancel_token_interrupts_subagent`（:655）覆盖，
// 这里在文末的 markdown 报告中确认覆盖范围。
//
// 所有测试基于 v2 路径（`build_v2_subagent_context` / `SubAgentTool::invoke`），
// 不依赖 v1 `ReActAgent`。

/// 场景 1（Fork 父消息透传）：端到端验证 fork 模式下子 agent 收到完整父对话历史。
///
/// 断言（对应 plan.md §场景1 acceptance）：
/// - mock LLM 收到的 messages.len() >= 4（3 父 + 1 fork_directive prompt）
///   实际还会包含 system_builder 注入的 1 条 System 消息
/// - mock LLM 收到的最后一条消息是 `BaseMessage::Human` 且 content 包含 `<fork_directive>`（验证 BUG-A）
/// - mock LLM 收到的 messages 包含 `BaseMessage::System` 内容含 "FORK-CONTEXT-SP"（验证 BUG-B fork 路径）
/// - mock LLM 收到的 messages 包含全部 3 条父消息（按顺序透传，验证 BUG-C）
#[tokio::test]
async fn test_integration_fork_parent_messages_passthrough() {
    let host = DurableHost::open("fixture-iv2-passthrough").await;
    // Arrange: 3 条父消息（Human/AI 交替 + 1 条 system context）
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages
        .write()
        .push(BaseMessage::human("parent Q1"));
    parent_messages
        .write()
        .push(BaseMessage::ai("parent A1 with details"));
    parent_messages
        .write()
        .push(BaseMessage::human("parent Q2 followup"));

    // H1 生产装配捕获：模型来源经子链装配点装 bridge，捕获真实 ModelRequest。
    let model = super::mock_model::RecordingModel::new("fork integration done");

    let t = host.bind(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new({
                let model = Arc::clone(&model);
                move |_: Option<&str>| {
                    SubagentLlmSource::model(
                        model.clone() as Arc<dyn peri_model::Model>,
                        "mock-model",
                    )
                }
            }),
            "/tmp".to_string(),
        )
        .with_parent_messages(Arc::clone(&parent_messages))
        .with_system_builder(Arc::new(|_ov, _cwd| "FORK-CONTEXT-SP".to_string())),
    );

    // Act: fork 模式触发端到端路径
    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "continue from parent context"
            }),
            host.context(&[]),
        )
        .await
        .unwrap();

    // Assert 0: 执行成功（v2 loop 正常完成）
    assert!(
        result.contains("fork integration done"),
        "fork should complete via v2 path: {}",
        result
    );

    let messages = model.last_messages();
    let texts = super::mock_model::conversation_texts(&messages);

    // Assert 1: 请求消息 >= 4（3 父 + 1 system + 1 fork_directive prompt）
    assert!(
        messages.len() >= 4,
        "fork should receive parent messages + system + directive (got {})",
        messages.len()
    );

    // Assert 2: 最后一条是 User，且包含 <fork_directive>（BUG-A）
    let last = texts.last().expect("messages non-empty");
    assert!(
        last.contains("<fork_directive>"),
        "last message should contain <fork_directive> (BUG-A), got: {}",
        last
    );
    assert!(
        last.contains("continue from parent context"),
        "fork directive should wrap original prompt"
    );

    // Assert 3: 身份经 bridge base system 注入（恰一次，不再写 transcript）
    let system = model.last_system();
    assert!(
        system.contains("FORK-CONTEXT-SP"),
        "bridge system should contain system_builder projection, got: {system}"
    );
    // 身份恰一次：只出现在 bridge base system；非 system 消息不得再出现
    // （`conversation_texts` 以 "[system]" 前缀标记 system 条目）。
    assert_eq!(
        texts
            .iter()
            .filter(|t| !t.starts_with("[system]"))
            .filter(|t| t.contains("FORK-CONTEXT-SP"))
            .count(),
        0,
        "identity must not be duplicated into conversation messages: {texts:?}"
    );

    // Assert 4: 父消息按顺序透传（BUG-C）
    // 验证三条父消息的 content 都在请求消息中
    let contents: Vec<String> = texts.clone();
    assert!(
        contents.iter().any(|c| c.contains("parent Q1")),
        "first parent message should pass through (BUG-C)"
    );
    assert!(
        contents
            .iter()
            .any(|c| c.contains("parent A1 with details")),
        "second parent message should pass through (BUG-C)"
    );
    assert!(
        contents.iter().any(|c| c.contains("parent Q2 followup")),
        "third parent message should pass through (BUG-C)"
    );
}

#[tokio::test]
async fn test_fork_prefers_tool_context_messages_over_parent_snapshot() {
    let host = DurableHost::open("fixture-iv2-prefer").await;
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages
        .write()
        .push(BaseMessage::human("old before_agent snapshot"));
    let ctx_messages = vec![
        BaseMessage::human("old before_agent snapshot"),
        BaseMessage::ai("new current turn detail"),
        BaseMessage::human("latest user request"),
    ];

    let captured: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);

    #[derive(Clone)]
    struct CaptureContentLLM {
        captured: Arc<std::sync::Mutex<Vec<String>>>,
    }
    impl CaptureContentLLM {
        async fn respond(
            &self,
            request: peri_model::ModelRequest,
            cancellation: tokio_util::sync::CancellationToken,
        ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
            use crate::subagent::test_support::*;
            let _ = &cancellation;
            let messages = base_messages(&request);
            let defined = defined_tools(&request);
            let tools: Vec<&dyn BaseTool> = defined.iter().map(|t| t as &dyn BaseTool).collect();

            *self.captured.lock().unwrap() = messages.iter().map(|m| m.content()).collect();
            text_events("ctx-preferred")
        }
    }
    crate::subagent::test_support::fixture_model_impl!(CaptureContentLLM);

    let t = host.bind(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(move |_: Option<&str>| {
                crate::subagent::test_support::fixture_source(
                    std::sync::Arc::new(CaptureContentLLM {
                        captured: Arc::clone(&captured_clone),
                    }),
                    "fixture-scripted",
                )
            }),
            "/tmp".to_string(),
        )
        .with_parent_messages(Arc::clone(&parent_messages)),
    );

    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "review current turn"
            }),
            host.context(&ctx_messages),
        )
        .await
        .unwrap();
    assert!(
        result.contains("ctx-preferred"),
        "fork should execute via context-preferred path: {}",
        result
    );
    let contents = captured.lock().unwrap().clone();
    assert!(
        contents
            .iter()
            .any(|c| c.contains("new current turn detail")),
        "fork should inherit current ToolContext messages, got: {:?}",
        contents
    );
    assert!(
        contents.iter().any(|c| c.contains("latest user request")),
        "fork should inherit latest ToolContext user request, got: {:?}",
        contents
    );
}

#[tokio::test]
async fn test_fork_falls_back_to_parent_messages_when_tool_context_empty() {
    let host = DurableHost::open("fixture-iv2-fallback").await;
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages
        .write()
        .push(BaseMessage::human("parent fallback context"));

    let captured: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);

    #[derive(Clone)]
    struct FallbackCaptureLLM {
        captured: Arc<std::sync::Mutex<Vec<String>>>,
    }
    impl FallbackCaptureLLM {
        async fn respond(
            &self,
            request: peri_model::ModelRequest,
            cancellation: tokio_util::sync::CancellationToken,
        ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
            use crate::subagent::test_support::*;
            let _ = &cancellation;
            let messages = base_messages(&request);
            let defined = defined_tools(&request);
            let tools: Vec<&dyn BaseTool> = defined.iter().map(|t| t as &dyn BaseTool).collect();

            *self.captured.lock().unwrap() = messages.iter().map(|m| m.content()).collect();
            text_events("fallback-used")
        }
    }
    crate::subagent::test_support::fixture_model_impl!(FallbackCaptureLLM);

    let t = host.bind(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(move |_: Option<&str>| {
                crate::subagent::test_support::fixture_source(
                    std::sync::Arc::new(FallbackCaptureLLM {
                        captured: Arc::clone(&captured_clone),
                    }),
                    "fixture-scripted",
                )
            }),
            "/tmp".to_string(),
        )
        .with_parent_messages(Arc::clone(&parent_messages)),
    );

    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "review fallback"
            }),
            host.context(&[]),
        )
        .await
        .unwrap();
    assert!(
        result.contains("fallback-used"),
        "fork should execute via fallback path: {}",
        result
    );
    let contents = captured.lock().unwrap().clone();
    assert!(
        contents
            .iter()
            .any(|c| c.contains("parent fallback context")),
        "fork should fall back to parent_messages when ToolContext is empty, got: {:?}",
        contents
    );
}

#[tokio::test]
async fn test_fork_drops_trailing_tool_call_message_from_tool_context() {
    let host = DurableHost::open("fixture-iv2-drop").await;
    let ctx_messages = vec![
        BaseMessage::human("stable context before tool call"),
        BaseMessage::ai_with_tool_calls(
            "unfinished agent tool call text",
            vec![peri_agent::messages::ToolCallRequest::new(
                "call-agent-1",
                "Agent",
                serde_json::json!({"fork": true, "prompt": "review"}),
            )],
        ),
    ];

    let captured: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);

    #[derive(Clone)]
    struct DropToolCallCaptureLLM {
        captured: Arc<std::sync::Mutex<Vec<String>>>,
    }
    impl DropToolCallCaptureLLM {
        async fn respond(
            &self,
            request: peri_model::ModelRequest,
            cancellation: tokio_util::sync::CancellationToken,
        ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
            use crate::subagent::test_support::*;
            let _ = &cancellation;
            let messages = base_messages(&request);
            let defined = defined_tools(&request);
            let tools: Vec<&dyn BaseTool> = defined.iter().map(|t| t as &dyn BaseTool).collect();

            *self.captured.lock().unwrap() = messages.iter().map(|m| m.content()).collect();
            text_events("tool-call-dropped")
        }
    }
    crate::subagent::test_support::fixture_model_impl!(DropToolCallCaptureLLM);

    let t = host.bind(SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>| {
            crate::subagent::test_support::fixture_source(
                std::sync::Arc::new(DropToolCallCaptureLLM {
                    captured: Arc::clone(&captured_clone),
                }),
                "fixture-scripted",
            )
        }),
        "/tmp".to_string(),
    ));

    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "review without dangling tool call"
            }),
            host.context(&ctx_messages),
        )
        .await
        .unwrap();
    assert!(
        result.contains("tool-call-dropped"),
        "fork should execute after dropping trailing tool call message: {}",
        result
    );
    let contents = captured.lock().unwrap().clone();
    assert!(
        contents
            .iter()
            .any(|c| c.contains("stable context before tool call")),
        "fork should keep earlier context, got: {:?}",
        contents
    );
    assert!(
        contents
            .iter()
            .all(|c| !c.contains("unfinished agent tool call text")),
        "fork should drop trailing AI message with unclosed tool call, got: {:?}",
        contents
    );
}

/// 场景 2（Background Independent cancel）：端到端验证 background fork 在父 cancel 后**不**中断。
///
/// 基于 `SubAgentTool::invoke` → `invoke_background` → `invoke_background_fork`
/// → `spawn_background_fork` 完整链路（v2 `build_v2_subagent_context` 装配）。
///
/// 关键断言：
/// - 父 cancel_token.cancel() 后，background task 仍能完成（Independent policy）
/// - mock LLM 至少被调用 1 次（证明未被父 cancel 中断）
/// - bg_event_sender 接收到 BackgroundTaskCompleted（task 正常结束）
#[tokio::test]
async fn test_integration_background_independent_survives_parent_cancel() {
    use peri_agent::agent::events::ExecutorEvent;
    use tokio::sync::mpsc;

    // Arrange: 共享的 LLM 调用计数（mock 会阻塞等待，但 cancel 不影响它）
    let llm_call_count: Arc<std::sync::atomic::AtomicUsize> =
        Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let llm_call_count_clone = Arc::clone(&llm_call_count);

    #[derive(Clone)]
    struct CountingLLM {
        count: Arc<std::sync::atomic::AtomicUsize>,
        release: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
    }
    impl CountingLLM {
        async fn respond(
            &self,
            _request: peri_model::ModelRequest,
            _cancellation: tokio_util::sync::CancellationToken,
        ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
            use crate::subagent::test_support::*;
            let receiver = self.release.lock().unwrap().take();
            if let Some(receiver) = receiver {
                let _ = receiver.await;
            }
            self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            text_events("bg done independent")
        }
    }
    crate::subagent::test_support::fixture_model_impl!(CountingLLM);

    // 门控：任务必须保持 active 直到测试断言（父 cancel 之后），再放行完成。
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let release = Arc::new(std::sync::Mutex::new(Some(release_rx)));

    // 父 cancel token（Independent policy 下不应传播到 background task）
    let parent_cancel = AgentCancellationToken::new();

    // bg_event_sender 通道，捕获完成事件
    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<ExecutorEvent>();

    // Background registry
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());

    // 父消息（fork background 需要）
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages
        .write()
        .push(BaseMessage::human("parent ctx"));

    let host = DurableHost::open_with_background(
        "fixture-iv2-independent",
        Arc::clone(&registry),
        bg_tx.clone(),
    )
    .await;
    let t = host.bind(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(move |_: Option<&str>| {
                crate::subagent::test_support::fixture_source(
                    std::sync::Arc::new(CountingLLM {
                        count: Arc::clone(&llm_call_count_clone),
                        release: Arc::clone(&release),
                    }),
                    "fixture-scripted",
                )
            }),
            "/tmp".to_string(),
        )
        .with_parent_messages(parent_messages)
        .with_cancel(parent_cancel.clone())
        .with_task_manager(Arc::clone(&registry))
        .with_bg_event_sender(bg_tx),
    );

    // Act 1: 启动 background fork
    let invoke_result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "run_in_background": true,
                "prompt": "long running bg task"
            }),
            host.context(&[]),
        )
        .await;
    assert!(
        invoke_result.is_ok(),
        "background fork should start: {:?}",
        invoke_result.err()
    );
    let invoke_msg = invoke_result.unwrap();
    assert!(
        invoke_msg.contains("Background task"),
        "should return background task started message: {}",
        invoke_msg
    );

    // Act 2: 父 cancel（不应影响 independent background task）
    // 注意：Independent policy = background task 使用独立 CancellationToken（spawner.rs:177），
    // 不与 parent_cancel 形成 child_token 关系，所以 cancel 不会传播。
    parent_cancel.cancel();
    host.cancel_parent();

    // Assert 1: background task 仍在运行（active_count >= 1，未被父 cancel 移除）
    assert!(
        registry.active_count() >= 1,
        "independent background task should survive parent cancel"
    );
    // 放行门控：任务在父 cancel 之后仍继续执行到完成。
    let _ = release_tx.send(());

    // Act 3: 等待 background task 完整执行（消耗所有事件直到 BackgroundTaskCompleted）
    // Independent policy 下 task 应运行到完成，不应被 cancel 中断。
    let mut got_started = false;
    let mut got_stopped = false;
    let mut got_completed = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, bg_rx.recv()).await {
            Ok(Some(ev)) => match ev {
                ExecutorEvent::SubagentStarted { is_background, .. } => {
                    got_started = true;
                    assert!(
                        is_background,
                        "SubagentStarted should have is_background=true"
                    );
                }
                ExecutorEvent::SubagentStopped { .. } => {
                    got_stopped = true;
                }
                ExecutorEvent::BackgroundTaskCompleted(ref res) => {
                    assert!(
                        res.success,
                        "background task result should be success (independent of parent cancel)"
                    );
                    assert!(
                        res.output.contains("bg done independent"),
                        "background output should match mock LLM answer: {}",
                        res.output
                    );
                    got_completed = true;
                    break;
                }
                _ => {}
            },
            Ok(None) => break, // channel closed
            Err(_) => break,   // timeout
        }
    }

    // Assert 2: 接收到完整事件序列（Started → Stopped → Completed）
    assert!(
        got_started,
        "should receive SubagentStarted event from bg pump"
    );
    assert!(
        got_stopped,
        "should receive SubagentStopped event from bg pump"
    );
    assert!(
        got_completed,
        "should receive BackgroundTaskCompleted within timeout (independent of parent cancel)"
    );

    // Assert 3: LLM 被调用过（>=1 次），证明执行未被 cancel 中断
    let call_count = llm_call_count.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        call_count >= 1,
        "mock LLM should be called at least once (parent cancel should not interrupt independent task), got {}",
        call_count
    );
}

/// 场景 3（Sync Cascade cancel）—— 补充断言。
///
/// 已有测试 `test_cancel_token_interrupts_subagent`（:655）验证了"父 cancel 在 SubAgent 执行前触发"
/// 的场景，但它的断言仅检查返回值 `contains("interrupted")`。这里补充验证：
/// - cancel 在执行前触发 → run_react_loop 返回 LoopResult::Interrupted
/// - 返回的 "interrupted" 字符串（通过 execute_fork.rs:236 / define.rs:584 的 output_summary）
/// - 不会调用任何工具/LLM 多次（避免 cancel 后的 zombie 执行）
///
/// 原测试 :655 已覆盖核心断言，本测试作为补充，验证 cancel 的"前置触发"边界条件
/// 与"返回值规范化"约定。
#[tokio::test]
async fn test_integration_sync_cascade_cancel_returns_interrupted_marker() {
    let dir = tempdir().unwrap();
    let host = DurableHost::open_in(dir.path(), "fixture-iv2-cascade").await;
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("cancellable.md"),
        "---\nname: cancellable\ndescription: Can be cancelled\n---\n\nYou are cancellable.\n",
    )
    .unwrap();

    // LLM 永远尝试调用不存在的工具，模拟"无限循环"——但 cancel 在执行前触发
    let llm_call_count: Arc<std::sync::atomic::AtomicUsize> =
        Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let llm_call_count_clone = Arc::clone(&llm_call_count);

    #[derive(Clone)]
    struct LoopingLLM {
        count: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl LoopingLLM {
        async fn respond(
            &self,
            request: peri_model::ModelRequest,
            cancellation: tokio_util::sync::CancellationToken,
        ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
            use crate::subagent::test_support::*;
            let _ = &cancellation;
            let messages = base_messages(&request);
            let defined = defined_tools(&request);
            let tools: Vec<&dyn BaseTool> = defined.iter().map(|t| t as &dyn BaseTool).collect();

            self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tool_events_from_react(vec![peri_agent::agent::react::ToolCall::new(
                "id1",
                "nonexistent",
                serde_json::json!({}),
            )])
        }
    }
    crate::subagent::test_support::fixture_model_impl!(LoopingLLM);

    let cancel = AgentCancellationToken::new();
    // 关键：在 SubAgent 执行**之前** cancel（模拟父 Agent 收到 Ctrl+C 后才 spawn SubAgent）
    cancel.cancel();
    host.cancel_parent();

    let t = host.bind(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(move |_: Option<&str>| {
                crate::subagent::test_support::fixture_source(
                    std::sync::Arc::new(LoopingLLM {
                        count: Arc::clone(&llm_call_count_clone),
                    }),
                    "fixture-scripted",
                )
            }),
            dir.path().to_str().unwrap().to_string(),
        )
        .with_cancel(cancel),
    );
    let t = with_agent_face(t, dir.path()).await;

    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "cancellable",
                "prompt": "run",
                "cwd": dir.path().to_str().unwrap()
            }),
            host.context(&[]),
        )
        .await
        .unwrap();

    // Assert 1: 返回值包含 interrupted 标记（Cascade cancel 传播成功）
    assert!(
        result.contains("interrupted"),
        "Cascade cancel should produce 'interrupted' marker, got: {}",
        result
    );

    // Assert 2: cancel 在 loop 入口前阻断 LLM。
    let final_count = llm_call_count.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        final_count, 0,
        "pre-cancelled Cascade child must not call the LLM (got {} calls)",
        final_count
    );
}

/// P0-2：background non-fork 必须通过实际 `invoke_background` 路径，由 loop 在
/// Receive 后唯一执行 before_agent。测试使用 registry 和 bg event sender，不轮询或 sleep。
#[tokio::test]
async fn test_p0_2_background_defined_skill_preload_once_after_parent_cancel() {
    use peri_agent::agent::events::ExecutorEvent;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc;

    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    let skills_dir = dir.path().join(".claude").join("skills").join("p0-2-skill");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
        agents_dir.join("p0-2-bg.md"),
        "---\nname: p0-2-bg\ndescription: P0-2 background agent\nskills:\n  - p0-2-skill\n---\n\nRun the task.\n",
    )
    .unwrap();
    // 磁盘文件是**反例**（W4b：子链预载只查 MCP registry，不读盘）。
    std::fs::write(
        skills_dir.join("SKILL.md"),
        "---\nname: p0-2-skill\ndescription: P0-2 skill\n---\n\nDISK CONTENT MUST NOT BE READ\n",
    )
    .unwrap();

    let llm_calls = Arc::new(AtomicUsize::new(0));
    let preload_count = Arc::new(std::sync::Mutex::new(0));
    let llm_calls_clone = Arc::clone(&llm_calls);
    let preload_count_clone = Arc::clone(&preload_count);
    #[derive(Clone)]
    struct BackgroundSkillLLM {
        calls: Arc<AtomicUsize>,
        preload_count: Arc<std::sync::Mutex<usize>>,
    }
    impl BackgroundSkillLLM {
        async fn respond(
            &self,
            request: peri_model::ModelRequest,
            cancellation: tokio_util::sync::CancellationToken,
        ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
            use crate::subagent::test_support::*;
            let _ = &cancellation;
            let messages = base_messages(&request);
            let defined = defined_tools(&request);
            let tools: Vec<&dyn BaseTool> = defined.iter().map(|t| t as &dyn BaseTool).collect();

            self.calls.fetch_add(1, Ordering::SeqCst);
            assert!(
                messages
                    .iter()
                    .any(|message| message.content().contains("p0-2 background prompt")),
                "before_agent must observe the prompt only after Receive"
            );
            *self.preload_count.lock().unwrap() = messages
                .iter()
                .filter(|message| message.content().contains("P0-2 BACKGROUND SKILL MARKER"))
                .count();
            text_events("p0-2 background done")
        }
    }
    crate::subagent::test_support::fixture_model_impl!(BackgroundSkillLLM);

    let parent_cancel = AgentCancellationToken::new();
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<ExecutorEvent>();
    let host = DurableHost::open_in_with_background(
        dir.path(),
        "fixture-iv2-skillpreload",
        Arc::clone(&registry),
        bg_tx.clone(),
    )
    .await;
    let tool = host.bind(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(move |_: Option<&str>| {
                crate::subagent::test_support::fixture_source(
                    std::sync::Arc::new(BackgroundSkillLLM {
                        calls: Arc::clone(&llm_calls_clone),
                        preload_count: Arc::clone(&preload_count_clone),
                    }),
                    "fixture-scripted",
                )
            }),
            dir.path().to_str().unwrap().to_string(),
        )
        .with_cancel(parent_cancel.clone())
        .with_task_manager(registry)
        .with_bg_event_sender(bg_tx),
    );
    let tool = host.bind(with_agent_face(tool, dir.path()).await);
    let tool = super::with_skill_registry(
        tool,
        "workspace",
        &[("p0-2-skill", "P0-2 BACKGROUND SKILL MARKER\n")],
    );

    let started = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "p0-2-bg",
                "run_in_background": true,
                "prompt": "p0-2 background prompt",
                "cwd": dir.path().to_str().unwrap(),
            }),
            host.context(&[]),
        )
        .await
        .expect("background defined subagent should start");
    assert!(started.contains("Background task"));
    parent_cancel.cancel();
    host.cancel_parent();

    let mut lifecycle = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(event) = bg_rx.recv().await {
            match event {
                ExecutorEvent::SubagentStarted { is_background, .. } => {
                    assert!(is_background);
                    lifecycle.push("started");
                }
                ExecutorEvent::SubagentStopped { .. } => lifecycle.push("stopped"),
                ExecutorEvent::BackgroundTaskCompleted(result) => {
                    assert!(result.success);
                    assert!(result.output.contains("p0-2 background done"));
                    lifecycle.push("completed");
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("background defined subagent must complete within the bounded receiver timeout");

    assert_eq!(lifecycle, ["started", "stopped", "completed"]);
    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        *preload_count.lock().unwrap(),
        1,
        "explicit skill preload must occur exactly once on execute_bg.rs"
    );
}

/// 验证语义优先级（define.rs:399-403 的逻辑：background 优先 → 走 invoke_background_fork）。
///
/// 关键断言：
/// - 调用返回值包含 "Background task"（证明走了 background 路径）
/// - bg_event_sender 接收到 SubagentStarted（is_background=true）
/// - background registry 中注册了任务（task_id 前缀为 "bg-"）
/// - 捕获的 mock LLM prompt 包含 `<fork_directive>`（英文模板，BgForkDirectiveKind::Fork）
///   而非 `<bg_fork_directive>`（中文模板）——证明 directive kind 正确
#[tokio::test]
async fn test_integration_fork_plus_background_priority() {
    use peri_agent::agent::events::ExecutorEvent;
    use tokio::sync::mpsc;

    // Arrange: 捕获 LLM 收到的 prompt（用于验证 directive kind）
    let prompt_capture: Arc<std::sync::Mutex<String>> =
        Arc::new(std::sync::Mutex::new(String::new()));
    let prompt_capture_clone = Arc::clone(&prompt_capture);

    #[derive(Clone)]
    struct PromptCaptureLLM {
        captured: Arc<std::sync::Mutex<String>>,
    }
    impl PromptCaptureLLM {
        async fn respond(
            &self,
            request: peri_model::ModelRequest,
            cancellation: tokio_util::sync::CancellationToken,
        ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
            use crate::subagent::test_support::*;
            let _ = &cancellation;
            let messages = base_messages(&request);
            let defined = defined_tools(&request);
            let tools: Vec<&dyn BaseTool> = defined.iter().map(|t| t as &dyn BaseTool).collect();

            // 找到最后一条 Human 消息（fork directive 在 prompt queue 里）
            if let Some(last_human) = messages
                .iter()
                .rev()
                .find(|m| matches!(m, BaseMessage::Human { .. }))
            {
                *self.captured.lock().unwrap() = last_human.content();
            }
            text_events("bg-fork done")
        }
    }
    crate::subagent::test_support::fixture_model_impl!(PromptCaptureLLM);

    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<ExecutorEvent>();
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());

    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages
        .write()
        .push(BaseMessage::human("ctx for bg fork"));

    let host = DurableHost::open_with_background(
        "fixture-iv2-independent",
        Arc::clone(&registry),
        bg_tx.clone(),
    )
    .await;
    let t = host.bind(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(move |_: Option<&str>| {
                crate::subagent::test_support::fixture_source(
                    std::sync::Arc::new(PromptCaptureLLM {
                        captured: Arc::clone(&prompt_capture_clone),
                    }),
                    "fixture-scripted",
                )
            }),
            "/tmp".to_string(),
        )
        .with_parent_messages(parent_messages)
        .with_task_manager(Arc::clone(&registry))
        .with_bg_event_sender(bg_tx),
    );

    // Act: 同时 fork=true + run_in_background=true（优先级测试）
    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "run_in_background": true,
                "prompt": "do both"
            }),
            host.context(&[]),
        )
        .await
        .unwrap();

    // Assert 1: 走 background 路径（返回值包含 "Background task"）
    assert!(
        result.contains("Background task"),
        "fork+bg should prioritize background path: {}",
        result
    );

    // Assert 2: 从返回值中提取 task_id，验证前缀为 "bg-"
    // 格式: "Background task bg-{uuid} started..."
    let task_id = result
        .split("Background task ")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .expect("task_id should be parseable from result");
    assert!(
        task_id.starts_with("bg-"),
        "task_id should have 'bg-' prefix (background spawn), got: {}",
        task_id
    );

    // Assert 3: registry 中注册了任务（active_count >= 1）
    assert!(
        registry.active_count() >= 1,
        "background fork should be registered in BackgroundTaskRegistry"
    );

    // Assert 4: bg_event_sender 收到 SubagentStarted（is_background=true）
    let started_ev = tokio::time::timeout(std::time::Duration::from_secs(2), bg_rx.recv())
        .await
        .expect("should receive SubagentStarted within timeout")
        .expect("channel should not be closed");
    match started_ev {
        ExecutorEvent::SubagentStarted { is_background, .. } => {
            assert!(
                is_background,
                "SubagentStarted should have is_background=true for fork+bg"
            );
        }
        other => panic!("expected SubagentStarted first, got: {:?}", other),
    }

    // Assert 5: 等待 background task 完成，捕获 LLM 收到的 prompt
    // 验证 directive kind = Fork（英文 `<fork_directive>`，非中文 `<bg_fork_directive>`）
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            match bg_rx.recv().await {
                Some(ExecutorEvent::BackgroundTaskCompleted(_)) => break,
                Some(_) => continue,
                None => break,
            }
        }
    })
    .await;

    let captured_prompt = prompt_capture.lock().unwrap().clone();
    assert!(
        captured_prompt.contains("<fork_directive>"),
        "fork+bg should use Fork directive kind (English <fork_directive>), got: {}",
        captured_prompt
    );
    assert!(
        !captured_prompt.contains("<bg_fork_directive>"),
        "fork+bg should NOT use Bg directive kind (Chinese <bg_fork_directive>), got: {}",
        captured_prompt
    );
    assert!(
        captured_prompt.contains("do both"),
        "fork directive should wrap original prompt 'do both'"
    );
}
