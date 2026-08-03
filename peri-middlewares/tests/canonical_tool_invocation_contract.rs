use std::{collections::BTreeMap, sync::Arc};

#[cfg(unix)]
use std::{collections::HashMap, path::PathBuf};

use async_trait::async_trait;
use parking_lot::{Mutex, RwLock};
use peri_agent::{
    agent::{
        react::{Reasoning, ToolCall},
        stages::{tool_dispatch::dispatch_tools, SharedToolMap, StageContext},
    },
    middleware::{r#trait::Middleware, state::MiddlewareState, MiddlewareChain},
    session::{FrozenContext, Session},
    tools::{BaseTool, ToolContext},
};
#[cfg(unix)]
use peri_middlewares::hooks::{HookEvent, HookMiddleware, HookType, RegisteredHook};
use peri_middlewares::{
    hitl::{
        default_requires_approval, HumanInTheLoopMiddleware, PermissionMode, SharedPermissionMode,
    },
    ExecuteExtraToolResolver, EXECUTE_EXTRA_TOOL_NAME,
};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

struct RecordingTool {
    name: &'static str,
    aliases: &'static [&'static str],
    calls: Arc<Mutex<Vec<Value>>>,
    schema: Value,
}

#[async_trait]
impl BaseTool for RecordingTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "contract-test tool"
    }

    fn parameters(&self) -> Value {
        self.schema.clone()
    }

    fn aliases(&self) -> &[&str] {
        self.aliases
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.calls.lock().push(input);
        Ok(self.name.to_string())
    }
}

struct PolicyRecorder(Arc<Mutex<Vec<ToolCall>>>);

#[async_trait]
impl Middleware for PolicyRecorder {
    fn name(&self) -> &str {
        "PolicyRecorder"
    }

    async fn before_tools_batch(
        &self,
        _state: &mut dyn MiddlewareState,
        calls: &[ToolCall],
    ) -> Vec<peri_agent::error::AgentResult<ToolCall>> {
        self.0.lock().extend_from_slice(calls);
        calls.iter().cloned().map(Ok).collect()
    }
}

fn make_context(
    tools: BTreeMap<String, Arc<dyn BaseTool>>,
    chain: MiddlewareChain,
) -> (StageContext, peri_agent::agent::events_v2::EventHandles) {
    let session = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None);
    let turn = session.start_turn();
    let (event_bus, handles) = peri_agent::agent::events_v2::EventBus::new(Default::default());
    let shared: SharedToolMap = Arc::new(RwLock::new(tools));
    let context = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_tools(shared)
        .with_tool_invocation_resolver(Arc::new(ExecuteExtraToolResolver::default()))
        .with_middleware_chain(Arc::new(chain))
        .with_event_bus(Arc::new(event_bus))
        .build();
    (context, handles)
}

#[tokio::test]
async fn wrapper_policy_is_canonical_while_event_and_transcript_stay_raw() {
    let invoked = Arc::new(Mutex::new(Vec::new()));
    let policy = Arc::new(Mutex::new(Vec::new()));
    let write: Arc<dyn BaseTool> = Arc::new(RecordingTool {
        name: "Write",
        aliases: &["Save"],
        calls: Arc::clone(&invoked),
        schema: json!({
            "type": "object",
            "properties": {"file_path": {"type": "string"}}
        }),
    });
    let shared = Arc::new(RwLock::new(BTreeMap::from([(
        "Write".to_string(),
        Arc::clone(&write),
    )])));
    let wrapper: Arc<dyn BaseTool> = Arc::new(
        peri_middlewares::tool_search::ExecuteExtraTool::new(Arc::clone(&shared)),
    );
    let mut tools = shared.read().clone();
    tools.insert(EXECUTE_EXTRA_TOOL_NAME.to_string(), wrapper);
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(PolicyRecorder(Arc::clone(&policy))));
    let (context, mut events) = make_context(tools, chain);
    let reasoning = Reasoning::with_tools(
        "",
        vec![ToolCall::new(
            "call-1",
            EXECUTE_EXTRA_TOOL_NAME,
            json!({"tool_name": "SAVE", "params": {"path": "/tmp/a"}}),
        )],
    );

    let outcome = dispatch_tools(&context, &reasoning, &CancellationToken::new())
        .await
        .unwrap();

    {
        let policy_calls = policy.lock();
        assert_eq!(policy_calls.len(), 1);
        assert_eq!(policy_calls[0].id, "call-1");
        assert_eq!(policy_calls[0].name, "Write");
        assert_eq!(policy_calls[0].input, json!({"file_path": "/tmp/a"}));
    }
    assert_eq!(*invoked.lock(), vec![json!({"file_path": "/tmp/a"})]);
    assert_eq!(outcome.results[0].1.tool_name, "Write");
    let started = events.render_rx.recv().await.unwrap();
    match started {
        peri_agent::agent::events_v2::RenderEvent::ToolStarted { name, input, .. } => {
            assert_eq!(name, EXECUTE_EXTRA_TOOL_NAME);
            assert_eq!(input["tool_name"], "SAVE");
        }
        event => panic!("expected raw ToolStarted, got {event:?}"),
    }
}

#[tokio::test]
async fn grep_path_parameter_is_preserved_not_renamed_to_file_path() {
    // 回归：Grep/Glob 的 schema 参数名就是 path，归一化不得将其重命名为
    // file_path（曾导致 path 丢失、搜索静默回退全仓库）。
    let invoked = Arc::new(Mutex::new(Vec::new()));
    let policy = Arc::new(Mutex::new(Vec::new()));
    let grep: Arc<dyn BaseTool> = Arc::new(RecordingTool {
        name: "Grep",
        aliases: &[],
        calls: Arc::clone(&invoked),
        schema: json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string"}
            },
            "required": ["pattern"]
        }),
    });
    let shared = Arc::new(RwLock::new(BTreeMap::from([(
        "Grep".to_string(),
        Arc::clone(&grep),
    )])));
    let wrapper: Arc<dyn BaseTool> = Arc::new(
        peri_middlewares::tool_search::ExecuteExtraTool::new(Arc::clone(&shared)),
    );
    let mut tools = shared.read().clone();
    tools.insert(EXECUTE_EXTRA_TOOL_NAME.to_string(), wrapper);
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(PolicyRecorder(Arc::clone(&policy))));
    let (context, _) = make_context(tools, chain);

    // 1) ExecuteExtraTool wrapper 路径
    let reasoning = Reasoning::with_tools(
        "",
        vec![ToolCall::new(
            "call-grep-1",
            EXECUTE_EXTRA_TOOL_NAME,
            json!({"tool_name": "Grep", "params": {"pattern": "tokio|serde", "path": "/tmp/a"}}),
        )],
    );
    let outcome = dispatch_tools(&context, &reasoning, &CancellationToken::new())
        .await
        .unwrap();
    assert!(!outcome.results[0].1.is_error);

    // 2) 直接调用路径（tool_dispatch 本地归一化）
    let reasoning = Reasoning::with_tools(
        "",
        vec![ToolCall::new(
            "call-grep-2",
            "Grep",
            json!({"pattern": "tokio", "path": "/tmp/a"}),
        )],
    );
    let outcome = dispatch_tools(&context, &reasoning, &CancellationToken::new())
        .await
        .unwrap();
    assert!(!outcome.results[0].1.is_error);

    // policy 与执行 input 都保留 path，不重命名为 file_path
    let expected = vec![
        json!({"pattern": "tokio|serde", "path": "/tmp/a"}),
        json!({"pattern": "tokio", "path": "/tmp/a"}),
    ];
    let policy_calls = policy.lock();
    assert_eq!(
        policy_calls
            .iter()
            .map(|c| c.input.clone())
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(*invoked.lock(), expected);
}

#[tokio::test]
async fn malformed_unknown_and_ambiguous_wrapper_targets_have_no_policy_or_invoke_side_effects() {
    let invoked = Arc::new(Mutex::new(Vec::new()));
    let policy = Arc::new(Mutex::new(Vec::new()));
    let target: Arc<dyn BaseTool> = Arc::new(RecordingTool {
        name: "Bash",
        aliases: &["Shell"],
        calls: Arc::clone(&invoked),
        schema: json!({}),
    });
    let conflicting: Arc<dyn BaseTool> = Arc::new(RecordingTool {
        name: "OtherBash",
        aliases: &["Shell"],
        calls: Arc::clone(&invoked),
        schema: json!({}),
    });
    let shared = Arc::new(RwLock::new(BTreeMap::from([
        ("Bash".to_string(), target),
        ("OtherBash".to_string(), conflicting),
    ])));
    let wrapper: Arc<dyn BaseTool> = Arc::new(
        peri_middlewares::tool_search::ExecuteExtraTool::new(Arc::clone(&shared)),
    );
    let mut tools = shared.read().clone();
    tools.insert(EXECUTE_EXTRA_TOOL_NAME.to_string(), wrapper);
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(PolicyRecorder(Arc::clone(&policy))));
    let (context, mut events) = make_context(tools, chain);
    let reasoning = Reasoning::with_tools(
        "",
        vec![
            ToolCall::new("malformed", EXECUTE_EXTRA_TOOL_NAME, json!({"params": {}})),
            ToolCall::new(
                "unknown",
                EXECUTE_EXTRA_TOOL_NAME,
                json!({"tool_name": "missing", "params": {}}),
            ),
            ToolCall::new(
                "ambiguous",
                EXECUTE_EXTRA_TOOL_NAME,
                json!({"tool_name": "shell", "params": {}}),
            ),
        ],
    );

    let outcome = dispatch_tools(&context, &reasoning, &CancellationToken::new())
        .await
        .unwrap();

    assert!(policy.lock().is_empty());
    assert!(invoked.lock().is_empty());
    assert!(outcome.results.iter().all(|(_, result)| result.is_error));
    assert!(events.render_rx.try_recv().is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn hook_receives_canonical_alias_identity() {
    let invoked = Arc::new(Mutex::new(Vec::new()));
    let bash: Arc<dyn BaseTool> = Arc::new(RecordingTool {
        name: "Bash",
        aliases: &["Shell"],
        calls: Arc::clone(&invoked),
        schema: json!({}),
    });
    let hook = RegisteredHook {
        hook: serde_json::from_value::<HookType>(json!({
            "type": "command",
            "command": "exit 2"
        }))
        .unwrap(),
        event: HookEvent::PreToolUse,
        matcher: Some("Bash".to_string()),
        plugin_name: "contract".to_string(),
        plugin_id: "contract".to_string(),
        plugin_root: PathBuf::from("/tmp"),
        plugin_data_dir: PathBuf::from("/tmp"),
        plugin_options: HashMap::new(),
    };
    let mode = SharedPermissionMode::new(PermissionMode::Bypass);
    let hook_middleware = HookMiddleware::new(
        vec![hook],
        Arc::new(|| unreachable!("command hook needs no LLM")),
        "/tmp",
        "contract",
        "/tmp/transcript",
        mode,
        "test",
    );
    let mut tools = BTreeMap::new();
    tools.insert("Bash".to_string(), bash);
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(hook_middleware));
    let (context, _) = make_context(tools, chain);
    let reasoning = Reasoning::with_tools(
        "",
        vec![ToolCall::new("shell", "SHELL", json!({"command": "true"}))],
    );

    let outcome = dispatch_tools(&context, &reasoning, &CancellationToken::new())
        .await
        .unwrap();

    assert!(outcome.results[0].1.is_error);
    assert!(invoked.lock().is_empty());
}

#[tokio::test]
async fn accept_edit_applies_to_wrapper_write_and_edit_but_not_bash_or_mcp() {
    let policy = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut tools = BTreeMap::new();
    for (name, aliases) in [
        ("Write", &["Save"][..]),
        ("Edit", &[][..]),
        ("Bash", &["Shell"][..]),
        ("mcp__server__tool", &[][..]),
    ] {
        tools.insert(
            name.to_string(),
            Arc::new(RecordingTool {
                name,
                aliases,
                calls: Arc::clone(&calls),
                schema: json!({}),
            }) as Arc<dyn BaseTool>,
        );
    }
    let shared = Arc::new(RwLock::new(tools.clone()));
    tools.insert(
        EXECUTE_EXTRA_TOOL_NAME.to_string(),
        Arc::new(peri_middlewares::tool_search::ExecuteExtraTool::new(shared)),
    );
    let mut chain = MiddlewareChain::new();
    chain.add(Box::new(PolicyRecorder(Arc::clone(&policy))));
    chain.add(Box::new(HumanInTheLoopMiddleware::with_shared_mode(
        Arc::new(RejectingBroker),
        default_requires_approval,
        SharedPermissionMode::new(PermissionMode::AcceptEdit),
        None,
    )));
    let (context, _) = make_context(tools, chain);
    let reasoning = Reasoning::with_tools(
        "",
        vec![
            ToolCall::new(
                "write",
                EXECUTE_EXTRA_TOOL_NAME,
                json!({"tool_name": "save", "params": {}}),
            ),
            ToolCall::new(
                "edit",
                EXECUTE_EXTRA_TOOL_NAME,
                json!({"tool_name": "Edit", "params": {}}),
            ),
            ToolCall::new(
                "bash",
                EXECUTE_EXTRA_TOOL_NAME,
                json!({"tool_name": "SHELL", "params": {}}),
            ),
            ToolCall::new(
                "mcp",
                EXECUTE_EXTRA_TOOL_NAME,
                json!({"tool_name": "mcp__server__tool", "params": {}}),
            ),
        ],
    );

    let outcome = dispatch_tools(&context, &reasoning, &CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(
        policy
            .lock()
            .iter()
            .map(|call| call.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Write", "Edit", "Bash", "mcp__server__tool"]
    );
    assert_eq!(calls.lock().len(), 2);
    let result_by_id: std::collections::HashMap<_, _> = outcome
        .results
        .iter()
        .map(|(_, result)| (result.tool_call_id.as_str(), result))
        .collect();
    assert!(!result_by_id["write"].is_error && !result_by_id["edit"].is_error);
    assert!(result_by_id["bash"].is_error && result_by_id["mcp"].is_error);
}

struct RejectingBroker;

#[async_trait]
impl peri_agent::interaction::UserInteractionBroker for RejectingBroker {
    async fn request(
        &self,
        context: peri_agent::interaction::InteractionContext,
    ) -> peri_agent::interaction::InteractionResponse {
        let peri_agent::interaction::InteractionContext::Approval { items } = context else {
            unreachable!();
        };
        peri_agent::interaction::InteractionResponse::Decisions(
            items
                .into_iter()
                .map(|_| peri_agent::interaction::ApprovalDecision::Reject {
                    reason: "rejected by contract broker".to_string(),
                    source: None,
                })
                .collect(),
        )
    }
}
