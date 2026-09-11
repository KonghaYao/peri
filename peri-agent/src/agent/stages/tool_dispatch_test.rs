//! 从 tool_dispatch.rs 分离的测试模块
use crate::middleware::capabilities as hook_state;
use std::collections::BTreeMap;

use super::*;
use serde_json::json;

use crate::middleware::{r#trait::Middleware, MiddlewareChain};
use crate::session::queue::MessageQueue;
use crate::session::transcript::MessageTranscript;
use crate::session::turn::TurnContext;
use crate::tools::normalize_params;

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
    let out = normalize_params(input, Some(&read_schema_tool()));
    assert!(out.get("file_path").is_some());
    assert!(out.get("path").is_none());
}

#[test]
fn test_normalize_params_keep_file_path_when_present() {
    // 当 file_path 已存在时，path 别名不覆盖
    let input = json!({"path": "/a", "file_path": "/b"});
    let out = normalize_params(input, Some(&read_schema_tool()));
    assert_eq!(out.get("file_path").unwrap(), &json!("/b"));
    // path 仍然保留（未触发别名替换）
    assert!(out.get("path").is_some());
}

#[test]
fn test_normalize_params_does_not_rename_path_for_path_schema_tools() {
    // 回归：Grep/Glob 的 schema 参数名就是 path，不得重命名为 file_path
    // （曾导致 path 丢失、搜索静默回退全仓库）
    let input = json!({"pattern": "tokio|serde", "path": "/tmp/a"});
    let out = normalize_params(input, Some(&grep_schema_tool()));
    assert_eq!(out.get("path").unwrap(), &json!("/tmp/a"));
    assert!(out.get("file_path").is_none());
}

#[test]
fn test_normalize_params_passthrough_non_object() {
    let input = json!("string");
    let out = normalize_params(input.clone(), None);
    assert_eq!(out, input);
}

#[test]
fn test_normalize_params_keep_unrelated_keys() {
    let input = json!({"query": "hello", "limit": 10});
    let out = normalize_params(input, None);
    assert_eq!(out.get("query").unwrap(), &json!("hello"));
    assert_eq!(out.get("limit").unwrap(), &json!(10));
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
        .unwrap();
    let outcome = dispatch_tools(&ctx, &reasoning, &catalog, &CancellationToken::new())
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
    handle_consecutive_failures(&ctx, &[(ok_call, ok_result)]);
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

    let reasoning = Reasoning::with_tools(
        "",
        vec![ToolCall::new("missing-1", "NotARealTool", json!({}))],
    );
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
        .unwrap();
    let outcome = dispatch_tools(&ctx, &reasoning, &catalog, &CancellationToken::new())
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
                tool_call_id, name, ..
            } => {
                assert_eq!(tool_call_id, "missing-1");
                assert_eq!(name, "NotARealTool");
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
/// Both properties are real API fields: approval must describe the same input
/// that the target receives, including any approved change to `path`.
async fn assert_dual_path_schema_survives_dispatch(approved_path: Option<&str>, wrapped: bool) {
    type InputTrace = Arc<parking_lot::Mutex<Vec<(&'static str, serde_json::Value)>>>;
    struct DualPathTool(InputTrace);

    #[async_trait::async_trait]
    impl BaseTool for DualPathTool {
        fn name(&self) -> &str {
            "DualPath"
        }
        fn description(&self) -> &str {
            "Uses path and file_path as distinct optional fields"
        }
        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object", "properties": {
                "path": {"type": "string"},
                "file_path": {"type": "string"}
            }})
        }
        async fn invoke(
            &self,
            input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            self.0.lock().push(("invoke", input));
            Ok("executed".into())
        }
    }

    // Use the production binding seam: policy/events see the target, while the
    // transcript must retain the model's original wrapper request for pairing.
    struct Wrapper(Arc<dyn BaseTool>);
    #[async_trait::async_trait]
    impl BaseTool for Wrapper {
        fn name(&self) -> &str {
            "ExecuteExtraTool"
        }
        fn description(&self) -> &str {
            "binds the fixture's canonical target"
        }
        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object", "properties": {
                "tool_name": {"type": "string"}, "params": {"type": "object"}
            }})
        }
        fn bind_invocation(
            &self,
            input: serde_json::Value,
        ) -> Result<
            Option<crate::tools::BoundToolInvocation>,
            Box<dyn std::error::Error + Send + Sync>,
        > {
            Ok(Some(crate::tools::BoundToolInvocation {
                policy_name: self.0.name().into(),
                policy_input: input["params"].clone(),
                target: Arc::clone(&self.0),
            }))
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            panic!("dispatch must invoke the bound target")
        }
    }

    struct ApprovePath {
        trace: InputTrace,
        replacement: Option<String>,
    }
    #[async_trait::async_trait]
    impl Middleware for ApprovePath {
        fn name(&self) -> &str {
            "ApprovePath"
        }
        async fn before_tools_batch(
            &self,
            _state: &mut dyn hook_state::BeforeToolState,
            calls: &[ToolCall],
        ) -> Vec<crate::error::AgentResult<ToolCall>> {
            calls
                .iter()
                .cloned()
                .map(|mut call| {
                    self.trace.lock().push(("approval", call.input.clone()));
                    if let Some(path) = &self.replacement {
                        call.input["path"] = json!(path);
                    }
                    Ok(call)
                })
                .collect()
        }
    }

    let trace: InputTrace = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut ctx = make_test_ctx();
    let (bus, mut events) = crate::agent::events_v2::EventBus::new(Default::default());
    ctx.runtime.event_bus = Arc::new(bus);
    let target: Arc<dyn BaseTool> = Arc::new(DualPathTool(Arc::clone(&trace)));
    ctx.runtime.tools.write().insert(
        if wrapped {
            "ExecuteExtraTool"
        } else {
            "DualPath"
        }
        .into(),
        if wrapped {
            Arc::new(Wrapper(target))
        } else {
            target
        },
    );
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(ApprovePath {
        trace: Arc::clone(&trace),
        replacement: approved_path.map(str::to_owned),
    }));
    ctx.runtime.middleware_chain = Arc::new(chain);
    let raw_call = ToolCall::new(
        "dual-path",
        if wrapped {
            "ExecuteExtraTool"
        } else {
            "DualPath"
        },
        if wrapped {
            json!({"tool_name": "DualPath", "params": {"path": "requested/root"}})
        } else {
            json!({"path": "requested/root"})
        },
    );
    let reasoning = Reasoning::with_tools("", vec![raw_call.clone()]);
    let catalog = ctx
        .runtime
        .tool_catalog
        .pin_working_tools(&ctx.runtime.tools.read())
        .unwrap();
    let outcome = dispatch_tools(&ctx, &reasoning, &catalog, &CancellationToken::new())
        .await
        .expect("valid declared input must dispatch");
    assert_eq!(outcome.results.len(), 1);
    assert!(!outcome.results[0].1.is_error);
    assert_eq!(
        *trace.lock(),
        vec![
            ("approval", json!({"path": "requested/root"})),
            (
                "invoke",
                json!({"path": approved_path.unwrap_or("requested/root")})
            ),
        ],
        "dispatch must not reinterpret a declared field after policy approval"
    );
    let transcript = ctx.session.transcript.read();
    let messages = transcript.visible_messages();
    let raw = &messages[0].tool_calls()[0];
    assert_eq!(raw.id, raw_call.id);
    assert_eq!(raw.name, raw_call.name);
    assert_eq!(
        raw.arguments, raw_call.input,
        "approval must not rewrite the model's source call"
    );
    assert_eq!(outcome.results[0].1.tool_call_id, raw_call.id);
    let starts: Vec<_> = std::iter::from_fn(|| events.try_render())
        .filter_map(|event| match event {
            RenderEvent::ToolStarted {
                tool_call_id,
                name,
                input,
                ..
            } => Some((tool_call_id, name, input)),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts,
        vec![(
            raw_call.id,
            "DualPath".into(),
            json!({"path": approved_path.unwrap_or("requested/root")})
        )],
        "ToolStarted/toolcard must describe the same approved target input that was invoked"
    );
}

#[tokio::test]
async fn test_dispatch_preserves_dual_path_schema_after_approval() {
    assert_dual_path_schema_survives_dispatch(None, false).await;
}

#[tokio::test]
async fn test_dispatch_preserves_approved_replacement_of_declared_path() {
    assert_dual_path_schema_survives_dispatch(Some("approved/root"), false).await;
}

#[tokio::test]
async fn test_approved_wrapper_started_uses_edited_target_input() {
    assert_dual_path_schema_survives_dispatch(Some("approved/root"), true).await;
}

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
            state: &mut dyn hook_state::StateView,
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
        .unwrap();
    let cancel = CancellationToken::new();
    let dispatch = dispatch_tools(&ctx, &reasoning, &catalog, &cancel);
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
        .unwrap()
        .unwrap();
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
