//! MCP over ACP 宿主接线的主路径测试：client 在 `session/new` 声明
//! `type: "acp"` 的 MCP server → 工具进入该会话的工具面 → 会话关闭时断开。
//!
//! 假件只在**协议对端**（一个实现 `AcpTransport` 的 client 宿主：应答
//! `mcp/connect`，把 `mcp/message` 交给它承载的那台 MCP server）。宿主侧走
//! 真实路径：真实 `session/new`、真实宿主网关 `AcpTransportGateway`、真实桥接
//! transport、真实 rmcp 握手与 `tools/list` 发现、真实 MCP 池提交与归属过滤。
//!
//! 声明的解析与注入点取自生产调用链：`session/new` 的参数形状就是生产形状，
//! attach 调用点与 `ServerLoop` 在 setup 响应之后的调用同款（`mcp/connect` 只带
//! `serverId`，必须在客户端拿到会话结果之后才发得出去）。
//!
//! 池与服务的注入理由：非 bare 工作区装配面才会构造 ACP MCP 服务（bare 池仅含 workspace）（`host/assemble.rs`），
//! 而拉起真实外部 server 不适合测试——这里注入同一份装配产物，装配面自身由
//! `host/assemble.rs` 的既有测试覆盖。

use std::time::Duration;

use async_trait::async_trait;
use peri_acp_types::ports::McpPoolPort;
use peri_middlewares::mcp::apps::McpCapabilityProfile;
use peri_middlewares::mcp::tool_bridge::build_tool_bridges_visible_to;
use peri_middlewares::mcp::{AcpMcpService, McpClientPool, McpTaskOwner};

use super::*;
use crate::transport::types::{IncomingMessage, RequestId};

/// client 宿主假件：真实部署里 client 就扮演这个角色。
#[derive(Default)]
struct FakeClientHost {
    /// 收到的 `mcp/connect` 的 `serverId`（按到达顺序）。
    connects: std::sync::Mutex<Vec<String>>,
    /// 收到的 `mcp/disconnect` 的 `connectionId`。
    disconnects: std::sync::Mutex<Vec<String>>,
    /// `mcp/message` 承载的内层 MCP 方法名。
    inner_methods: std::sync::Mutex<Vec<String>>,
}

impl FakeClientHost {
    fn recorded(values: &std::sync::Mutex<Vec<String>>) -> Vec<String> {
        values.lock().unwrap().clone()
    }
}

#[async_trait]
impl crate::transport::AcpTransport for FakeClientHost {
    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        match method {
            "mcp/connect" => {
                self.connects.lock().unwrap().push(
                    params
                        .get("serverId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                );
                Ok(json!({ "connectionId": "conn-1" }))
            }
            "mcp/disconnect" => {
                self.disconnects.lock().unwrap().push(
                    params
                        .get("connectionId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                );
                Ok(json!({}))
            }
            "mcp/message" => {
                let inner = params
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.inner_methods.lock().unwrap().push(inner.clone());
                match inner.as_str() {
                    "initialize" => Ok(json!({
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "serverInfo": { "name": "client-hosted-fixture", "version": "1" }
                    })),
                    "tools/list" => Ok(json!({ "tools": [{
                        "name": "echo",
                        "description": "fixture echo tool",
                        "inputSchema": { "type": "object", "properties": {} }
                    }] })),
                    "ping" => Ok(json!({})),
                    // 未实现的方法按 JSON-RPC 回错：rmcp 的 lifecycle 协商依赖
                    // -32601 判定 legacy initialize 回退，码值不得被抹平。
                    other => Err(AcpError::new(-32601, format!("method not found: {other}"))),
                }
            }
            other => Err(AcpError::new(-32601, format!("unexpected method: {other}"))),
        }
    }

    async fn send_notification(&self, _method: &str, _params: Value) -> Result<(), AcpError> {
        Ok(())
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        None
    }

    async fn send_response(
        &self,
        _id: RequestId,
        _result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        Ok(())
    }
}

/// 等待条件成立（建连与工具发现是异步的，没有固定时序）。
async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..600 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("等待超时: {what}");
}

#[tokio::test]
async fn acp_declared_server_reaches_the_session_tool_face_and_disconnects_on_close() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "key", "model"));
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = make_server_config(config, provider, &tmp).await;
    let cwd = tmp.path().canonicalize().unwrap();
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd: cwd.to_str().unwrap().to_owned(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });

    let peer = Arc::new(FakeClientHost::default());
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::clone(&peer) as Arc<_>;
    let mut sessions = HashMap::new();
    let params = json!({
        "cwd": cwd,
        "mcpServers": [{ "type": "acp", "name": "fixture", "serverId": "srv-1" }],
    });
    let created = handle_request("session/new", &params, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    let session_id = created["sessionId"].as_str().unwrap().to_owned();

    // 会话级 MCP 池与 MCP over ACP 服务（非 bare 工作区装配面的产物）。
    let (_mcp_owner, mcp_spawner) = McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner_and_profile(
        mcp_spawner,
        McpCapabilityProfile::disabled(),
    ));
    {
        let environment = Arc::get_mut(
            sessions
                .get_mut(&session_id)
                .unwrap()
                .environment
                .as_mut()
                .unwrap(),
        )
        .expect("会话刚建立，工作区装配未被共享");
        environment.cfg.mcp_pool = Some(Arc::clone(&pool) as Arc<dyn McpPoolPort>);
        environment.cfg.acp_mcp = Some(Arc::new(AcpMcpService::new(Arc::clone(&pool))));
    }

    // `ServerLoop` 在 setup 响应写入之后做的事。
    {
        let environment_cfg = &sessions[&session_id].environment.as_ref().unwrap().cfg;
        crate::host::requests::acp_mcp::attach_session_servers(
            environment_cfg,
            &transport,
            &params,
            &session_id,
        );
    }

    wait_until("声明的连接进入本会话的池", || {
        pool.get_client_visible_to("fixture", Some(&session_id))
            .is_some()
    })
    .await;
    let handle = pool
        .get_client_visible_to("fixture", Some(&session_id))
        .expect("归属会话应看到连接");
    assert!(matches!(
        handle.status,
        peri_middlewares::mcp::ClientStatus::Connected
    ));
    assert_eq!(
        handle
            .tools
            .iter()
            .map(|tool| tool.name.to_string())
            .collect::<Vec<_>>(),
        vec!["echo".to_string()]
    );
    // 建连依的是客户端声明的 serverId；工具经内层 tools/list 发现。
    assert_eq!(
        FakeClientHost::recorded(&peer.connects),
        vec!["srv-1".to_string()]
    );
    assert!(FakeClientHost::recorded(&peer.inner_methods)
        .iter()
        .any(|method| method == "tools/list"));

    // 工具进入该会话的工具面（deferred：`build_tool_bridges_visible_to` 是唯一
    // 会话级构造入口，名字带 server 前缀）。
    let tools = build_tool_bridges_visible_to(&pool, Some(&session_id));
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool.name().to_string())
            .collect::<Vec<_>>(),
        vec!["mcp__fixture__echo".to_string()]
    );
    assert!(tools[0].mcp_server_name().is_some());
    assert!(
        !tools[0].is_direct(),
        "ACP 工具与其它 MCP 工具一致走 deferred 发现面"
    );

    // 入站：宿主按 connectionId 定位承载会话的服务，`ping` 由 rmcp 客户端
    // runtime 应答——证明「宿主路由 → 桥接 → rmcp → 响应回程」整条路径。
    let port = sessions[&session_id]
        .environment
        .as_ref()
        .unwrap()
        .cfg
        .acp_mcp
        .clone()
        .unwrap();
    assert!(port.owns_connection("conn-1"));
    assert!(!port.owns_connection("conn-2"));
    let Ok((routed, inbound)) = crate::host::requests::acp_mcp::route_inbound(
        &sessions,
        &json!({ "connectionId": "conn-1", "method": "ping" }),
    ) else {
        panic!("已建立的连接必须能被宿主路由");
    };
    assert!(Arc::ptr_eq(&routed, &port));
    routed.request(inbound).await.expect("ping 应由 rmcp 应答");

    // 会话关闭：连接事实随会话消失，且必须先于池关闭完成（否则没有出站通道）。
    handle_request(
        "session/close",
        &json!({"sessionId": session_id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(
        FakeClientHost::recorded(&peer.disconnects),
        vec!["conn-1".to_string()]
    );
    assert!(pool
        .get_client_visible_to("fixture", Some(&session_id))
        .is_none());
    assert!(build_tool_bridges_visible_to(&pool, Some(&session_id)).is_empty());
}
