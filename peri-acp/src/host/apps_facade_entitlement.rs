//! `SessionAppsFacade` 的 entitlement 门（R4 消费点；裁决：方案 b「只接线不注入」）。
//!
//! 契约：[`peri_acp_types::session_authorization`]（§4.3）。本模块只做「按能力询问 →
//! 复核 → 拒绝投影」，不铸造也不解释身份：调用者身份取自**服务端连接事实**
//! （`ConnectionContext::id`，服务端 `Uuid::now_v7()`；不是请求体里的任何字符串），
//! 由部署侧身份层把它映射为 `TrustedCallerId` 并作答。
//!
//! **默认拒绝**：本仓 stdio 装配不注入身份层（`Option` 为 `None`）⇒ `apps/*` 的
//! 六个受控方法（attach / request / rerun / cancel / detach / close）一律拒绝
//! （`EntitlementDenied`）。仅 SDK / 服务化部署在装配点注入
//! `SessionAuthorizationPort` 后这一面才可用——这是本批的既定代价，不是缺陷。
//!
//! 与 `SessionAccessPort` **不得互相替代**：这里的判断只来自 `grant` + `covers`，
//! 绝不回落到「按 id 查得到对象」。`EntitlementState::Unenforced` 描述的是 record
//! 层（签发点早于任何 App 回调），与本层已强制不矛盾。

use std::sync::Arc;

use peri_acp_types::mcp_apps::McpAppsErrorKind;
use peri_acp_types::session_authorization::{
    SessionAuthorizationPort, SessionCapability, SessionEntitlementRequest,
};

use super::apps_error;
use crate::transport::types::AcpError;

/// 会话级 entitlement 门：`(session, caller, capability)` 的授权消费点。
pub(super) struct SessionEntitlementGate {
    session_id: String,
    /// 部署 / SDK 装配层注入的身份层；`None` = 未注入 = 一律拒绝（契约 fail-closed）。
    port: Option<Arc<dyn SessionAuthorizationPort>>,
}

impl SessionEntitlementGate {
    pub(super) fn new(
        session_id: impl Into<String>,
        port: Option<Arc<dyn SessionAuthorizationPort>>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            port,
        }
    }

    /// 两步义务（契约 §4.3）：`grant` 给出授权事实，`covers` 复核它**恰好**覆盖本次
    /// 请求；任一步不成立（含未注入端口、grant 落在别的 session / caller / 能力）一律
    /// 拒绝。调用方负责先做定位校验（会话 / 在线 endpoint），本门只回答授权。
    pub(super) fn ensure(
        &self,
        caller: &str,
        capability: SessionCapability,
    ) -> Result<(), AcpError> {
        let request = SessionEntitlementRequest::new(self.session_id.clone(), caller, capability);
        match self.port.as_ref().and_then(|port| port.grant(&request)) {
            Some(grant) if grant.covers(&request) => Ok(()),
            _ => Err(apps_error(McpAppsErrorKind::EntitlementDenied)),
        }
    }
}

/// 测试专用身份层（仅 `cfg(test)`）：模拟「部署侧已接入身份层」。
///
/// 生产 stdio 装配**不**注入端口（方案 b）⇒ `apps/*` 默认拒绝；既有夹具必须显式
/// 注入本模块的端口才能继续验证门后的行为。放在 `cfg(test)` 而不是 `test-support`
/// feature：消费方全部在 `peri-acp` 自身的 lib 测试内，没有任何跨 crate 装配点
/// 构造 `AcpServerConfig` / `SessionAppsFacade`（跨 crate 暴露只会扩大信任面）。
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Arc;

    use peri_acp_types::session_authorization::{
        SessionAuthorizationPort, SessionEntitlementRequest, SessionGrant,
    };

    /// 授权一切 (session, caller, capability) 的部署身份层。
    struct AllowAllCallers;

    impl SessionAuthorizationPort for AllowAllCallers {
        fn grant(&self, request: &SessionEntitlementRequest) -> Option<SessionGrant> {
            Some(SessionGrant::new(
                request.session_id.clone(),
                request.caller.clone(),
                request.capability,
            ))
        }
    }

    /// 已注入的身份层（全允许）。
    pub(crate) fn allow_all() -> Option<Arc<dyn SessionAuthorizationPort>> {
        Some(Arc::new(AllowAllCallers))
    }
}
