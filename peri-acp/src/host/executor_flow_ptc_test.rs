use super::*;

// ── PTC production-path E2E ────────────────────────────────────────────────

#[cfg(not(windows))]
struct PtcScriptedModel {
    calls: AtomicUsize,
    visible_tools: Arc<Mutex<Vec<String>>>,
    source: String,
}

#[cfg(not(windows))]
#[async_trait]
impl Model for PtcScriptedModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: true,
            supports_reasoning: false,
            supports_vision: false,
            supports_streaming: true,
        }
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: AgentCancellationToken,
    ) -> ModelResult<ModelStream> {
        *self.visible_tools.lock().unwrap() =
            request.tools.iter().map(|tool| tool.name.clone()).collect();
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let (message, stop_reason, mut events) = if call < 2 {
            let (name, arguments) = if call == 0 {
                (
                    "SearchExtraTools",
                    serde_json::json!({ "query": "ptc run code 程序化 批量" }),
                )
            } else {
                (
                    "ExecuteExtraTool",
                    serde_json::json!({
                        "tool_name": "RunPtcCode",
                        "params": { "source": self.source }
                    }),
                )
            };
            let tool_call = ToolCall::new(
                "ptc-e2e-outer",
                name,
                JsonObject::from_value(arguments.clone()).unwrap(),
            );
            (
                ModelMessage::assistant(vec![], vec![tool_call]),
                StopReason::ToolUse,
                vec![ModelStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some("ptc-e2e-outer".into()),
                    name: Some(name.into()),
                    arguments_delta: arguments.to_string(),
                }],
            )
        } else {
            (
                ModelMessage::assistant_text("PTC E2E complete"),
                StopReason::EndTurn,
                vec![ModelStreamEvent::TextDelta {
                    text: "PTC E2E complete".into(),
                }],
            )
        };
        let response = ModelResponse::new(message, stop_reason, None, None)?;
        events.push(ModelStreamEvent::Completed(response));
        Ok(ModelStream::with_parent_cancellation(
            stream::iter(events.into_iter().map(Ok)),
            cancellation,
        ))
    }
}

#[cfg(not(windows))]
struct RecordingApproveBroker {
    approvals: Arc<Mutex<Vec<String>>>,
}

#[cfg(not(windows))]
#[async_trait]
impl UserInteractionBroker for RecordingApproveBroker {
    async fn request(&self, ctx: InteractionContext) -> InteractionResponse {
        match ctx {
            InteractionContext::Approval { items } => {
                self.approvals
                    .lock()
                    .unwrap()
                    .extend(items.iter().map(|item| item.tool_name.clone()));
                InteractionResponse::Decisions(
                    items
                        .iter()
                        .map(|_| peri_acp_types::interaction::ApprovalDecision::Approve {
                            source: None,
                        })
                        .collect(),
                )
            }
            _ => InteractionResponse::Rejected,
        }
    }
}

#[cfg(not(windows))]
fn write_ptc_cache_fixture(root: &std::path::Path) {
    let package = root.join(".peri/ptc/0.2.3/node_modules/@peri-code/ptc");
    std::fs::create_dir_all(package.join("dist")).unwrap();
    std::fs::write(
        package.join("package.json"),
        r#"{"name":"@peri-code/ptc","version":"0.2.3","type":"module","main":"dist/index.js","bin":{"peri-ptc":"dist/peri-ptc.js"},"periProtocolVersion":1,"periBuildId":"@peri-code/ptc@0.2.3"}"#,
    )
    .unwrap();
    std::fs::write(package.join("dist/index.js"), "export {};\n").unwrap();
    std::fs::write(
        package.join("dist/peri-ptc.js"),
        r#"import readline from 'node:readline';
const pending = new Map();
let nextId = 100;
function send(message) { process.stdout.write(JSON.stringify(message) + '\n'); }
function callTool(toolName, input, options = {}) {
  if (options.signal?.aborted) return Promise.reject(Object.assign(new Error('cancelled'), { name: 'AbortError' }));
  const id = nextId++;
  const invocationId = `ptc-${id}`;
  send({ jsonrpc: '2.0', id, method: 'tool/call', params: { invocationId, toolName, input } });
  return new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
}
const tools = new Proxy({}, { get: (_, toolName) => (input, options) => callTool(toolName, input, options) });
const AsyncFunction = Object.getPrototypeOf(async function() {}).constructor;
const rl = readline.createInterface({ input: process.stdin });
rl.on('line', async line => {
  const request = JSON.parse(line);
  if (request.method === 'ptc/start') {
    send({ jsonrpc: '2.0', id: request.id, result: { ok: true, protocolVersion: 1, buildId: '@peri-code/ptc@0.2.3' } });
  } else if (request.method === 'execute') {
    try {
      const logs = [];
      const console = { log: (...values) => logs.push(values.join(' ')) };
      const result = await new AsyncFunction('tools', 'input', 'console', request.params.source)(tools, request.params.input, console);
      send({ jsonrpc: '2.0', id: request.id, result: { value: result, logs } });
    } catch (error) {
      send({ jsonrpc: '2.0', id: request.id, error: { code: -32001, message: 'JavaScript execution failed', data: { code: error.code ?? 'EXECUTION_FAILED' } } });
    }
  } else if (Object.hasOwn(request, 'id')) {
    const waiter = pending.get(request.id);
    if (!waiter) return;
    pending.delete(request.id);
    if (request.error) {
      const error = Object.assign(new Error(request.error.message), request.error.data ?? {});
      error.name = 'ToolCallError';
      waiter.reject(error);
    } else waiter.resolve(request.result);
  }
});
"#,
    )
    .unwrap();
}

/// 起一份**生产同构**的 builtin 宿主 pool（真实注入 + 真实 `run_initialize`），
/// 供 PTC 生产路径用例取用真实的 `mcp__workspace__*` 工具面。
///
/// 为什么必须起 pool：C1 摘除 `FilesystemMiddleware` / `TerminalMiddleware` 之后，
/// 7 个文件工具在链上已无直接供给，唯一来源是 `workspace` builtin 实例经
/// `McpMiddleware::collect_tools` 投影出的桥（`McpToolBridge`）。PTC 的工具目录取自
/// `state.local_tools()`（= 链收集后的直连工具面），因此没有 pool 就没有
/// `Read`，脚本里的 `tools.Read(...)` 必然落空。
///
/// 装配步骤序列与生产 `host/assemble.rs::pending_mcp_pool` 逐位相同，也与
/// `host::mcp_v4_wave2` 的 `BuiltinHostFixture` 同形：pool → 绑定 cwd → 注入
/// `BuiltinInstanceContext`（A33：必须早于 `run_initialize`）→ `run_initialize`
/// → 等 `Ready`。
///
/// 上下文取舍（逐条登记，避免读成「顺手漏配」）：
/// - `cron` / `lsp` 给最小可用输入：二者缺输入会让实例**不可装配**（`instance_input_ready`
///   为假），而本用例要的是「生产同构的实例集合」，不是故障注入。
/// - `workspace` **不给** session 级输入（`WorkspaceInstanceInput` 留空）：本用例只走
///   `Read` 前台路径，7 个工具照常声明与执行；`TaskManager` / `on_bg_complete` 缺失带来的
///   `Bash` 后台退化面（AW3-11）已由 `mcp::builtin::workspace` 的线路级用例覆盖，
///   此处复刻 session 装配反而不必要——与 wave2 夹具的 `workspace_input: None`
///   （夹具不构造 session）是同一处置。
/// - `claude_home` 取 `cwd` 而非真实 `~/.claude`：隔离本机配置与插件，让工具面只由
///   注册表决定（否则用例结果随开发者机器而变）。
#[cfg(not(windows))]
async fn start_workspace_pool(cwd: &std::path::Path) -> (Arc<McpClientPool>, McpTaskOwner) {
    let cwd_str = cwd.to_string_lossy().into_owned();
    let (cron_trigger_tx, _cron_triggers) = tokio::sync::mpsc::unbounded_channel();
    let scheduler = Arc::new(parking_lot::Mutex::new(peri_mcp_cron::CronScheduler::new(
        cron_trigger_tx,
    )));
    let context = BuiltinInstanceContext::new(cwd_str.clone())
        .with_cron(CronInstanceInput {
            scheduler,
            tick_enabled: false,
        })
        .with_lsp(LspInstanceInput {
            pool: peri_mcp_lsp::create_host_lsp_pool(&cwd_str, &[]),
        });

    let (owner, spawner) = McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    pool.bind_execution_cwd(cwd)
        .expect("夹具首次绑定 execution cwd");
    // A33：上下文注入必须早于 `run_initialize`（含其后台 spawn 窗口）。
    pool.set_builtin_instance_context(Arc::new(context))
        .expect("首次注入必须成功（夹具不会二次注入）");

    let (status_tx, mut status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
    let init_pool = Arc::clone(&pool);
    let workspace = cwd.to_path_buf();
    let claude_home = cwd.to_path_buf();
    let init_task = tokio::spawn(async move {
        McpClientPool::run_initialize(init_pool, &workspace, &claude_home, status_tx, None).await;
    });
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if matches!(
                &*status_rx.borrow_and_update(),
                McpInitStatus::Ready { .. } | McpInitStatus::Failed(_)
            ) {
                break;
            }
            status_rx
                .changed()
                .await
                .expect("init status 发送端在本任务内");
        }
    })
    .await
    .expect("真实 MCP 初始化不得挂起");
    tokio::time::timeout(std::time::Duration::from_secs(30), init_task)
        .await
        .expect("初始化任务必须在有界等待内结束")
        .expect("初始化任务不得 panic");

    let ready_total = match &*status_rx.borrow() {
        McpInitStatus::Ready { total } => *total,
        other => panic!("生产同构装配必须收口为 Ready（否则断言面空洞）: {other:?}"),
    };
    assert_eq!(
        ready_total,
        peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES.len(),
        "注册表里每个 builtin 实例都必须连上，否则工作区工具面不完整"
    );

    (pool, owner)
}

#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn test_ptc_runs_through_acp_session_agent_production_path() {
    let tmp = tempfile::tempdir().unwrap();
    let ptc_cache = tempfile::tempdir().unwrap();
    write_ptc_cache_fixture(ptc_cache.path());
    let _home = HomeGuard::set(ptc_cache.path());
    std::fs::write(tmp.path().join("a.txt"), "alpha").unwrap();
    std::fs::write(tmp.path().join("b.txt"), "beta").unwrap();
    let a = serde_json::to_string(&tmp.path().join("a.txt").to_string_lossy()).unwrap();
    let b = serde_json::to_string(&tmp.path().join("b.txt").to_string_lossy()).unwrap();
    let source = format!(
        r#"const [a, b] = await Promise.all([
            tools.Read({{ file_path: {a} }}),
            tools.Read({{ file_path: {b} }})
        ]);
        let structured;
        try {{ await tools.NoSuchPtcTool({{}}); }}
        catch (error) {{ structured = {{ name: error.name, code: error.code }}; }}
        const controller = new AbortController(); controller.abort();
        let cancelled;
        try {{ await tools.Read({{ file_path: {a} }}, {{ signal: controller.signal }}); }}
        catch (error) {{ cancelled = error.name; }}
        return {{ a, b, structured, cancelled }};"#
    );
    let visible_tools = Arc::new(Mutex::new(Vec::new()));
    let model = Arc::new(PtcScriptedModel {
        calls: AtomicUsize::new(0),
        visible_tools: Arc::clone(&visible_tools),
        source,
    }) as Arc<dyn Model>;
    let approvals = Arc::new(Mutex::new(Vec::new()));
    let mut ctx = make_session_context("ptc-production-e2e").await;
    ctx.cwd = tmp.path().to_string_lossy().into_owned();
    // 生产同构的 builtin pool：C1 之后 7 个文件工具只由 `workspace` 实例供给，
    // PTC 目录（`state.local_tools()`）要拿到 `mcp__workspace__*` 就必须有真实 pool。
    // `_mcp_owner` 必须活到用例结束——drop 即回收内置 transport。
    let (mcp_pool, _mcp_owner) = start_workspace_pool(tmp.path()).await;
    ctx.mcp_pool = Some(mcp_pool as Arc<dyn McpPoolPort>);
    ctx.permission_mode = SharedPermissionMode::new(PermissionMode::Default);
    ctx.broker = Arc::new(RecordingApproveBroker {
        approvals: Arc::clone(&approvals),
    });
    ctx.primary_llm_factory = Some(Arc::new(move || Arc::clone(&model)));
    let stage_build = make_stage_build(&ctx);
    let sink = Arc::new(MockEventSink::new());
    let turn = make_turn_input(
        Arc::clone(&sink) as Arc<dyn EventSink>,
        MessageContent::text("run the scripted PTC scenario"),
        false,
        vec![],
        stage_build,
    );

    let result = run_session_loop(ctx, turn).await;

    assert!(
        result.ok,
        "PTC production path failed: stop_reason={:?}",
        result.stop_reason
    );
    let tools = visible_tools.lock().unwrap();
    assert!(tools.iter().any(|name| name == "ExecuteExtraTool"));
    assert!(tools.iter().any(|name| name == "SearchExtraTools"));
    assert!(!tools.iter().any(|name| name == "RunPtcCode"));
    assert!(!tools.iter().any(|name| name == "run_code"));
    // `Read` 只由 workspace 实例以原名提供；旧前缀不得继续暴露。
    assert!(
        tools.iter().any(|name| name == "Read"),
        "PTC 目录必须由 workspace 实例供给原名 Read：{tools:?}"
    );
    assert!(
        !tools.iter().any(|name| name == "mcp__workspace__Read"),
        "旧前缀名不得再出现在模型工具面：{tools:?}"
    );
    assert_eq!(approvals.lock().unwrap().as_slice(), ["RunPtcCode"]);
    let events = sink.pushed_events.lock().unwrap().join("\n");
    assert!(events.contains("ptc-e2e-outer/ptc-"), "{events}");
    assert!(events.contains("RunPtcCode"), "{events}");
    assert!(events.contains("UNKNOWN_TOOL"), "{events}");
    assert!(
        events.contains("alpha") && events.contains("beta"),
        "{events}"
    );
}
