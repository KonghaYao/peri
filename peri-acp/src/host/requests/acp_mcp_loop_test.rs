//! MCP over ACP 宿主接线的主路径测试：client 在 `session/new` 声明
//! `type: "acp"` 的 MCP server → 工具进入该会话的工具面 → 会话关闭时清理。
//!
//! 假件只在**协议对端**（一个实现 `AcpTransport` 的 client 宿主：应答请求作用
//! 域的 `mcp/message`，每次调用自带 `serverId` / `requestId`）。宿主侧走真实
//! 路径：真实 `session/new`、真实宿主网关 `AcpTransportGateway`、真实桥接
//! transport、真实 rmcp 握手与 `tools/list` 发现、真实 MCP 池提交与归属过滤。
//!
//! 声明的解析与注入点取自生产调用链：`session/new` 的参数形状就是生产形状，
//! attach 调用点与 `ServerLoop` 在 setup 响应之后的调用同款（声明在客户端拿到
//! 会话结果之后才受理）。
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
    /// 收到的 ACP 请求方法名（应当只有 `mcp/message`）。
    request_methods: std::sync::Mutex<Vec<String>>,
    /// `mcp/message` 承载的内层 MCP 方法名。
    inner_methods: std::sync::Mutex<Vec<String>>,
    /// 内层 `mcp/message` 请求的 `(serverId, requestId)`。
    message_targets: std::sync::Mutex<Vec<(String, String)>>,
}

impl FakeClientHost {
    fn recorded(values: &std::sync::Mutex<Vec<String>>) -> Vec<String> {
        values.lock().unwrap().clone()
    }
}

#[async_trait]
impl crate::transport::AcpTransport for FakeClientHost {
    async fn send_cancellable_request(
        &self,
        method: &str,
        params: Value,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<Value, AcpError> {
        self.send_request(method, params).await
    }

    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.request_methods
            .lock()
            .unwrap()
            .push(method.to_string());
        match method {
            "mcp/message" => {
                let inner = params
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.inner_methods.lock().unwrap().push(inner.clone());
                self.message_targets.lock().unwrap().push((
                    params
                        .get("serverId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    params
                        .get("requestId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                ));
                // 外层 ACP 响应承载 `MessageMcpResponse`：成功分支是 `{ result }`。
                match inner.as_str() {
                    // 请求作用域协议的首个请求：现代 lifecycle 的协商入口。
                    "server/discover" => Ok(json!({ "result": {
                        "resultType": "complete",
                        "supportedVersions": ["2026-07-28"],
                        "capabilities": { "tools": {} },
                        "ttlMs": 0,
                        "cacheScope": "private",
                        "_meta": {
                            "io.modelcontextprotocol/serverInfo": {
                                "name": "client-hosted-fixture",
                                "version": "1"
                            }
                        }
                    } })),
                    "tools/list" => Ok(json!({ "result": { "tools": [{
                        "name": "echo",
                        "description": "fixture echo tool",
                        "inputSchema": { "type": "object", "properties": {} }
                    }] } })),
                    "ping" => Ok(json!({ "result": {} })),
                    // 未实现的方法按内层 MCP 错误回：rmcp 的 lifecycle 协商依赖
                    // -32601 判定 legacy initialize 回退，码值不得被抹平。
                    other => Ok(json!({ "error": {
                        "code": -32601,
                        "message": format!("method not found: {other}")
                    } })),
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

/// 等待条件成立（协议协商与工具发现是异步的，没有固定时序）。
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
async fn acp_declared_server_reaches_the_session_tool_face_and_is_cleaned_up_on_close() {
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
    // 夹具补生产步骤：生产装配给每个会话池 spawn `run_initialize`
    // （`peri-acp/src/host/assemble.rs`），初始化收口即发布「零 server」的目录事实；
    // 本夹具手工造的池没有这一代，会话关闭前的 workspace task scope 对账
    // （`wait_for_task_owner_catalog`）会一直等到超时。`mark_initialized` 正是
    // 生产「空配置」终态（`peri-middlewares/src/mcp/initialize.rs` 的空集合分支）。
    pool.mark_initialized();
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
    let registry = Arc::new(peri_acp_types::acp_mcp::AcpMcpRegistry::default());
    {
        let environment_cfg = &sessions[&session_id].environment.as_ref().unwrap().cfg;
        crate::host::requests::acp_mcp::attach_session_servers(
            registry.clone(),
            environment_cfg,
            &transport,
            &params,
            &session_id,
        );
    }

    wait_until("声明的 server 进入本会话的池", || {
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

    // 发现依的是客户端声明的 serverId；每个内层请求都是独立的 `mcp/message`
    // 请求（自带 requestId），没有 `mcp/connect` / `mcp/disconnect`。
    assert_eq!(
        FakeClientHost::recorded(&peer.request_methods)
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec!["mcp/message".to_string()]
    );
    assert!(FakeClientHost::recorded(&peer.inner_methods)
        .iter()
        .any(|method| method == "server/discover"));
    assert!(FakeClientHost::recorded(&peer.inner_methods)
        .iter()
        .any(|method| method == "tools/list"));
    let targets = peer.message_targets.lock().unwrap().clone();
    assert!(
        targets.iter().all(|(server_id, _)| server_id == "srv-1"),
        "所有内层请求都必须寻址声明的 serverId: {targets:?}"
    );
    assert!(
        targets
            .iter()
            .all(|(_, request_id)| request_id.starts_with("peri-")),
        "每个内层请求都必须带独立 requestId: {targets:?}"
    );
    let unique = targets
        .iter()
        .map(|(_, request_id)| request_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        unique.len(),
        targets.len(),
        "requestId 不得复用: {targets:?}"
    );

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

    // 入站：宿主按 serverId 定位承载会话的服务，client 通知被桥接接收。
    let port = sessions[&session_id]
        .environment
        .as_ref()
        .unwrap()
        .cfg
        .acp_mcp
        .clone()
        .unwrap();
    assert!(port.owns_server("srv-1"));
    assert!(!port.owns_server("srv-2"));
    let Ok((routed, inbound)) = crate::host::requests::acp_mcp::route_inbound(
        &registry,
        &sessions,
        &json!({
            "serverId": "srv-1",
            "requestId": "peri-1",
            "method": "notifications/cancelled",
            "params": { "requestId": "rmcp-1", "reason": "fixture" }
        }),
    ) else {
        panic!("已声明的 server 必须能被宿主路由");
    };
    assert!(Arc::ptr_eq(&routed, &port));
    routed
        .notify(inbound)
        .await
        .expect_err("已完成请求的迟到通知必须被拒绝");

    // 第二条真实会话使用独立 service / pool，但必须共享当前连接的注册表。
    let created_second = handle_request(
        "session/new",
        &json!({"cwd": cwd}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let second_id = created_second["sessionId"].as_str().unwrap().to_owned();
    let (_second_owner, second_spawner) = McpTaskOwner::new();
    let second_pool = Arc::new(McpClientPool::new_pending_with_spawner_and_profile(
        second_spawner,
        McpCapabilityProfile::disabled(),
    ));
    second_pool.mark_initialized();
    let second_port: Arc<dyn peri_acp_types::ports::AcpMcpServerPort> =
        Arc::new(AcpMcpService::new(second_pool.clone()));
    {
        let environment = Arc::get_mut(
            sessions
                .get_mut(&second_id)
                .unwrap()
                .environment
                .as_mut()
                .unwrap(),
        )
        .unwrap();
        environment.cfg.mcp_pool = Some(second_pool.clone());
        environment.cfg.acp_mcp = Some(second_port.clone());
    }
    let gateway = Arc::new(crate::host::requests::acp_mcp::AcpTransportGateway::new(
        transport.clone(),
    ));
    let declaration = |server_id: &str| peri_acp_types::acp_mcp::AcpMcpServerSpec {
        session_id: second_id.clone(),
        name: "second".into(),
        server_id: server_id.into(),
    };
    let conflict = second_port
        .attach(
            registry.clone(),
            gateway.clone(),
            vec![declaration("srv-1")],
        )
        .unwrap_err();
    assert_eq!(
        conflict.code,
        peri_acp_types::acp_mcp::AcpMcpError::CODE_UNAVAILABLE
    );
    second_port
        .attach(registry.clone(), gateway, vec![declaration("srv-2")])
        .unwrap();
    wait_until("第二会话 MCP 就绪", || {
        second_pool
            .get_client_visible_to("second", Some(&second_id))
            .is_some()
    })
    .await;
    for (server_id, expected) in [("srv-1", &port), ("srv-2", &second_port)] {
        let (routed, _) = crate::host::requests::acp_mcp::route_inbound(&registry, &sessions, &json!({
            "serverId": server_id, "requestId": "peri-1", "method": "notifications/tools/list_changed"
        })).unwrap();
        assert!(Arc::ptr_eq(&routed, expected));
    }
    // 相同 peri-1 只能出现在不同 serverId，冲突声明没有发出第二条 srv-1/peri-1。
    assert_eq!(
        peer.message_targets
            .lock()
            .unwrap()
            .iter()
            .filter(|(server, request)| server == "srv-1" && request == "peri-1")
            .count(),
        1
    );
    second_port.close_session(&second_id).await;
    second_port.close_session(&second_id).await;
    assert_eq!(
        registry.owner("srv-1").as_deref(),
        Some(session_id.as_str())
    );
    handle_request(
        "session/close",
        &json!({"sessionId": second_id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();

    // 会话关闭：声明与池条目随会话消失，且不再有反向的断开请求。
    handle_request(
        "session/close",
        &json!({"sessionId": session_id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert!(pool
        .get_client_visible_to("fixture", Some(&session_id))
        .is_none());
    assert!(build_tool_bridges_visible_to(&pool, Some(&session_id)).is_empty());
    assert!(!port.owns_server("srv-1"));
    // 关闭后再按 serverId 定位必须失败（-32001）。
    let Err(error) = crate::host::requests::acp_mcp::route_inbound(
        &registry,
        &sessions,
        &json!({
            "serverId": "srv-1",
            "requestId": "peri-3",
            "method": "notifications/cancelled"
        }),
    ) else {
        panic!("会话关闭后不得再路由到该 server");
    };
    assert_eq!(
        error.code,
        peri_acp_types::acp_mcp::AcpMcpError::CODE_NOT_FOUND
    );
    assert_eq!(
        FakeClientHost::recorded(&peer.request_methods)
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        ["mcp/message".to_string()].into_iter().collect()
    );
}
