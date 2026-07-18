//! 从 tool_dispatch.rs 分离的测试模块
use super::*;
use serde_json::json;

use crate::messages::MessageContent;
use crate::session::queue::MessageQueue;
use crate::session::transcript::MessageTranscript;
use crate::session::turn::TurnContext;

// ── normalize_params ──

#[test]
fn test_normalize_params_path_alias_to_file_path() {
    let input = json!({"path": "/tmp/foo.rs"});
    let out = normalize_params(input);
    assert!(out.get("file_path").is_some());
    assert!(out.get("path").is_none());
}

#[test]
fn test_normalize_params_keep_file_path_when_present() {
    // 当 file_path 已存在时，path 别名不覆盖
    let input = json!({"path": "/a", "file_path": "/b"});
    let out = normalize_params(input);
    assert_eq!(out.get("file_path").unwrap(), &json!("/b"));
    // path 仍然保留（未触发别名替换）
    assert!(out.get("path").is_some());
}

#[test]
fn test_normalize_params_passthrough_non_object() {
    let input = json!("string");
    let out = normalize_params(input.clone());
    assert_eq!(out, input);
}

#[test]
fn test_normalize_params_keep_unrelated_keys() {
    let input = json!({"query": "hello", "limit": 10});
    let out = normalize_params(input);
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

// ── dispatch_concurrent / settle_results / post_process_result / handle_consecutive_failures ──

/// 可返回自定义输出的测试工具
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
    let results = dispatch_concurrent(&ctx, &ready_calls, &all_tools, &cancel, &ai_msg).await;
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
    let results = dispatch_concurrent(&ctx, &ready_calls, &all_tools, &cancel, &ai_msg).await;
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
