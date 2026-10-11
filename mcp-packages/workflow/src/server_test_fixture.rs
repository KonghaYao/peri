//! `server_test.rs` 的共享装配夹具（拆分自 `server_test.rs`，STD-SIZE-001）：
//! 假 agent 执行器、runtime/服务器/双工连接与 wire 调用辅助。
//!
//! 真实链路与覆盖口径见 `server_test.rs` 头注（注入生产 handler + rmcp 原生 client）。

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use peri_acp_types::workflow::{AgentRunParams, AgentRunResult, Usage};
use peri_workflow::protocol::ProgressEvent;
use peri_workflow::runner::AgentExecutor;
use peri_workflow::runtime::WorkflowRuntime;
use peri_workflow::runtime::WORKFLOW_CONSUMER_META_KEY;
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResult, ErrorCode, MetaObject, ReadResourceRequestParams,
        ReadResourceResult, RequestMetaObject,
    },
    service::{Peer, QuitReason, RunningService},
    ClientLifecycleMode, RoleClient, ServiceError,
};
use serde_json::Value;

use super::WorkflowMcpServer;
use crate::input::WorkflowInstanceInput;

const DUPLEX_BUF: usize = 8 * 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

/// 测试用 agent 执行器：wire 测试不触发真实 agent 执行。
struct MockAgentExecutor;

#[async_trait]
impl AgentExecutor for MockAgentExecutor {
    async fn execute(&self, _params: AgentRunParams) -> AgentRunResult {
        AgentRunResult::Ok {
            output: "mock".into(),
            usage: Usage { output_tokens: 0 },
            model: None,
            tool_count: None,
            token_count: None,
            phase: None,
            duration_ms: None,
        }
    }
}

pub(super) fn make_runtime() -> Arc<WorkflowRuntime> {
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockAgentExecutor);
    let (notification_tx, _notification_rx) = tokio::sync::broadcast::channel(32);
    Arc::new(WorkflowRuntime::new(
        executor,
        std::env::temp_dir().to_str().expect("temp dir 可用"),
        notification_tx,
        None,
    ))
}

/// 任务发布参数（订阅唤醒只需任务事件，不触发真实执行）。
pub(super) fn task_params(run_id: &str) -> AgentRunParams {
    AgentRunParams {
        run_id: run_id.to_string(),
        agent_id: 1,
        prompt: "idle".into(),
        schema: None,
        model: None,
        max_tokens: None,
        agent_type: None,
        isolation: None,
        allowed_tools: None,
        label: None,
        phase: None,
    }
}

pub(super) struct Pair {
    service: RunningService<RoleClient, ()>,
    server_task: tokio::task::JoinHandle<Result<QuitReason, tokio::task::JoinError>>,
}

impl Pair {
    pub(super) fn peer(&self) -> Peer<RoleClient> {
        self.service.peer().clone()
    }

    pub(super) async fn shutdown(mut self) {
        let _ = self.service.close_with_timeout(CLOSE_TIMEOUT).await;
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, &mut self.server_task).await;
    }
}

/// 已绑定槽位的 handler：夹具跳过「装配期空槽 → 会话就绪绑定」的两阶段时序，
/// 未绑定路径由 `unbound_slot_*` 测试单独覆盖。
pub(super) fn server(runtime: Arc<WorkflowRuntime>) -> WorkflowMcpServer {
    WorkflowMcpServer::new(WorkflowInstanceInput::new(runtime))
}

pub(super) async fn connect(server: WorkflowMcpServer) -> Pair {
    let (client_io, server_io) = tokio::io::duplex(DUPLEX_BUF);
    let server_task = tokio::spawn(async move {
        let running = rmcp::serve_server(server, tokio::io::split(server_io))
            .await
            .expect("workflow server 装配失败");
        running.waiting().await
    });
    let service = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        rmcp::serve_client_with_lifecycle(
            (),
            tokio::io::split(client_io),
            ClientLifecycleMode::Auto {
                preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
                legacy_version: None,
            },
        ),
    )
    .await
    .expect("client 侧握手超时")
    .expect("client 侧握手失败");
    Pair {
        service,
        server_task,
    }
}

pub(super) fn call_params(name: &str, arguments: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_string())
        .with_arguments(arguments.as_object().cloned().unwrap_or_default())
}

/// 消费者令牌只经请求 `_meta` 传递（[`WORKFLOW_CONSUMER_META_KEY`]）。
pub(super) fn consumer_meta(token: &str) -> RequestMetaObject {
    let mut fields = serde_json::Map::new();
    fields.insert(
        WORKFLOW_CONSUMER_META_KEY.to_string(),
        Value::String(token.to_string()),
    );
    RequestMetaObject(MetaObject(fields))
}

pub(super) async fn call_tool(
    peer: &Peer<RoleClient>,
    name: &str,
    arguments: Value,
) -> CallToolResult {
    call_tool_with_meta(peer, name, arguments, None).await
}

pub(super) async fn call_tool_with_meta(
    peer: &Peer<RoleClient>,
    name: &str,
    arguments: Value,
    meta: Option<RequestMetaObject>,
) -> CallToolResult {
    let mut params = call_params(name, arguments);
    params.meta = meta;
    peer.call_tool(params)
        .await
        .expect("tools/call 必须返回 Ok（工具级失败在结果内）")
}

/// 协议级错误断言路径：`tools/call` 必须返回 JSON-RPC error（不是工具结果）。
pub(super) async fn call_tool_protocol_error(
    peer: &Peer<RoleClient>,
    name: &str,
    arguments: Value,
    meta: Option<RequestMetaObject>,
) -> rmcp::model::ErrorData {
    let mut params = call_params(name, arguments);
    params.meta = meta;
    match peer.call_tool(params).await {
        Ok(result) => panic!("期望协议级错误，实际返回工具结果: {}", first_text(&result)),
        Err(ServiceError::McpError(error)) => error,
        Err(other) => panic!("期望 MCP error，实际: {other}"),
    }
}

pub(super) fn assert_invalid_params(error: &rmcp::model::ErrorData, context: &str) {
    assert_eq!(
        error.code,
        ErrorCode::INVALID_PARAMS,
        "{context} 必须是 invalid_params: {}",
        error.message
    );
}

/// 模拟「run 已登记」：progress store 只在 `RunStarted` 提交时登记 run
/// （`peri-workflow/src/progress.rs` 的 `commit`）。
pub(super) fn register_run(runtime: &WorkflowRuntime, run_id: &str) {
    runtime
        .progress_store()
        .apply_event(&ProgressEvent::RunStarted {
            run_id: run_id.to_string(),
            workflow_name: "test-workflow".to_string(),
            meta: None,
        });
}

pub(super) fn first_text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .find_map(|block| match block {
            rmcp::model::ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

pub(super) async fn read_resource(
    peer: &Peer<RoleClient>,
    uri: &str,
) -> Result<ReadResourceResult, String> {
    peer.read_resource(ReadResourceRequestParams::new(uri))
        .await
        .map_err(|error| error.to_string())
}

pub(super) fn first_resource_text(result: &ReadResourceResult) -> String {
    result
        .contents
        .iter()
        .find_map(|content| match content {
            rmcp::model::ResourceContents::TextResourceContents { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

pub(super) fn published_params(run_id: &str, agent_id: u64) -> AgentRunParams {
    AgentRunParams {
        run_id: run_id.to_string(),
        agent_id,
        prompt: "inspect".to_string(),
        schema: None,
        model: None,
        max_tokens: None,
        agent_type: None,
        isolation: None,
        allowed_tools: None,
        label: None,
        phase: None,
    }
}
