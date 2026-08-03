//! 从 tool_dispatch.rs 分离的测试模块
use super::*;
use serde_json::json;

use crate::messages::MessageContent;
use crate::middleware::{r#trait::Middleware, MiddlewareChain};
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

// ── resolve_tool ──

fn make_tools() -> HashMap<String, Arc<dyn BaseTool>> {
    /// 可指定 name 和 aliases 的测试用 ToolStub
    struct NamedToolStub {
        name: &'static str,
        aliases: &'static [&'static str],
    }
    #[async_trait::async_trait]
    impl BaseTool for NamedToolStub {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            ""
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn aliases(&self) -> &[&str] {
            self.aliases
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok(String::new())
        }
    }
    let mut map: HashMap<String, Arc<dyn BaseTool>> = HashMap::new();
    map.insert(
        "Read".to_string(),
        Arc::new(NamedToolStub {
            name: "Read",
            aliases: &["reading"],
        }),
    );
    map.insert(
        "Bash".to_string(),
        Arc::new(NamedToolStub {
            name: "Bash",
            aliases: &["Shell"],
        }),
    );
    map.insert(
        "Agent".to_string(),
        Arc::new(NamedToolStub {
            name: "Agent",
            aliases: &["task"],
        }),
    );
    map
}

#[test]
fn test_resolve_tool_exact_match() {
    let tools = make_tools();
    let tool = resolve_tool("Read", &tools);
    assert!(tool.is_some());
}

#[test]
fn test_resolve_tool_case_insensitive_match() {
    let tools = make_tools();
    let tool = resolve_tool("read", &tools);
    assert!(tool.is_some());
}

#[test]
fn test_resolve_tool_alias_reading() {
    let tools = make_tools();
    // "reading" 通过 Read 工具的 aliases() 解析为 "Read"
    let tool = resolve_tool("reading", &tools);
    assert!(tool.is_some());
}

#[test]
fn test_resolve_tool_alias_task() {
    let tools = make_tools();
    // "task" 通过 Agent 工具的 aliases() 解析为 "Agent"
    let tool = resolve_tool("task", &tools);
    assert!(tool.is_some());
}

#[test]
fn test_resolve_tool_unknown_returns_none() {
    let tools = make_tools();
    let tool = resolve_tool("Unknown", &tools);
    assert!(tool.is_none());
}

#[test]
fn test_resolve_tool_alias_case_insensitive() {
    let tools = make_tools();
    // 工具自声明别名大小写无关：SHELL → Bash (aliases 含 "Shell")
    let tool = resolve_tool("SHELL", &tools);
    assert!(tool.is_some());
}

/// 工具自声明别名（BaseTool::aliases()）应能被 resolve_tool 解析。
#[test]
fn test_resolve_tool_self_declared_alias() {
    struct ToolWithAlias;
    #[async_trait::async_trait]
    impl BaseTool for ToolWithAlias {
        fn name(&self) -> &str {
            "MyTool"
        }
        fn description(&self) -> &str {
            ""
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn aliases(&self) -> &[&str] {
            &["Alternative"]
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok(String::new())
        }
    }
    let mut tools: HashMap<String, Arc<dyn BaseTool>> = HashMap::new();
    let arc: Arc<dyn BaseTool> = Arc::new(ToolWithAlias);
    tools.insert("MyTool".to_string(), arc);

    // 精确匹配仍生效
    let tool = resolve_tool("MyTool", &tools);
    assert!(tool.is_some(), "精确匹配应成功");

    // 自声明别名应能解析
    let tool = resolve_tool("Alternative", &tools);
    assert!(tool.is_some(), "工具自声明别名'Alternative'应能解析");
    assert_eq!(tool.unwrap().name(), "MyTool");

    // 自声明别名大小写无关
    let tool = resolve_tool("ALTERNATIVE", &tools);
    assert!(tool.is_some(), "自声明别名应大小写无关");

    // 未声明的名称不应匹配
    let tool = resolve_tool("Unknown", &tools);
    assert!(tool.is_none(), "未声明名称不应匹配");
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
            _state: &mut dyn crate::middleware::state::MiddlewareState,
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

    let outcome = dispatch_tools(&ctx, &reasoning, &CancellationToken::new())
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

struct OutputTool {
    name: String,
    output: String,
}

#[async_trait::async_trait]
impl BaseTool for OutputTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "test output"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.output.clone())
    }
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
async fn test_dispatch_concurrent_single_tool_succeeds() {
    let ctx = make_test_ctx();
    let tool = std::sync::Arc::new(OutputTool {
        name: "Read".to_string(),
        output: "ok".to_string(),
    });
    let mut all_tools: HashMap<String, std::sync::Arc<dyn BaseTool>> = HashMap::new();
    all_tools.insert("Read".to_string(), tool);
    let cancel = CancellationToken::new();
    let ai_msg = BaseMessage::ai(MessageContent::text("thinking...".to_string()));
    let ready_calls = vec![ToolCall {
        id: "call_1".to_string(),
        name: "Read".to_string(),
        input: serde_json::json!({"file_path": "/tmp/test.txt"}),
    }];
    let mut target_tools: HashMap<String, std::sync::Arc<dyn BaseTool>> = HashMap::new();
    target_tools.insert(
        "call_1".to_string(),
        Arc::clone(all_tools.get("Read").unwrap()),
    );
    let raw_calls = HashMap::new();
    let results = dispatch_concurrent(
        &ctx,
        &ready_calls,
        &raw_calls,
        &target_tools,
        &cancel,
        &ai_msg,
    )
    .await;
    assert_eq!(results.len(), 1);
    assert!(results[0].is_ok(), "工具应成功执行");
    assert_eq!(results[0].as_ref().unwrap(), "ok");
}

#[tokio::test]
async fn test_dispatch_concurrent_cancelled() {
    let ctx = make_test_ctx();
    let tool = std::sync::Arc::new(OutputTool {
        name: "Read".to_string(),
        output: "ok".to_string(),
    });
    let mut all_tools: HashMap<String, std::sync::Arc<dyn BaseTool>> = HashMap::new();
    all_tools.insert("Read".to_string(), tool);
    let cancel = CancellationToken::new();
    cancel.cancel(); // 提前触发取消
    let ai_msg = BaseMessage::ai(MessageContent::text("thinking...".to_string()));
    let ready_calls = vec![ToolCall {
        id: "call_1".to_string(),
        name: "Read".to_string(),
        input: serde_json::json!({}),
    }];
    let mut target_tools: HashMap<String, std::sync::Arc<dyn BaseTool>> = HashMap::new();
    target_tools.insert(
        "call_1".to_string(),
        Arc::clone(all_tools.get("Read").unwrap()),
    );
    let raw_calls = HashMap::new();
    let results = dispatch_concurrent(
        &ctx,
        &ready_calls,
        &raw_calls,
        &target_tools,
        &cancel,
        &ai_msg,
    )
    .await;
    assert_eq!(results.len(), 1);
    assert!(results[0].is_err(), "取消后应返回错误");
    let err = results[0].as_ref().unwrap_err().to_string();
    assert!(
        err.contains("interrupted by user"),
        "错误信息应包含取消描述，实际: {err}"
    );
}

#[tokio::test]
async fn test_settle_results_mixed_ready_settled() {
    let ctx = make_test_ctx();
    let approval = ApprovalOutcome {
        ready_calls: vec![ToolCall {
            id: "call_ready".to_string(),
            name: "Read".to_string(),
            input: serde_json::json!({}),
        }],
        settled_results: vec![(
            ToolCall {
                id: "call_rejected".to_string(),
                name: "Bash".to_string(),
                input: serde_json::json!({}),
            },
            ToolResult::error("call_rejected", "Bash", "HITL rejected"),
        )],
    };
    let tool_results: Vec<Result<String, AgentError>> = vec![Ok("success output".to_string())];
    let all_tools: HashMap<String, std::sync::Arc<dyn BaseTool>> = HashMap::new();
    let outcome = settle_results(&ctx, approval, tool_results, false, &all_tools).await;
    // ready + settled = 2 条
    assert_eq!(outcome.results.len(), 2, "应合并 ready 和 settled 结果");
    // settled 在前，ready 在后
    assert!(outcome.results[0].1.is_error, "rejected 应是错误");
    assert!(!outcome.results[1].1.is_error, "ready 工具应成功");
    assert_eq!(outcome.results[1].1.output, "success output");
}

#[test]
fn test_post_process_result_no_registry() {
    let ctx = make_test_ctx();
    let call = ToolCall {
        id: "call_1".to_string(),
        name: "Read".to_string(),
        input: serde_json::json!({"file_path": "/tmp/x"}),
    };
    let mut result = ToolResult::error("call_1", "Read", "ENOENT: file not found");
    let all_tools: HashMap<String, std::sync::Arc<dyn BaseTool>> = HashMap::new();
    let output_before = result.output.clone();
    // error_suggest_registry 为 None（默认），不应修改 output
    post_process_result(&ctx, &call, &mut result, &all_tools);
    assert_eq!(
        result.output, output_before,
        "无 registry 时 output 不应变化，实际: {}",
        result.output
    );
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
