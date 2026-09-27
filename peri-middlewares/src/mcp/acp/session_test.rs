//! [`AcpMcpService`] 的 crate 内契约测试。
//!
//! 走真实链路：真实 `AcpMcpGatewayPort` 调用面、真实桥接 transport、真实
//! `serve_client_auto` 握手与 `tools/list` 发现、真实 MCP 池提交。假件只在
//! **协议对端**（`mcp/connect` / `mcp/message` 的应答方）——client 在真实部署
//! 里就扮演这个角色。
//!
//! 断言的行为契约（每条对应一处生产不变量）：
//! 1. **不阻塞**：`attach` 立即返回，连接在后台完成（deferred 工具面）；
//! 2. **幂等**：同会话同 `serverId` 重复声明只建一次连接；
//! 3. **不跨会话泄漏**：池内条目、工具桥接、状态面均按归属会话过滤；
//! 4. **失败可见**：建连失败留在本会话可见的池状态面（不 panic、不回抛）；
//! 5. **关闭即断**：`close_session` 终止在建任务（不落池）、发 `mcp/disconnect`、
//!    清理池条目，且不误伤其他会话的连接。

use std::sync::Arc;
use std::time::Duration;

use peri_acp_types::acp_mcp::{AcpMcpError, AcpMcpInbound, AcpMcpServerSpec};
use peri_acp_types::plugin::ConfigSource;
use peri_acp_types::ports::{AcpMcpGatewayPort, AcpMcpServerPort};
use serde_json::{json, Map, Value};

use super::AcpMcpService;
use crate::mcp::apps::McpCapabilityProfile;
use crate::mcp::client::{ClientStatus, McpClientPool};
use crate::mcp::task_scope::McpTaskOwner;
use crate::mcp::tool_bridge::build_tool_bridges_visible_to;

/// 假件观测到的协议调用。
#[derive(Default)]
struct FakeGateway {
    /// 收到的 `mcp/connect` 的 `serverId`（按到达顺序）。
    connects: parking_lot::Mutex<Vec<String>>,
    /// 收到的 `mcp/disconnect` 的 `connectionId`。
    disconnects: parking_lot::Mutex<Vec<String>>,
    /// `mcp/message` 通知的 `method`（内层方法名）。
    notifications: parking_lot::Mutex<Vec<String>>,
    /// `mcp/message` 请求的内层方法名。
    inner_requests: parking_lot::Mutex<Vec<String>>,
    /// `mcp/connect` 前的延迟（模拟慢建连）。
    connect_delay: Option<Duration>,
    /// `mcp/connect` 直接失败（模拟 client 拒绝该 server）。
    fail_connect: bool,
    /// 假 server 的 `tools/list` 载荷。
    tools: Vec<Value>,
}

impl FakeGateway {
    fn with_tools(tools: &[&str]) -> Self {
        Self {
            tools: tools
                .iter()
                .map(|name| {
                    json!({
                        "name": name,
                        "description": format!("{name} tool"),
                        "inputSchema": { "type": "object", "properties": {} }
                    })
                })
                .collect(),
            ..Self::default()
        }
    }

    fn connect_count(&self) -> usize {
        self.connects.lock().len()
    }
}

#[async_trait::async_trait]
impl AcpMcpGatewayPort for FakeGateway {
    async fn request(&self, method: &str, params: Value) -> Result<Value, AcpMcpError> {
        match method {
            "mcp/connect" => {
                if let Some(delay) = self.connect_delay {
                    tokio::time::sleep(delay).await;
                }
                if self.fail_connect {
                    return Err(AcpMcpError::unavailable("client 拒绝建连"));
                }
                let server_id = params
                    .get("serverId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.connects.lock().push(server_id);
                let connection_id = format!("conn-{}", self.connects.lock().len());
                Ok(json!({ "connectionId": connection_id }))
            }
            "mcp/message" => {
                let inner = params
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.inner_requests.lock().push(inner.clone());
                match inner.as_str() {
                    "initialize" => Ok(json!({
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "serverInfo": { "name": "acp-fixture", "version": "1" }
                    })),
                    "tools/list" => Ok(json!({ "tools": self.tools })),
                    // 未实现方法：按 JSON-RPC 码回错——rmcp 的 lifecycle 协商
                    // 依赖 -32601 判定 legacy 回退，码值不得被抹平。
                    other => Err(AcpMcpError {
                        code: -32601,
                        message: format!("method not found: {other}"),
                    }),
                }
            }
            "mcp/disconnect" => {
                let connection_id = params
                    .get("connectionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.disconnects.lock().push(connection_id);
                Ok(json!({}))
            }
            other => Err(AcpMcpError::not_found(format!("未知方法: {other}"))),
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), AcpMcpError> {
        if method == "mcp/message" {
            let inner = params
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            self.notifications.lock().push(inner);
        }
        Ok(())
    }
}

struct Fixture {
    pool: Arc<McpClientPool>,
    service: Arc<AcpMcpService>,
    gateway: Arc<FakeGateway>,
    _owner: McpTaskOwner,
}

fn fixture(tools: &[&str]) -> Fixture {
    let (owner, spawner) = McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner_and_profile(
        spawner,
        McpCapabilityProfile::disabled(),
    ));
    Fixture {
        service: Arc::new(AcpMcpService::new(Arc::clone(&pool))),
        pool,
        gateway: Arc::new(FakeGateway::with_tools(tools)),
        _owner: owner,
    }
}

fn spec(session_id: &str, name: &str, server_id: &str) -> AcpMcpServerSpec {
    AcpMcpServerSpec {
        session_id: session_id.to_string(),
        name: name.to_string(),
        server_id: server_id.to_string(),
    }
}

/// 轮询等待条件成立（连接握手 + 工具发现是异步的，无固定时序）。
async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..600 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("等待超时: {what}");
}

/// 契约 1 + 3：`attach` 不阻塞、连接最终进池，且只对声明它的会话可见。
#[tokio::test]
async fn attach_connects_in_background_and_stays_session_scoped() {
    let fixture = fixture(&["echo"]);
    let pool = Arc::clone(&fixture.pool);
    fixture.service.attach(
        Arc::clone(&fixture.gateway) as Arc<dyn AcpMcpGatewayPort>,
        vec![spec("s1", "acp-srv", "srv-1")],
    );

    // attach 本身是同步返回的：此处尚未有任何连接证据。
    assert!(pool.get_all_clients_visible_to(Some("s1")).is_empty());
    wait_until("acp 连接进池", || {
        pool.get_client_visible_to("acp-srv", Some("s1")).is_some()
    })
    .await;

    let handle = pool
        .get_client_visible_to("acp-srv", Some("s1"))
        .expect("归属会话应看到连接");
    assert!(matches!(handle.status, ClientStatus::Connected));
    assert_eq!(handle.source, Some(ConfigSource::Acp));
    assert_eq!(
        handle
            .tools
            .iter()
            .map(|t| t.name.to_string())
            .collect::<Vec<_>>(),
        vec!["echo".to_string()]
    );
    // 内层 MCP 握手走的就是 `mcp/message` 请求；初始化完成的
    // `notifications/initialized` 走 `mcp/message` 通知。
    assert!(fixture
        .gateway
        .inner_requests
        .lock()
        .iter()
        .any(|method| method == "tools/list"));
    assert!(fixture
        .gateway
        .notifications
        .lock()
        .iter()
        .any(|method| method == "notifications/initialized"));

    // 另一个会话：工具面、状态面、发现面都看不到这条连接。
    assert!(!pool.is_visible_to_session("acp-srv", "s2"));
    assert!(pool.get_client_visible_to("acp-srv", Some("s2")).is_none());
    assert!(pool.get_all_clients_visible_to(Some("s2")).is_empty());
    assert!(build_tool_bridges_visible_to(&pool, Some("s2")).is_empty());
    assert!(pool
        .all_server_infos_visible_to(Some("s2"))
        .iter()
        .all(|info| info.name != "acp-srv"));
    // 部署面（`None`）不做归属过滤，仍能看到池内的真实连接。
    assert!(pool.get_client_visible_to("acp-srv", None).is_some());
    assert_eq!(build_tool_bridges_visible_to(&pool, Some("s1")).len(), 1);
}

/// 契约 1 续：client 经 `mcp/message` 反向下发的请求打到内层 MCP 连接。
#[tokio::test]
async fn inbound_request_reaches_the_inner_connection() {
    let fixture = fixture(&["echo"]);
    fixture.service.attach(
        Arc::clone(&fixture.gateway) as Arc<dyn AcpMcpGatewayPort>,
        vec![spec("s1", "acp-srv", "srv-1")],
    );
    let pool = Arc::clone(&fixture.pool);
    wait_until("acp 连接进池", || {
        pool.get_client_visible_to("acp-srv", Some("s1")).is_some()
    })
    .await;

    // `ping` 由 rmcp 客户端 runtime 应答（默认 handler），因此这里证明的是
    // 「入站 mcp/message → 桥接 → rmcp → 响应回程」整条路径。
    let result = fixture
        .service
        .request(AcpMcpInbound {
            connection_id: "conn-1".to_string(),
            method: "ping".to_string(),
            params: None,
        })
        .await;
    assert!(result.is_ok(), "入站请求应得到内层响应: {result:?}");

    // 未知 connectionId：按契约码 -32001 拒绝，不静默成功。
    let missing = fixture
        .service
        .request(AcpMcpInbound {
            connection_id: "conn-missing".to_string(),
            method: "ping".to_string(),
            params: None,
        })
        .await
        .expect_err("未知连接必须报错");
    assert_eq!(missing.code, AcpMcpError::CODE_NOT_FOUND);

    let mut params = Map::new();
    params.insert("anything".to_string(), json!(1));
    assert!(fixture
        .service
        .notify(AcpMcpInbound {
            connection_id: "conn-1".to_string(),
            method: "notifications/cancelled".to_string(),
            params: Some(params),
        })
        .await
        .is_ok());
}

/// 契约 2：同会话同 `serverId` 重复声明幂等；不同会话同名各自建连且池内名不冲突。
#[tokio::test]
async fn attach_is_idempotent_and_cross_session_names_do_not_collide() {
    let fixture = fixture(&["echo"]);
    let gateway = Arc::clone(&fixture.gateway) as Arc<dyn AcpMcpGatewayPort>;
    fixture
        .service
        .attach(Arc::clone(&gateway), vec![spec("s1", "acp-srv", "srv-1")]);
    fixture
        .service
        .attach(Arc::clone(&gateway), vec![spec("s1", "acp-srv", "srv-1")]);
    let pool = Arc::clone(&fixture.pool);
    wait_until("首个连接进池", || {
        pool.get_client_visible_to("acp-srv", Some("s1")).is_some()
    })
    .await;
    assert_eq!(fixture.gateway.connect_count(), 1, "重复声明不得重复建连");

    // 另一会话声明同名 server：连接独立，池内名加后缀，互不覆盖。
    fixture
        .service
        .attach(Arc::clone(&gateway), vec![spec("s2", "acp-srv", "srv-2")]);
    wait_until("第二会话连接进池", || {
        pool.get_all_clients_visible_to(Some("s2")).len() == 1
    })
    .await;
    let second = pool
        .get_all_clients_visible_to(Some("s2"))
        .pop()
        .expect("第二会话应有自己的连接");
    assert_eq!(second.name, "acp-srv_2");
    assert!(matches!(second.status, ClientStatus::Connected));
    // 归属不变：各自只看得到自己那条。
    assert_eq!(build_tool_bridges_visible_to(&pool, Some("s1")).len(), 1);
    assert_eq!(build_tool_bridges_visible_to(&pool, Some("s2")).len(), 1);
    assert_eq!(fixture.gateway.connect_count(), 2);
}

/// 契约 3 续：ACP 连接的上下线不得进入**部署级**通知面。
///
/// 状态变化缓冲与 notifier 都是部署级的（任一会话 drain 一次即清空；notifier
/// 推的是 TUI 通知面），而 ACP 连接只属于声明它的会话。失败条目的首次插入本
/// 就不产生"变化"，这里锁的是更隐蔽的一代：同一 `serverId` 重试失败时旧状态
/// 已被取代，若不按归属跳过就会把别会话的 server 名与失败原因推进共享面。
#[tokio::test]
async fn acp_status_changes_stay_out_of_the_deployment_wide_notification_buffer() {
    let fixture = fixture(&[]);
    let pool = Arc::clone(&fixture.pool);
    pool.mark_initialized();

    // 对照组：部署级 server 的状态变化照旧进缓冲（守卫不是整体关停通知）。
    McpClientPool::insert_failed(&pool, "cfg-srv", "配置连接失败".to_string());
    pool.record_status_change("cfg-srv", Some(&ClientStatus::Uninitialized));
    let changes = pool.drain_pending_changes();
    assert!(
        changes.iter().any(|text| text.contains("cfg-srv")),
        "部署级 server 的状态变化必须照旧进入缓冲: {changes:?}"
    );

    // 归属会话的 ACP 条目：首代失败（无"变化"）与重试失败（有"变化"）都不进缓冲。
    pool.record_acp_failure("s1", "acp-srv", "第一代失败");
    pool.record_acp_failure("s1", "acp-srv", "重试仍失败");
    assert!(
        pool.drain_pending_changes().is_empty(),
        "ACP 连接的状态变化不得进入部署级通知缓冲"
    );
    // 事实仍在归属会话可见的池状态面（缓冲不是唯一出口）。
    let info = pool
        .all_server_infos_visible_to(Some("s1"))
        .into_iter()
        .find(|info| info.name == "acp-srv")
        .expect("失败条目应对归属会话可见");
    assert!(matches!(info.status, ClientStatus::Failed(_)));
    assert!(pool
        .all_server_infos_visible_to(Some("s2"))
        .iter()
        .all(|info| info.name != "acp-srv"));
}

/// 契约 4：建连失败留在池状态面（本会话可见、他会话不可见），不 panic 不回抛。
#[tokio::test]
async fn connect_failure_is_recorded_in_the_pool_state_face() {
    let (owner, spawner) = McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner_and_profile(
        spawner,
        McpCapabilityProfile::disabled(),
    ));
    let service = AcpMcpService::new(Arc::clone(&pool));
    let gateway = Arc::new(FakeGateway {
        fail_connect: true,
        ..FakeGateway::default()
    });
    service.attach(
        Arc::clone(&gateway) as Arc<dyn AcpMcpGatewayPort>,
        vec![spec("s1", "acp-srv", "srv-1")],
    );

    wait_until("失败条目落池", || {
        pool.all_server_infos_visible_to(Some("s1"))
            .iter()
            .any(|info| info.name == "acp-srv")
    })
    .await;
    let info = pool
        .all_server_infos_visible_to(Some("s1"))
        .into_iter()
        .find(|info| info.name == "acp-srv")
        .expect("失败条目应对归属会话可见");
    assert!(matches!(info.status, ClientStatus::Failed(_)));
    assert!(info.error_summary.is_some(), "失败原因应可核对");
    // 他会话看不到这条失败证据（免得把别人的 server 读成自己的）。
    assert!(pool
        .all_server_infos_visible_to(Some("s2"))
        .iter()
        .all(|info| info.name != "acp-srv"));
    // 失败条目没有连接（peer / tools 为空），因此不进工具面。
    assert!(build_tool_bridges_visible_to(&pool, Some("s1")).is_empty());
    assert!(pool.get_client_visible_to("acp-srv", Some("s2")).is_none());
    drop(owner);
}

/// 契约 5：`close_session` 断开该会话全部连接，其他会话不受影响。
#[tokio::test]
async fn close_session_disconnects_only_that_session() {
    let fixture = fixture(&["echo"]);
    let gateway = Arc::clone(&fixture.gateway) as Arc<dyn AcpMcpGatewayPort>;
    let pool = Arc::clone(&fixture.pool);
    fixture
        .service
        .attach(Arc::clone(&gateway), vec![spec("s1", "acp-srv", "srv-1")]);
    fixture
        .service
        .attach(Arc::clone(&gateway), vec![spec("s2", "acp-srv", "srv-2")]);
    wait_until("两条连接进池", || {
        pool.get_all_clients_visible_to(Some("s1")).len() == 1
            && pool.get_all_clients_visible_to(Some("s2")).len() == 1
    })
    .await;

    fixture.service.close_session("s1").await;

    assert!(pool.get_client_visible_to("acp-srv", Some("s1")).is_none());
    assert!(pool.all_server_infos_visible_to(Some("s1")).is_empty());
    assert_eq!(fixture.gateway.disconnects.lock().len(), 1);
    assert_eq!(
        fixture
            .gateway
            .disconnects
            .lock()
            .first()
            .map(String::as_str),
        Some("conn-1")
    );
    // s2 的连接仍在，且仍是自己的。
    assert_eq!(pool.get_all_clients_visible_to(Some("s2")).len(), 1);
    // 幂等：重复关闭不产生额外协议调用。
    fixture.service.close_session("s1").await;
    assert_eq!(fixture.gateway.disconnects.lock().len(), 1);
    assert_eq!(fixture.gateway.connect_count(), 2);
}

/// 契约 5 续：在建连接被会话关闭终止，握手完成后**不得**落池。
#[tokio::test]
async fn close_session_aborts_inflight_connect_without_committing() {
    let (owner, spawner) = McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner_and_profile(
        spawner,
        McpCapabilityProfile::disabled(),
    ));
    let service = AcpMcpService::new(Arc::clone(&pool));
    let gateway = Arc::new(FakeGateway {
        connect_delay: Some(Duration::from_millis(150)),
        ..FakeGateway::with_tools(&["echo"])
    });
    service.attach(
        Arc::clone(&gateway) as Arc<dyn AcpMcpGatewayPort>,
        vec![spec("s1", "acp-srv", "srv-1")],
    );
    // `mcp/connect` 尚未返回（延迟窗口内关闭会话）。
    service.close_session("s1").await;
    tokio::time::sleep(Duration::from_millis(400)).await;

    assert!(
        pool.get_client_visible_to("acp-srv", Some("s1")).is_none(),
        "会话关闭后在建连接不得提交进池"
    );
    assert!(
        pool.all_server_infos_visible_to(Some("s1")).is_empty(),
        "会话关闭后不得留下该会话的池条目"
    );
    drop(owner);
}
