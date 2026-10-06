//! before_react_start 闸门准入、失败与工具发布。
use super::super::*;
use crate::middleware::capabilities as hook_state;
use crate::session::queue::MessageSource;
use crate::session::store::FrozenContext;
use crate::session::Session;

#[derive(Debug, Default)]
struct LoopEventSummary {
    stage_lifecycle: Vec<(Stage, bool)>,
    llm_start_steps: Vec<usize>,
    llm_end_steps: Vec<usize>,
}

fn drain_loop_observe_events(
    handles: &mut crate::agent::events_v2::EventHandles,
) -> LoopEventSummary {
    let mut summary = LoopEventSummary::default();
    while let Some(event) = handles.try_observe() {
        match event {
            ObserveEvent::StageStarted { stage, .. } => {
                summary.stage_lifecycle.push((stage, false));
            }
            ObserveEvent::StageEnded { stage, status, .. } => {
                assert_eq!(status, StageStatus::Done, "阶段必须以 Done 成对结束");
                summary.stage_lifecycle.push((stage, true));
            }
            ObserveEvent::LlmCallStart { step, .. } => summary.llm_start_steps.push(step),
            ObserveEvent::LlmCallEnd { step, .. } => summary.llm_end_steps.push(step),
            _ => {}
        }
    }
    summary
}

fn expected_stage_lifecycle(stages: &[Stage]) -> Vec<(Stage, bool)> {
    stages
        .iter()
        .flat_map(|stage| [(*stage, false), (*stage, true)])
        .collect()
}

// ─── 启动闸门 hook（before_react_start）回归 ────────────────────────────────
//
// 契约 2：System MCP 等启动依赖未完成前不得进入可启动 react loop。闸门在首批
// before_agent 之后、Compact 之前调用一次；Err 终止本次 loop，Interrupted 仍按
// 中断分类；既有 before_agent 的软失败降级不变。

/// 静态 MCP bridge 测试桩（名称与生产 `mcp__{server}__{tool}` 同形）。
struct StartupGateStubTool {
    name: String,
    server: Option<String>,
    direct: bool,
}

#[async_trait::async_trait]
impl crate::tools::BaseTool for StartupGateStubTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.name
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    fn mcp_server_name(&self) -> Option<&str> {
        self.server.as_deref()
    }

    fn is_direct(&self) -> bool {
        self.direct
    }

    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(String::new())
    }
}

fn startup_gate_stub(
    name: &str,
    server: Option<&str>,
    direct: bool,
) -> Arc<dyn crate::tools::BaseTool> {
    Arc::new(StartupGateStubTool {
        name: name.to_string(),
        server: server.map(str::to_owned),
        direct,
    })
}

/// 本地目录 + working map：闸门提交的静态 MCP bridge 必须与既有条目共存。
fn startup_gate_catalog() -> (SharedToolMap, Arc<SessionToolCatalog>) {
    let local = startup_gate_stub("startup_gate_local_tool", None, false);
    let working: SharedToolMap = Arc::new(parking_lot::RwLock::new(BTreeMap::from([(
        "startup_gate_local_tool".to_string(),
        Arc::clone(&local),
    )])));
    let catalog = Arc::new(SessionToolCatalog::new(
        BTreeMap::from([("startup_gate_local_tool".to_string(), local)]),
        None,
    ));
    (working, catalog)
}

/// 本次准入的候选：静态 bridge + 必需工具身份。
fn startup_gate_candidate() -> crate::session::tool_catalog::StartupToolUpdate {
    crate::session::tool_catalog::StartupToolUpdate {
        tools: vec![startup_gate_stub(
            "mcp__system__lookup",
            Some("system"),
            true,
        )],
        required: vec![crate::session::tool_catalog::StartupRequiredTool {
            server_name: "system".to_string(),
            original_tool_name: "lookup".to_string(),
            effective_tool_name: "mcp__system__lookup".to_string(),
        }],
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StartupGateOutcome {
    /// 暂存候选后成功返回。
    Publish,
    /// 暂存候选后返回 Err：候选必须整批丢弃。
    StageThenFail,
    /// 返回 `AgentError::Interrupted`：按中断分类，不是 fatal。
    Interrupt,
}

/// 同时实现 `before_agent` 与 `before_react_start`，用于断言调用次序。
struct StartupGateProbe {
    outcome: StartupGateOutcome,
    before_agent_fails: bool,
    order: Arc<std::sync::Mutex<Vec<&'static str>>>,
    gate_calls: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::middleware::Middleware for StartupGateProbe {
    fn name(&self) -> &str {
        "StartupGateProbe"
    }

    async fn before_agent(
        &self,
        _state: &mut dyn hook_state::BeforeAgentState,
    ) -> crate::error::AgentResult<()> {
        self.order.lock().unwrap().push("before_agent");
        if self.before_agent_fails {
            return Err(crate::error::AgentError::MiddlewareError {
                middleware: self.name().to_string(),
                reason: "soft before_agent failure".to_string(),
            });
        }
        Ok(())
    }

    async fn before_react_start(
        &self,
        state: &mut dyn hook_state::StartupState,
    ) -> crate::error::AgentResult<()> {
        self.order.lock().unwrap().push("before_react_start");
        self.gate_calls.fetch_add(1, Ordering::SeqCst);
        state.stage_startup_tools(startup_gate_candidate())?;
        match self.outcome {
            StartupGateOutcome::Publish => Ok(()),
            StartupGateOutcome::StageThenFail => Err(crate::error::AgentError::MiddlewareError {
                middleware: self.name().to_string(),
                reason: "startup gate failure".to_string(),
            }),
            StartupGateOutcome::Interrupt => Err(crate::error::AgentError::Interrupted),
        }
    }
}

/// 记录每次模型调用看到的 direct 工具名；首轮触发一次本地工具调用以产生第二轮迭代。
struct StartupGateLLM {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    seen_tools: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
}

#[async_trait::async_trait]
impl ReactLLM for StartupGateLLM {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        self.seen_tools.lock().unwrap().push(
            tools
                .iter()
                .map(|tool| tool.name().to_string())
                .collect::<Vec<_>>(),
        );
        match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => Ok(crate::agent::react::Reasoning::with_tools(
                "use the local probe tool",
                vec![crate::agent::react::ToolCall::new(
                    "startup-gate-tool-call",
                    "startup_gate_local_tool",
                    serde_json::json!({}),
                )],
            )),
            _ => Ok(crate::agent::react::Reasoning::with_answer(
                "thinking", "done",
            )),
        }
    }

    fn model_name(&self) -> String {
        "startup-gate-mock".to_string()
    }
}

#[tokio::test]
async fn test_react_start_gate_publishes_candidate_before_first_reason_and_runs_once() {
    let (working, catalog) = startup_gate_catalog();
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let gate_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let llm_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen_tools = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(StartupGateProbe {
        outcome: StartupGateOutcome::Publish,
        before_agent_fails: false,
        order: Arc::clone(&order),
        gate_calls: Arc::clone(&gate_calls),
    }));

    let session = Session::new(
        Arc::from("/tmp/react-start-gate-publish"),
        FrozenContext::builder().build(),
        None,
    );
    let turn = session.start_turn();
    let context = StageContext::best_effort_fixture_builder(
        turn,
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(StartupGateLLM {
        calls: Arc::clone(&llm_calls),
        seen_tools: Arc::clone(&seen_tools),
    }))
    .with_tools(working)
    .with_tool_catalog(Arc::clone(&catalog))
    .with_middleware_chain(Arc::new(chain))
    .build();
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("startup gate prompt"),
    ));

    assert!(matches!(
        run_react_loop(context.clone(), 10).await,
        LoopResult::Completed
    ));

    assert_eq!(
        *order.lock().unwrap(),
        vec!["before_agent", "before_react_start"],
        "闸门必须在首批 before_agent 之后执行"
    );
    assert_eq!(
        gate_calls.load(Ordering::SeqCst),
        1,
        "闸门每次 loop 只执行一次，第二轮迭代不得重复准入"
    );
    assert_eq!(
        llm_calls.load(Ordering::SeqCst),
        2,
        "工具往返产生第二轮迭代，闸门成功不得阻止 Reason"
    );
    let snapshot = catalog.snapshot();
    assert!(
        snapshot
            .direct_definitions
            .iter()
            .any(|definition| definition.name == "mcp__system__lookup"),
        "闸门候选必须作为 direct 工具进入目录"
    );
    assert!(
        snapshot.tools.contains_key("startup_gate_local_tool"),
        "闸门提交不得覆盖既有非 MCP 条目"
    );
    let seen = seen_tools.lock().unwrap();
    assert_eq!(seen.len(), 2, "两轮 Reason 都必须真正调用模型");
    assert!(
        seen[0].iter().any(|name| name == "mcp__system__lookup"),
        "闸门提交的 required 工具必须出现在首个模型请求的 tools 中, got {:?}",
        seen[0]
    );
}

#[tokio::test]
async fn test_react_start_gate_error_stops_before_compact_without_publishing_candidate() {
    let (working, catalog) = startup_gate_catalog();
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let gate_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let llm_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen_tools = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(StartupGateProbe {
        outcome: StartupGateOutcome::StageThenFail,
        before_agent_fails: false,
        order: Arc::clone(&order),
        gate_calls: Arc::clone(&gate_calls),
    }));
    let (bus, mut handles) = crate::agent::events_v2::EventBus::new(Default::default());

    let session = Session::new(
        Arc::from("/tmp/react-start-gate-error"),
        FrozenContext::builder().build(),
        None,
    );
    let turn = session.start_turn();
    let context = StageContext::best_effort_fixture_builder(
        turn,
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(StartupGateLLM {
        calls: Arc::clone(&llm_calls),
        seen_tools: Arc::clone(&seen_tools),
    }))
    .with_tools(working)
    .with_tool_catalog(Arc::clone(&catalog))
    .with_middleware_chain(Arc::new(chain))
    .with_event_bus(Arc::new(bus))
    .build();
    let published_before = catalog.snapshot();
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("startup gate prompt"),
    ));

    let result = run_react_loop(context.clone(), 10).await;

    match result {
        LoopResult::Error(crate::error::AgentError::MiddlewareError { middleware, reason }) => {
            assert_eq!(middleware, "StartupGateProbe");
            assert_eq!(reason, "startup gate failure");
        }
        other => panic!("闸门 Err 必须作为 loop fatal 返回, got {other:?}"),
    }
    assert_eq!(
        gate_calls.load(Ordering::SeqCst),
        1,
        "闸门失败即终止，不得重试或重复准入"
    );
    assert_eq!(
        llm_calls.load(Ordering::SeqCst),
        0,
        "闸门失败后不得进入 Reason 调用模型"
    );
    assert_eq!(
        drain_loop_observe_events(&mut handles).stage_lifecycle,
        expected_stage_lifecycle(&[Stage::Receive]),
        "闸门失败只允许完成 Receive，不得进入 Compact / Reason / Act"
    );
    assert!(
        Arc::ptr_eq(&published_before, &catalog.snapshot()),
        "闸门失败必须丢弃 state 内候选，不发布部分目录"
    );
}

#[tokio::test]
async fn test_react_start_gate_interrupted_maps_to_interrupted_not_fatal() {
    let (working, catalog) = startup_gate_catalog();
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let gate_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let llm_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen_tools = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(StartupGateProbe {
        outcome: StartupGateOutcome::Interrupt,
        before_agent_fails: false,
        order: Arc::clone(&order),
        gate_calls: Arc::clone(&gate_calls),
    }));

    let session = Session::new(
        Arc::from("/tmp/react-start-gate-interrupted"),
        FrozenContext::builder().build(),
        None,
    );
    let turn = session.start_turn();
    let context = StageContext::best_effort_fixture_builder(
        turn,
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(StartupGateLLM {
        calls: Arc::clone(&llm_calls),
        seen_tools: Arc::clone(&seen_tools),
    }))
    .with_tools(working)
    .with_tool_catalog(Arc::clone(&catalog))
    .with_middleware_chain(Arc::new(chain))
    .build();
    let published_before = catalog.snapshot();
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("startup gate prompt"),
    ));

    let result = run_react_loop(context.clone(), 10).await;

    assert!(
        matches!(result, LoopResult::Interrupted),
        "闸门 Interrupted 必须按中断分类，不得升级为 fatal, got {result:?}"
    );
    assert_eq!(gate_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        llm_calls.load(Ordering::SeqCst),
        0,
        "中断后不得进入 Reason 调用模型"
    );
    assert!(
        Arc::ptr_eq(&published_before, &catalog.snapshot()),
        "中断时暂存候选必须随 state 丢弃"
    );
}

#[tokio::test]
async fn test_before_agent_soft_failure_still_reaches_reason_after_startup_gate() {
    let (working, catalog) = startup_gate_catalog();
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let gate_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let llm_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen_tools = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(StartupGateProbe {
        outcome: StartupGateOutcome::Publish,
        before_agent_fails: true,
        order: Arc::clone(&order),
        gate_calls: Arc::clone(&gate_calls),
    }));

    let session = Session::new(
        Arc::from("/tmp/react-start-gate-soft-before-agent"),
        FrozenContext::builder().build(),
        None,
    );
    let turn = session.start_turn();
    let context = StageContext::best_effort_fixture_builder(
        turn,
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(StartupGateLLM {
        calls: Arc::clone(&llm_calls),
        seen_tools: Arc::clone(&seen_tools),
    }))
    .with_tools(working)
    .with_tool_catalog(Arc::clone(&catalog))
    .with_middleware_chain(Arc::new(chain))
    .build();
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("startup gate prompt"),
    ));

    assert!(
        matches!(
            run_react_loop(context.clone(), 10).await,
            LoopResult::Completed
        ),
        "既有 before_agent 的软失败必须保持 warn 降级、不阻止 loop"
    );
    assert_eq!(
        *order.lock().unwrap(),
        vec!["before_agent", "before_react_start"],
        "before_agent 软失败不影响闸门按序执行"
    );
    assert_eq!(
        llm_calls.load(Ordering::SeqCst),
        2,
        "软失败后 loop 仍必须走到 Reason 并完成工具往返"
    );
    assert_eq!(
        gate_calls.load(Ordering::SeqCst),
        1,
        "软失败不得让闸门重复执行"
    );
}

#[tokio::test]
async fn test_react_start_gate_skipped_when_loop_exits_at_receive() {
    let (working, catalog) = startup_gate_catalog();
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let gate_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let llm_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen_tools = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(StartupGateProbe {
        outcome: StartupGateOutcome::Publish,
        before_agent_fails: false,
        order: Arc::clone(&order),
        gate_calls: Arc::clone(&gate_calls),
    }));

    let session = Session::new(
        Arc::from("/tmp/react-start-gate-empty-queue"),
        FrozenContext::builder().build(),
        None,
    );
    let turn = session.start_turn();
    let context = StageContext::best_effort_fixture_builder(
        turn,
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(StartupGateLLM {
        calls: Arc::clone(&llm_calls),
        seen_tools: Arc::clone(&seen_tools),
    }))
    .with_tools(working)
    .with_tool_catalog(Arc::clone(&catalog))
    .with_middleware_chain(Arc::new(chain))
    .build();

    assert!(matches!(
        run_react_loop(context.clone(), 10).await,
        LoopResult::Completed
    ));
    assert_eq!(
        gate_calls.load(Ordering::SeqCst),
        0,
        "Receive 直接退出时不得调用启动闸门"
    );
    assert!(order.lock().unwrap().is_empty());
    assert_eq!(llm_calls.load(Ordering::SeqCst), 0);
}
