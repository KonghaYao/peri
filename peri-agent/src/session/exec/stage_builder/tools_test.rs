//! 启动提交的碰撞目录注册回调（`catalog_registration`）接线测试。
//!
//! 只验证 middleware 侧 registry 拒绝到 `CatalogRefreshError` 的映射与透传：
//! registry 自己的拒绝语义由 `mcp::dynamic::registry` 的测试覆盖，目录提交的
//! 原子性由 `session::tool_catalog` 的测试覆盖，本文件补的是两端之间的接线。
use super::*;
use async_trait::async_trait;
use peri_acp_types::dynamic_mcp::{
    CanonicalDynamicMcpAction, DynamicMcpFailure, DynamicMcpOperationState, DynamicMcpResponse,
    DynamicMcpShutdownReport,
};
use peri_acp_types::ports::{SessionCloseRegistration, SessionMcpCapabilityPort};
use std::sync::Mutex;

/// 只控制 `register_catalog` 结果的 deployment stub；其余方法在测试里不可达。
struct StubDeployment {
    failure: DynamicMcpFailure,
    seen: Mutex<Vec<(String, Vec<String>)>>,
}

impl StubDeployment {
    fn rejecting(code: DynamicMcpErrorCode, safe_summary: &str) -> Arc<Self> {
        Arc::new(Self {
            failure: DynamicMcpFailure::new(code, DynamicMcpOperationState::Failed, safe_summary),
            seen: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl DynamicMcpDeploymentPort for StubDeployment {
    async fn execute(
        &self,
        _session_id: &str,
        _action: CanonicalDynamicMcpAction,
    ) -> Result<DynamicMcpResponse, DynamicMcpFailure> {
        unreachable!("本测试只驱动 register_catalog")
    }

    fn register_catalog(
        &self,
        session_id: &str,
        tools: Vec<DynamicMcpCatalogTool>,
    ) -> Result<(), DynamicMcpFailure> {
        self.seen.lock().unwrap().push((
            session_id.to_string(),
            tools.into_iter().map(|tool| tool.name).collect(),
        ));
        Err(self.failure.clone())
    }

    fn capability(&self, _session_id: &str) -> Arc<dyn SessionMcpCapabilityPort> {
        struct Empty;
        impl SessionMcpCapabilityPort for Empty {
            fn snapshot(&self) -> Arc<peri_acp_types::dynamic_mcp::SessionMcpCapabilitySnapshot> {
                Arc::new(Default::default())
            }
        }
        Arc::new(Empty)
    }

    fn close_registration(&self, _session_id: &str) -> Arc<dyn SessionCloseRegistration> {
        struct Noop;
        #[async_trait]
        impl SessionCloseRegistration for Noop {
            async fn revoke_and_cleanup(&self) -> DynamicMcpShutdownReport {
                DynamicMcpShutdownReport::Complete
            }
        }
        Arc::new(Noop)
    }

    fn begin_shutdown(&self) {}

    async fn close_session(&self, _session_id: &str) -> DynamicMcpShutdownReport {
        DynamicMcpShutdownReport::Complete
    }

    async fn shutdown(&self) -> DynamicMcpShutdownReport {
        DynamicMcpShutdownReport::Complete
    }
}

fn catalog_tool(name: &str) -> DynamicMcpCatalogTool {
    DynamicMcpCatalogTool {
        name: name.to_string(),
        aliases: Vec::new(),
        static_mcp_server: None,
    }
}

/// registry 以 `ToolNameConflict` 拒绝时必须映射为 `StartupRegistrationRejected`，
/// 且冲突工具名无损保留（registry 侧固定用它作 `safe_summary`）。
#[test]
fn startup_registration_maps_tool_conflict_to_rejection() {
    let deployment =
        StubDeployment::rejecting(DynamicMcpErrorCode::ToolNameConflict, "mcp__sys__echo");
    let register = catalog_registration(deployment, "session-a".to_string());

    let error = register(vec![catalog_tool("mcp__sys__echo")]).unwrap_err();

    assert_eq!(
        error,
        CatalogRefreshError::StartupRegistrationRejected {
            tool: "mcp__sys__echo".to_string()
        }
    );
}

/// 非工具冲突的拒绝（会话/任务已关闭）不得被伪装成「某个工具名与目录冲突」。
#[test]
fn startup_registration_maps_non_conflict_to_inconsistent_capability() {
    let deployment = StubDeployment::rejecting(
        DynamicMcpErrorCode::TaskOwnerClosed,
        "Dynamic MCP task admission is closed",
    );
    let register = catalog_registration(deployment, "session-a".to_string());

    let error = register(vec![catalog_tool("mcp__sys__echo")]).unwrap_err();

    assert_eq!(error, CatalogRefreshError::InconsistentCapability);
}

/// 回调必须把本次 session 身份与整批候选工具原样交给 registry：session 传错会
/// 让碰撞目录注册到别的会话，工具丢失会让晚到的静态工具不被重验。
#[test]
fn startup_registration_forwards_session_and_candidate_tools() {
    let deployment = StubDeployment::rejecting(DynamicMcpErrorCode::ToolNameConflict, "boom");
    let register = catalog_registration(
        Arc::clone(&deployment) as Arc<dyn DynamicMcpDeploymentPort>,
        "session-z".to_string(),
    );

    let _ = register(vec![catalog_tool("Read"), catalog_tool("mcp__sys__echo")]);

    let seen = deployment.seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "每次提交恰好注册一次");
    assert_eq!(seen[0].0, "session-z");
    assert_eq!(
        seen[0].1,
        vec!["Read".to_string(), "mcp__sys__echo".to_string()],
        "候选工具必须整批透传且顺序不变"
    );
}
