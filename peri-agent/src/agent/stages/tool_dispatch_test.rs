//! 从 tool_dispatch.rs 分离的测试模块
use crate::middleware::capabilities as hook_state;
use std::collections::BTreeMap;

use crate::agent::events_v2::RenderEvent;
use crate::agent::react::Reasoning;
use crate::agent::react::ToolCall;
use crate::agent::react::ToolResult;
use crate::agent::stages::tool_dispatch::execution;

use crate::agent::stages::StageContext;
use crate::error::AgentError;
use crate::error::AgentResult;
use crate::messages::BaseMessage;

use crate::tools::BaseTool;
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::middleware::r#trait::Middleware;
use crate::middleware::MiddlewareChain;
use crate::session::queue::MessageQueue;
use crate::session::transcript::MessageTranscript;
use crate::session::turn::TurnContext;

// ── normalize_params ──

/// 可配置 schema 的测试工具：归一化是否生效取决于 schema 是否声明 file_path
struct SchemaToolStub {
    name: &'static str,
    schema: serde_json::Value,
}

#[async_trait::async_trait]
impl BaseTool for SchemaToolStub {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        ""
    }
    fn parameters(&self) -> serde_json::Value {
        self.schema.clone()
    }
    fn aliases(&self) -> &[&str] {
        &[]
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok("ok".to_string())
    }
}

fn read_schema_tool() -> SchemaToolStub {
    SchemaToolStub {
        name: "Read",
        schema: json!({
            "type": "object",
            "properties": {"file_path": {"type": "string"}}
        }),
    }
}

fn grep_schema_tool() -> SchemaToolStub {
    SchemaToolStub {
        name: "Grep",
        schema: json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string"}
            }
        }),
    }
}

#[test]
fn test_normalize_params_path_alias_to_file_path() {
    let input = json!({"path": "/tmp/foo.rs"});
    // Read 等工具 schema 声明 file_path → path 别名仍归一化
    let out = crate::tools::normalize_params(input, Some(&read_schema_tool()));
    assert!(out.get("file_path").is_some());
    assert!(out.get("path").is_none());
}

#[test]
fn test_normalize_params_keep_file_path_when_present() {
    // 当 file_path 已存在时，path 别名不覆盖
    let input = json!({"path": "/a", "file_path": "/b"});
    let out = crate::tools::normalize_params(input, Some(&read_schema_tool()));
    assert_eq!(
        out.get("file_path")
            .expect("the fixture must contain file_path"),
        &json!("/b")
    );
    // path 仍然保留（未触发别名替换）
    assert!(out.get("path").is_some());
}

#[test]
fn test_normalize_params_does_not_rename_path_for_path_schema_tools() {
    // 回归：Grep/Glob 的 schema 参数名就是 path，不得重命名为 file_path
    // （曾导致 path 丢失、搜索静默回退全仓库）
    let input = json!({"pattern": "tokio|serde", "path": "/tmp/a"});
    let out = crate::tools::normalize_params(input, Some(&grep_schema_tool()));
    assert_eq!(
        out.get("path").expect("the fixture must contain path"),
        &json!("/tmp/a")
    );
    assert!(out.get("file_path").is_none());
}

#[test]
fn test_normalize_params_passthrough_non_object() {
    let input = json!("string");
    let out = crate::tools::normalize_params(input.clone(), None);
    assert_eq!(out, input);
}

#[test]
fn test_normalize_params_keep_unrelated_keys() {
    let input = json!({"query": "hello", "limit": 10});
    let out = crate::tools::normalize_params(input, None);
    assert_eq!(
        out.get("query").expect("the fixture must contain query"),
        &json!("hello")
    );
    assert_eq!(
        out.get("limit").expect("the fixture must contain limit"),
        &json!(10)
    );
}

#[test]
fn test_canonical_resolver_normalizes_alias_and_params() {
    use std::collections::BTreeMap;

    use crate::tools::{DirectToolInvocationResolver, ToolInvocationResolver};

    struct AliasTool {
        schema: serde_json::Value,
    }
    #[async_trait::async_trait]
    impl BaseTool for AliasTool {
        fn name(&self) -> &str {
            "Bash"
        }
        fn description(&self) -> &str {
            ""
        }
        fn parameters(&self) -> serde_json::Value {
            self.schema.clone()
        }
        fn aliases(&self) -> &[&str] {
            &["Shell"]
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok(String::new())
        }
    }

    // Bash 真实 schema 无 file_path → path 是无关参数，不归一化
    let mut tools: BTreeMap<String, Arc<dyn BaseTool>> = BTreeMap::new();
    tools.insert(
        "Bash".to_string(),
        Arc::new(AliasTool {
            schema: json!({"type": "object", "properties": {"command": {"type": "string"}}}),
        }),
    );

    let invocation = DirectToolInvocationResolver
        .resolve(
            &ToolCall::new("call_1", "SHELL", json!({"path": "/tmp/x"})),
            &tools,
        )
        .expect("alias should resolve");

    assert_eq!(invocation.raw_call.name, "SHELL");
    assert_eq!(invocation.policy_call.name, "Bash");
    assert_eq!(invocation.policy_call.input, json!({"path": "/tmp/x"}));

    // 声明 file_path 的工具（如 Write）→ path 别名仍归一化
    let mut file_tools: BTreeMap<String, Arc<dyn BaseTool>> = BTreeMap::new();
    file_tools.insert(
        "Bash".to_string(),
        Arc::new(AliasTool {
            schema: json!({
                "type": "object",
                "properties": {"file_path": {"type": "string"}}
            }),
        }),
    );
    let invocation = DirectToolInvocationResolver
        .resolve(
            &ToolCall::new("call_2", "SHELL", json!({"path": "/tmp/y"})),
            &file_tools,
        )
        .expect("alias should resolve");
    assert_eq!(invocation.policy_call.input, json!({"file_path": "/tmp/y"}));
}

#[tokio::test]
async fn test_dispatch_rejects_duplicate_and_empty_ids_before_policy_or_invoke() {
    struct CountingTool {
        name: &'static str,
        invoked: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl BaseTool for CountingTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            ""
        }
        fn parameters(&self) -> serde_json::Value {
            json!({})
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            self.invoked
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(self.name.to_string())
        }
    }

    struct CountingMiddleware(Arc<std::sync::atomic::AtomicUsize>);

    #[async_trait::async_trait]
    impl Middleware for CountingMiddleware {
        fn name(&self) -> &str {
            "CountingMiddleware"
        }
        async fn before_tools_batch(
            &self,
            _state: &mut dyn hook_state::BeforeToolState,
            calls: &[ToolCall],
        ) -> Vec<crate::error::AgentResult<ToolCall>> {
            self.0
                .fetch_add(calls.len(), std::sync::atomic::Ordering::Relaxed);
            calls.iter().cloned().map(Ok).collect()
        }
    }

    let invoked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let policy_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut tools = BTreeMap::new();
    tools.insert(
        "Read".to_string(),
        Arc::new(CountingTool {
            name: "Read",
            invoked: Arc::clone(&invoked),
        }) as Arc<dyn BaseTool>,
    );
    tools.insert(
        "Bash".to_string(),
        Arc::new(CountingTool {
            name: "Bash",
            invoked: Arc::clone(&invoked),
        }) as Arc<dyn BaseTool>,
    );
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(CountingMiddleware(Arc::clone(&policy_calls))));

    let mut ctx = make_test_ctx();
    ctx.runtime.tools.write().extend(tools);
    ctx.runtime.middleware_chain = Arc::new(chain);
    let reasoning = Reasoning::with_tools(
        "",
        vec![
            ToolCall::new("same", "Read", json!({})),
            ToolCall::new("same", "Bash", json!({})),
            ToolCall::new("", "Read", json!({})),
        ],
    );

    let catalog = ctx
        .runtime
        .tool_catalog
        .pin_working_tools(&ctx.runtime.tools.read())
        .expect("the pin working tools fixture result is required for this assertion");
    let outcome = crate::agent::stages::tool_dispatch::dispatch_tools(
        &ctx,
        &reasoning,
        &catalog,
        &CancellationToken::new(),
    )
    .await
    .expect("malformed calls should settle as tool errors");

    assert_eq!(policy_calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(invoked.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(outcome.results.len(), 3);
    assert!(outcome.results.iter().all(|(_, result)| result.is_error));
    assert_eq!(
        outcome
            .results
            .iter()
            .map(|(call, _)| call.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Read", "Bash", "Read"]
    );
}

#[tokio::test]
async fn test_dispatch_batch_cardinality_error_stops_before_tool_execution() {
    struct CountingTool(Arc<std::sync::atomic::AtomicUsize>);
    #[async_trait::async_trait]
    impl BaseTool for CountingTool {
        fn name(&self) -> &str {
            "Read"
        }
        fn description(&self) -> &str {
            ""
        }
        fn parameters(&self) -> serde_json::Value {
            json!({})
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok("unexpected execution".to_string())
        }
    }

    struct MissingBatchResult;
    #[async_trait::async_trait]
    impl Middleware for MissingBatchResult {
        fn name(&self) -> &str {
            "MissingBatchResult"
        }
        async fn before_tools_batch(
            &self,
            _state: &mut dyn hook_state::BeforeToolState,
            _calls: &[ToolCall],
        ) -> Vec<crate::error::AgentResult<ToolCall>> {
            Vec::new()
        }
    }

    let invoked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut ctx = make_test_ctx();
    ctx.runtime.tools.write().insert(
        "Read".to_string(),
        Arc::new(CountingTool(Arc::clone(&invoked))),
    );
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(MissingBatchResult));
    ctx.runtime.middleware_chain = Arc::new(chain);
    let reasoning = Reasoning::with_tools("", vec![ToolCall::new("call_1", "Read", json!({}))]);
    let catalog = ctx
        .runtime
        .tool_catalog
        .pin_working_tools(&ctx.runtime.tools.read())
        .expect("the pin working tools fixture result is required for this assertion");

    let error = match crate::agent::stages::tool_dispatch::dispatch_tools(
        &ctx,
        &reasoning,
        &catalog,
        &CancellationToken::new(),
    )
    .await
    {
        Ok(_) => panic!("批量规模错配必须让 dispatch 失败，而不是继续执行工具"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        AgentError::MiddlewareError { middleware, reason }
            if middleware == "MissingBatchResult"
                && reason == "before_tools_batch returned 0 results for 1 calls"
    ));
    assert_eq!(invoked.load(std::sync::atomic::Ordering::Relaxed), 0);
}

fn make_test_ctx() -> StageContext {
    let turn = TurnContext::new(
        std::sync::Arc::from("/tmp"),
        std::sync::Arc::new(CancellationToken::new()),
    );
    let transcript = std::sync::Arc::new(parking_lot::RwLock::new(MessageTranscript::new()));
    let queue = MessageQueue::new();
    StageContext::new(turn, transcript, queue)
}

#[tokio::test]
async fn test_handle_consecutive_failures_success_resets() {
    let ctx = make_test_ctx();
    // 先设置失败计数为非 0
    ctx.compact
        .consecutive_failures
        .store(4, std::sync::atomic::Ordering::Relaxed);
    let ok_call = ToolCall {
        id: "call_1".to_string(),
        name: "Read".to_string(),
        input: serde_json::json!({}),
    };
    let ok_result = ToolResult::success("call_1", "Read", "ok");
    crate::agent::stages::tool_dispatch::handle_consecutive_failures(
        &ctx,
        &[execution::SettledInvocation::unreported(ok_call, ok_result)],
    )
    .await;
    assert_eq!(
        ctx.compact
            .consecutive_failures
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "成功执行后失败计数器应重置为 0"
    );
}

#[tokio::test]
async fn test_resolution_error_emits_tool_started_and_ended() {
    use crate::agent::events_v2::{EventBus, RenderEvent};
    use std::sync::Arc;

    let (bus, mut handles) = EventBus::new(Default::default());
    let turn = TurnContext::new(Arc::from("/tmp"), Arc::new(CancellationToken::new()));
    let transcript = Arc::new(parking_lot::RwLock::new(MessageTranscript::new()));
    let queue = MessageQueue::new();
    let ctx = StageContext::builder(turn, transcript, queue)
        .with_event_bus(Arc::new(bus))
        .build();

    let mut reasoning = Reasoning::with_tools(
        "",
        vec![ToolCall::new("missing-1", "NotARealTool", json!({}))],
    );
    // 真实模型路径总能拿到本条 assistant 消息身份；工具事件必须携带它，
    // TUI 才能在纯工具轮次建立可淘汰归属。
    let source_message = crate::messages::BaseMessage::ai_with_tool_calls(
        "",
        vec![crate::messages::ToolCallRequest::new(
            "missing-1",
            "NotARealTool",
            json!({}),
        )],
    );
    let ai_msg_id = source_message.id();
    reasoning.source_message = Some(source_message);
    ctx.compact
        .token_tracker
        .write()
        .accumulate(&peri_model::TokenUsage {
            input_tokens: 1_000,
            ..Default::default()
        });
    let catalog = ctx
        .runtime
        .tool_catalog
        .pin_working_tools(&ctx.runtime.tools.read())
        .expect("the pin working tools fixture result is required for this assertion");
    let outcome = crate::agent::stages::tool_dispatch::dispatch_tools(
        &ctx,
        &reasoning,
        &catalog,
        &CancellationToken::new(),
    )
    .await
    .expect("unknown tool should settle as error");
    let error_output = &outcome.results[0].1.output;
    assert!(!error_output.is_empty());
    assert_eq!(
        ctx.compact.token_tracker.read().estimated_context_tokens(),
        Some(1_000 + (error_output.chars().count() / 4) as u64),
        "已经提交的解析失败结果也必须计入下一轮压力"
    );

    let mut started = false;
    let mut ended = false;
    while let Some(event) = handles.try_render() {
        match event {
            RenderEvent::ToolStarted {
                tool_call_id,
                name,
                origin_message_id,
                origin_readable,
                ..
            } => {
                assert_eq!(tool_call_id, "missing-1");
                assert_eq!(name, "NotARealTool");
                assert_eq!(
                    origin_message_id,
                    Some(ai_msg_id),
                    "结算路径的 ToolStarted 必须携带真实 assistant 消息身份"
                );
                assert_eq!(
                    origin_readable,
                    Some(true),
                    "结算路径归属模型批次，必须显式标记可回读"
                );
                started = true;
            }
            RenderEvent::ToolEnded {
                tool_call_id,
                is_error,
                ..
            } => {
                assert_eq!(tool_call_id, "missing-1");
                assert!(is_error);
                ended = true;
            }
            _ => {}
        }
    }
    assert!(started, "resolution error must emit ToolStarted");
    assert!(ended, "resolution error must emit ToolEnded");
}

#[test]
fn settled_render_suppression_requires_host_proxy_identity() {
    struct IdentityTool {
        name: &'static str,
        server: Option<&'static str>,
        namespace: Option<&'static str>,
    }

    #[async_trait::async_trait]
    impl BaseTool for IdentityTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "identity fixture"
        }
        fn parameters(&self) -> serde_json::Value {
            json!({})
        }
        fn mcp_server_name(&self) -> Option<&str> {
            self.server
        }
        fn namespace(&self) -> Option<&str> {
            self.namespace
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok(String::new())
        }
    }

    let build = |tool: Arc<dyn BaseTool>| {
        let tools = BTreeMap::from([("ToolInvoke".to_string(), tool)]);
        let shared: crate::agent::stages::SharedToolMap = Arc::new(parking_lot::RwLock::new(tools));
        let catalog = Arc::new(
            crate::session::tool_catalog::SessionToolCatalog::try_new(shared.read().clone(), None)
                .expect("identity fixtures must not collide"),
        );
        let turn = TurnContext::new(Arc::from("/tmp"), Arc::new(CancellationToken::new()));
        let transcript = Arc::new(parking_lot::RwLock::new(MessageTranscript::new()));
        StageContext::builder(turn, transcript, MessageQueue::new())
            .with_tools(shared)
            .with_tool_catalog(catalog)
            .build()
    };
    let call = ToolCall::new("proxy-1", "ToolInvoke", json!({}));

    let host = build(Arc::new(IdentityTool {
        name: "ToolInvoke",
        server: None,
        namespace: Some("meta"),
    }));
    assert!(
        !super::tool_dispatcher::should_emit_settled_tool_render(&host, &call),
        "宿主代理解析失败时不投影 wrapper 行"
    );

    let external = build(Arc::new(IdentityTool {
        name: "ToolInvoke",
        server: Some("ext"),
        namespace: None,
    }));
    assert!(
        super::tool_dispatcher::should_emit_settled_tool_render(&external, &call),
        "外部 MCP 同名工具不是宿主代理，必须保留 settled render"
    );

    let unresolved = make_test_ctx();
    assert!(
        super::tool_dispatcher::should_emit_settled_tool_render(&unresolved, &call),
        "无法解析的名字保持成对 emit"
    );
}

// ── 结算路径逐条日志（本文件专用内联最小捕获，不引入公共测试工具/依赖）──────
//
// `dispatch_tools` 由测试线程 await，`warn!` 就发生在该线程，线程局部
// `set_default` 即可捕获；`with_max_level(INFO)` 复现生产默认过滤起点
// （`telemetry/subscriber.rs` 的 `EnvFilter::new("info,...")`），从而证明
// 这些记录在默认可见级别下出现。
//

#[derive(Clone, Default)]
struct LogCapture(Arc<parking_lot::Mutex<Vec<u8>>>);

impl std::io::Write for LogCapture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl LogCapture {
    fn text(&self) -> String {
        String::from_utf8_lossy(self.0.lock().as_slice()).into_owned()
    }

    /// 只取结算路径的逐条记录行，避免其它 `warn!` 干扰计数。
    fn resolution_log_lines(&self) -> Vec<String> {
        self.text()
            .lines()
            .filter(|line| line.contains("tool call resolution failed"))
            .map(str::to_string)
            .collect()
    }

    fn subscriber(&self) -> impl tracing::Subscriber + Send + Sync {
        let sink = self.clone();
        tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .with_writer(move || sink.clone())
            .finish()
    }
}

#[tokio::test]
async fn test_malformed_tool_ids_log_per_call_at_default_level() {
    let capture = LogCapture::default();
    let _subscriber = tracing::subscriber::set_default(capture.subscriber());

    // 畸形 ID 在 resolver 之前结算，空工具表即可复现。
    let ctx = make_test_ctx();
    let reasoning = Reasoning::with_tools(
        "",
        vec![
            ToolCall::new("dup", "Read", json!({})),
            ToolCall::new("dup", "Bash", json!({})),
            ToolCall::new("", "Read", json!({})),
        ],
    );
    let catalog = ctx
        .runtime
        .tool_catalog
        .pin_working_tools(&ctx.runtime.tools.read())
        .expect("the pin working tools fixture result is required for this assertion");
    let outcome = crate::agent::stages::tool_dispatch::dispatch_tools(
        &ctx,
        &reasoning,
        &catalog,
        &CancellationToken::new(),
    )
    .await
    .expect("malformed calls should settle as tool errors");
    assert_eq!(outcome.results.len(), 3);

    let lines = capture.resolution_log_lines();
    assert_eq!(lines.len(), 3, "每条结算错误都必须有独立记录: {lines:?}");
    assert_eq!(
        lines.iter().filter(|line| line.contains("dup")).count(),
        2,
        "重复 ID 的两条调用各自记录，而不是合并: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("dup") && line.contains("tool=Read")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("dup") && line.contains("tool=Bash")),
        "{lines:?}"
    );
    let empty_id_line = lines
        .iter()
        .find(|line| line.contains("tool=Read") && !line.contains("dup"))
        .unwrap_or_else(|| panic!("空 ID 缺少逐条记录: {lines:?}"));
    assert!(empty_id_line.contains("tool_call_id="), "{empty_id_line}");
    assert!(
        empty_id_line.contains("reason=malformed tool call id"),
        "{empty_id_line}"
    );
}

#[tokio::test]
async fn test_resolver_failure_logs_per_call_at_default_level() {
    let capture = LogCapture::default();
    let _subscriber = tracing::subscriber::set_default(capture.subscriber());

    // BestEffortFixture：`ensure()` 返回 Ok(None)，进入 resolver 兜底分支。
    let ctx = make_test_ctx();
    let reasoning = Reasoning::with_tools(
        "",
        vec![ToolCall::new("missing-1", "NotARealTool", json!({}))],
    );
    let catalog = ctx
        .runtime
        .tool_catalog
        .pin_working_tools(&ctx.runtime.tools.read())
        .expect("the pin working tools fixture result is required for this assertion");
    let _ = crate::agent::stages::tool_dispatch::dispatch_tools(
        &ctx,
        &reasoning,
        &catalog,
        &CancellationToken::new(),
    )
    .await
    .expect("unknown tool should settle as error");

    let lines = capture.resolution_log_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert!(line.contains("tool_call_id=missing-1"), "{line}");
    assert!(line.contains("tool=NotARealTool"), "{line}");
    assert!(
        line.contains("reason=Tool not found: NotARealTool"),
        "{line}"
    );
}
include!("tool_dispatch_approval_cases_test.rs");

#[tokio::test]
async fn test_dispatch_emits_fast_completion_before_atomic_batch_commit() {
    struct GatedTool {
        name: &'static str,
        release: Option<Arc<tokio::sync::Notify>>,
    }
    #[async_trait::async_trait]
    impl BaseTool for GatedTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "batch ordering fixture"
        }
        fn parameters(&self) -> serde_json::Value {
            json!({})
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            if let Some(release) = &self.release {
                release.notified().await;
            }
            Ok(self.name.into())
        }
    }
    type HookTrace = Arc<parking_lot::Mutex<Vec<(&'static str, usize)>>>;
    struct Hooks(HookTrace);
    #[async_trait::async_trait]
    impl Middleware for Hooks {
        fn name(&self) -> &str {
            "BatchHooks"
        }
        async fn after_tool(
            &self,
            state: &mut dyn hook_state::AfterToolState,
            _call: &ToolCall,
            _result: &ToolResult,
        ) -> AgentResult<()> {
            self.0.lock().push(("after_tool", state.messages().len()));
            Ok(())
        }
        async fn after_tools_batch(
            &self,
            state: &mut dyn hook_state::AfterToolsBatchState,
            _results: &[(ToolCall, ToolResult)],
        ) -> AgentResult<()> {
            self.0.lock().push(("after_batch", state.messages().len()));
            Ok(())
        }
    }
    let (bus, mut handles) = crate::agent::events_v2::EventBus::new(Default::default());
    let turn = TurnContext::new(Arc::from("/tmp"), Arc::new(CancellationToken::new()));
    let transcript = Arc::new(parking_lot::RwLock::new(MessageTranscript::new()));
    transcript
        .write()
        .append(BaseMessage::human("previous history"));
    let mut ctx = StageContext::builder(turn, Arc::clone(&transcript), MessageQueue::new())
        .with_event_bus(Arc::new(bus))
        .build();
    let release = Arc::new(tokio::sync::Notify::new());
    for (name, gate) in [("Slow", Some(Arc::clone(&release))), ("Fast", None)] {
        ctx.runtime.tools.write().insert(
            name.into(),
            Arc::new(GatedTool {
                name,
                release: gate,
            }),
        );
    }
    let trace: HookTrace = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(Hooks(Arc::clone(&trace))));
    ctx.runtime.middleware_chain = Arc::new(chain);
    let reasoning = Reasoning::with_tools(
        "",
        vec![
            ToolCall::new("slow", "Slow", json!({})),
            ToolCall::new("fast", "Fast", json!({})),
        ],
    );
    let catalog = ctx
        .runtime
        .tool_catalog
        .pin_working_tools(&ctx.runtime.tools.read())
        .expect("the pin working tools fixture result is required for this assertion");
    let cancel = CancellationToken::new();
    let dispatch =
        crate::agent::stages::tool_dispatch::dispatch_tools(&ctx, &reasoning, &catalog, &cancel);
    tokio::pin!(dispatch);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            tokio::select! {
                result = &mut dispatch => panic!("slow tool must still block the batch: {}", result.is_ok()),
                event = handles.render_rx.recv() => {
                    if matches!(event, Some(RenderEvent::ToolEnded { tool_call_id, .. }) if tool_call_id == "fast") { break; }
                }
            }
        }
    }).await.expect("fast completion must be observable while slow invoke waits");
    assert_eq!(
        transcript.read().len(),
        1,
        "no partial batch may become visible"
    );
    assert!(
        trace.lock().is_empty(),
        "after_tool waits for all concurrent invocations"
    );
    release.notify_one();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), dispatch)
        .await
        .expect("the timeout fixture result is required for this assertion")
        .expect("dispatch emits fast completion before atomic batch commit: the completed fixture operation must provide its result payload");
    assert_eq!(
        outcome
            .results
            .iter()
            .map(|(call, _)| call.id.as_str())
            .collect::<Vec<_>>(),
        vec!["slow", "fast"]
    );
    assert_eq!(
        *trace.lock(),
        vec![("after_tool", 1), ("after_tool", 1), ("after_batch", 4)]
    );
    assert_eq!(transcript.read().len(), 4);
}
