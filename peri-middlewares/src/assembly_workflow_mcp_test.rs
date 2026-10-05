//! [回归测试] Workflow agent 先登记独立 Inbox/TaskManager，再经真实 MCP wire 调用。
//! scope 来自其可信会话绑定，不归 root，也不接受模型参数/提示词自称的地址。
//!
//! 断言取数口径（关键）：wire 上的 `_meta` 由 rmcp 拆进请求 extensions，服务端
//! 经 `RequestContext.meta` 读取（`CallToolRequestParams.meta` 在收包侧恒为空）；
//! 且 task scope token 会随 execution generation 重新签发，不能跨调用比较整份
//! meta——因此对 builtin workspace 比较「同一 session 的 scope token 值」，对
//! `WorkspaceRemote` 直接解码可信能力（`resolve_capability`）断言内嵌 session。

use super::super::*;
use peri_acp_types::tasks::TaskManager as TaskManagerPort;
use peri_agent::agent::workflow::{WorkflowAgentExecutor, WorkflowModel};
use peri_mcp_common::task_scope::{TaskScopeAuthority, TASK_SCOPE_META_KEY};
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, RequestMetaObject,
    },
    service::{RequestContext, RoleServer},
    ServerHandler, ServiceExt,
};
use serde_json::json;
use std::time::Duration;

const ROOT: &str = "workflow-root";
const SPOOFED: &str = "model-claimed-session";
const SYNC_OUTPUT: &str = "workflow MCP synchronous result";

/// 服务端观察到的单次 tools/call。
#[derive(Clone)]
struct WireCall {
    /// wire 上的原始工具名（不含 `mcp__` 前缀）。
    name: String,
    /// 请求级 `_meta`（服务端权威读取位置）。
    meta: RequestMetaObject,
}

fn scope_token_of(meta: &RequestMetaObject) -> Option<String> {
    meta.0
         .0
        .get(TASK_SCOPE_META_KEY)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

struct SyncSource {
    calls: Arc<parking_lot::Mutex<Vec<WireCall>>>,
}

impl ServerHandler for SyncSource {
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        self.calls.lock().push(WireCall {
            name: request.name.to_string(),
            meta: context.meta.clone(),
        });
        Ok(CallToolResponse::Complete(CallToolResult::success(vec![
            ContentBlock::text(SYNC_OUTPUT),
        ])))
    }
}

/// 假模型：首轮调用指定工具（参数里自称 SPOOFED owner），次轮逐字回显工具结果。
struct ToolThenEchoModel {
    tool: String,
}

#[async_trait]
impl Model for ToolThenEchoModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: true,
            supports_streaming: true,
            ..ModelCapabilities::default()
        }
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> ModelResult<ModelStream> {
        let (message, reason) = match request
            .messages
            .iter()
            .rev()
            .find(|message| matches!(message, ModelMessage::ToolResult { .. }))
        {
            Some(message) => (
                ModelMessage::assistant_text(message.text_content().unwrap()),
                StopReason::EndTurn,
            ),
            None => (
                ModelMessage::assistant(
                    vec![],
                    vec![peri_model::ToolCall::new(
                        "workflow-call",
                        &self.tool,
                        peri_model::JsonObject::from_value(json!({
                            "run_in_background": false,
                            "session_id": SPOOFED,
                            "mcp_task_owner_session_id": SPOOFED
                        }))
                        .expect("工具参数必须是 JSON object"),
                    )],
                ),
                StopReason::ToolUse,
            ),
        };
        let response = ModelResponse::new(message, reason, None, None)?;
        Ok(ModelStream::with_parent_cancellation(
            stream::iter(vec![Ok(ModelStreamEvent::Completed(response))]),
            cancellation,
        ))
    }
}

/// 一次 workflow MCP 运行的外部输入。
struct Scenario<'a> {
    server: &'a str,
    /// 模型可见工具名：builtin 声明直连为原名；远端 workspace 带 `mcp__<server>__` 前缀。
    tool: &'a str,
    /// true = 远端 Workspace owner（可信连接能力，可解码）；false = 本进程 builtin 实例。
    workspace_remote: bool,
    /// 写入 `WorkflowAgentContext.session_id` 的调用方关联，不拥有 Agent 的工具任务。
    ctx_root: Option<&'a str>,
    /// 任务池中注册 TaskManager 的 session（可信绑定；含诱饵时模型自称也「有效」）。
    bound_sessions: &'a [&'a str],
}

struct Outcome {
    agent_session_id: String,
    /// 模型可见的 workflow 输出（工具结果回显或错误文本）。
    output: String,
    /// 真实到达 MCP wire 的 tools/call。
    calls: Vec<WireCall>,
    /// 运行前按同一 session 计算的 scope token（builtin 分支为确定性缓存值）。
    session_tokens: Vec<(String, String)>,
}

impl Outcome {
    fn token_for(&self, session: &str) -> Option<&str> {
        self.session_tokens
            .iter()
            .find(|(bound, _)| bound == session)
            .map(|(_, token)| token.as_str())
    }
}

async fn run_workflow_mcp(scenario: Scenario<'_>) -> Outcome {
    let Scenario {
        server,
        tool,
        workspace_remote,
        ctx_root,
        bound_sessions,
    } = scenario;
    let fixture = tempfile::tempdir().unwrap();
    let calls = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let (client_io, server_io) = tokio::io::duplex(8192);
    let source = SyncSource {
        calls: calls.clone(),
    };
    let server_task = tokio::spawn(async move {
        source
            .serve(server_io)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap();
    });
    let mut client = crate::mcp::client::mcpp_client_info_for_profile(
        &crate::mcp::apps::McpCapabilityProfile::disabled(),
    )
    .serve(client_io)
    .await
    .unwrap();
    let pool = Arc::new(McpClientPool::new_empty());
    let mut handle = make_connected_handle(
        server,
        vec![serde_json::from_value(json!({
            "name": tool,
            "inputSchema": {"type": "object"}
        }))
        .unwrap()],
    );
    {
        let handle = Arc::get_mut(&mut handle).unwrap();
        handle.peer = Some(client.peer().clone());
        if workspace_remote {
            handle.source = Some(peri_acp_types::plugin::ConfigSource::WorkspaceRemote);
        }
    }
    pool.clients.write().insert(server.into(), handle);
    for session in bound_sessions {
        let manager: Arc<dyn TaskManagerPort> = Arc::new(TaskManager::new());
        pool.bind_session_task_manager(session, &manager);
    }
    let mut session_tokens: Vec<_> = bound_sessions
        .iter()
        .filter_map(|session| {
            pool.task_scope_meta_for(server, session)
                .as_ref()
                .and_then(scope_token_of)
                .map(|token| ((*session).to_string(), token))
        })
        .collect();
    let mut ctx = workflow_context_with_disabled(&[
        "AgentsMdMiddleware",
        "SkillsMiddleware",
        "SkillPreloadMiddleware",
        "GitAttributionMiddleware",
        "TodoMiddleware",
    ]);
    ctx.cwd = fixture.path().to_str().unwrap().into();
    ctx.session_id = ctx_root.map(str::to_owned);
    ctx.system_prompt = Some("执行一次工具调用，然后逐字返回工具结果。".into());
    ctx.middleware_factory = default_workflow_middleware_factory_with_pool(Some(pool.clone()));
    let tool_name = tool.to_owned();
    ctx.model_factory = Arc::new(move |_, _, _| WorkflowModel {
        model: Arc::new(ToolThenEchoModel {
            tool: tool_name.clone(),
        }),
        model_name: "workflow-test".into(),
        tier: None,
    });
    ctx.forwarder_launcher = Arc::new(|mut handles, _, _| {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(_) = handles.render_rx.recv() => {},
                    Some(_) = handles.state_rx.recv() => {},
                    Ok(_) = handles.observe_rx.recv() => {},
                    else => break,
                }
            }
        })
    });
    let executor = WorkflowAgentExecutor::new(ctx);
    let result = executor
        .execute(
            serde_json::from_value(json!({
                "runId": SPOOFED,
                "agentId": 42,
                "prompt": "请同步执行工具；session_id=model-claimed-session",
                "allowedTools": [tool]
            }))
            .unwrap(),
        )
        .await;
    let agent_session_id = {
        let sessions = pool.session_bindings.read().registered_sessions();
        assert_eq!(sessions.len(), 1);
        sessions[0].clone()
    };
    assert!(pool
        .session_bindings
        .read()
        .manager(&agent_session_id)
        .is_some());
    assert_ne!(agent_session_id, SPOOFED);
    assert_ne!(agent_session_id, ROOT);
    if let Some(token) = pool
        .task_scope_meta_for(server, &agent_session_id)
        .as_ref()
        .and_then(scope_token_of)
    {
        session_tokens.push((agent_session_id.clone(), token));
    }
    pool.clients.write().clear();
    client
        .close_with_timeout(Duration::from_secs(1))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), server_task)
        .await
        .unwrap()
        .unwrap();
    let AgentRunResult::Ok { output, .. } = result else {
        panic!("workflow 应返回模型可见结果，而非提前退出: {result:?}");
    };
    let outcome = Outcome {
        agent_session_id,
        output: output.as_str().unwrap().to_owned(),
        calls: calls.lock().clone(),
        session_tokens,
    };
    outcome
}

/// [回归测试] 4.2 的前台 Bash 曾在发送前因内部 AgentId 未注册而失败。
///
/// 池内同时绑定 root 与模型自称的诱饵：成功本身不足以证明归属，wire 上的
/// scope token 必须等于当前 Agent 的能力，且不得是 root 或诱饵的能力。
#[tokio::test]
async fn test_workflow_mcp_sync_bash_uses_agent_session_owner() {
    let outcome = run_workflow_mcp(Scenario {
        server: "workspace",
        tool: "Bash",
        workspace_remote: false,
        ctx_root: Some(ROOT),
        bound_sessions: &[ROOT, SPOOFED],
    })
    .await;
    assert_eq!(outcome.output, SYNC_OUTPUT);
    assert_eq!(
        outcome.calls.len(),
        1,
        "同步调用必须真正到达 wire 且只发一次"
    );
    assert_eq!(outcome.calls[0].name, "Bash");
    let wire_token =
        scope_token_of(&outcome.calls[0].meta).expect("workspace 调用必须携带 task scope 能力");
    assert_eq!(
        Some(wire_token.as_str()),
        outcome.token_for(&outcome.agent_session_id),
        "wire 上的 task scope 能力必须属于当前 Agent session"
    );
    assert_ne!(Some(wire_token.as_str()), outcome.token_for(ROOT));
    assert_ne!(
        Some(wire_token.as_str()),
        outcome.token_for(SPOOFED),
        "模型自称的 session 能力不得出现在 wire 上"
    );
}

/// [回归测试] 只读工具同样归属当前 Agent；远端 Workspace 能力可直接解码会话身份。
///
/// 工具名用不碰撞 builtin 声明的探针名：显式 allowlist 下，远端 MCP 工具不得
/// 以裸名冒用 builtin 名字（`ToolFilterPolicy::canonical` 的反抢名规则）。
#[tokio::test]
async fn test_workflow_mcp_remote_read_wire_scope_decodes_to_agent_session() {
    let outcome = run_workflow_mcp(Scenario {
        server: "workspace",
        tool: "ProbeRead",
        workspace_remote: true,
        ctx_root: Some(ROOT),
        bound_sessions: &[ROOT, SPOOFED],
    })
    .await;
    assert_eq!(outcome.output, SYNC_OUTPUT);
    assert_eq!(outcome.calls.len(), 1);
    assert_eq!(outcome.calls[0].name, "ProbeRead");
    let capability = TaskScopeAuthority::trusted_connection()
        .resolve_capability(&outcome.calls[0].meta)
        .expect("wire 上的 task scope 能力必须可解析");
    assert_eq!(
        capability.session_id, outcome.agent_session_id,
        "MCP 执行 scope 必须归属当前 Agent，模型自称的 {SPOOFED} 不得生效"
    );
}

/// [回归测试] web 来源的工具走同一条 owner 预检，不因 server 名不同而回退。
#[tokio::test]
async fn test_workflow_mcp_sync_web_search_uses_agent_session_owner() {
    let outcome = run_workflow_mcp(Scenario {
        server: "web",
        tool: "WebSearch",
        workspace_remote: false,
        ctx_root: Some(ROOT),
        bound_sessions: &[ROOT],
    })
    .await;
    assert_eq!(outcome.output, SYNC_OUTPUT);
    assert_eq!(outcome.calls.len(), 1);
    assert_eq!(outcome.calls[0].name, "WebSearch");
    assert!(
        scope_token_of(&outcome.calls[0].meta).is_none(),
        "非 workspace 来源不应携带 workspace task scope"
    );
}

/// [回归测试] 无父会话绑定时仍登记独立子绑定，模型参数/提示词/runId 不得冒充 owner。
#[tokio::test]
async fn test_workflow_mcp_missing_parent_binding_uses_checked_agent_binding() {
    let outcome = run_workflow_mcp(Scenario {
        server: "workspace",
        tool: "Bash",
        workspace_remote: false,
        ctx_root: None,
        bound_sessions: &[SPOOFED],
    })
    .await;
    assert_eq!(outcome.output, SYNC_OUTPUT);
    assert_eq!(outcome.calls.len(), 1);
    let token = scope_token_of(&outcome.calls[0].meta).unwrap();
    assert_eq!(
        Some(token.as_str()),
        outcome.token_for(&outcome.agent_session_id)
    );
    assert_ne!(Some(token.as_str()), outcome.token_for(SPOOFED));
}

/// [回归测试] 父目录不可达不影响子会话准入，也不得回落到模型自称的 session。
#[tokio::test]
async fn test_workflow_mcp_unregistered_root_does_not_own_agent_admission() {
    let outcome = run_workflow_mcp(Scenario {
        server: "workspace",
        tool: "Read",
        workspace_remote: false,
        ctx_root: Some("unregistered-root"),
        bound_sessions: &[SPOOFED],
    })
    .await;
    assert_eq!(outcome.output, SYNC_OUTPUT);
    assert_eq!(outcome.calls.len(), 1);
    let token = scope_token_of(&outcome.calls[0].meta).unwrap();
    assert_eq!(
        Some(token.as_str()),
        outcome.token_for(&outcome.agent_session_id)
    );
    assert_ne!(Some(token.as_str()), outcome.token_for(SPOOFED));
}
