//! 进程内 host 的本地 MCP Apps 授权层（仅显式 opt-in 的装配点注入）。
//!
//! 契约：[`peri_acp_types::session_authorization`]（§4.3）。本模块只提供一个
//! `SessionAuthorizationPort` 实现，不新增授权概念，也不解释身份。
//!
//! # 安全边界（为什么仅本地、默认拒绝、开启后的影响面）
//!
//! - **为什么仅本地**：实现把「能到达本 facade 的连接」视为本机用户。它成立
//!   的唯一依据是部署边界——进程内 ACP host 的唯一传输是 TUI 自己的 MPSC pair
//!   （`launch.rs` 中 `mpsc_transport_pair`），没有外部进程可接入的连接。
//!   它**不做认证**：不得用于 stdio、TCP/socket 或任何可被非本进程客户端接入的
//!   装配路径；那些部署仍需真实身份层（或保持默认拒绝）。
//! - **默认拒绝**：只有 `assemble_server_config_with_local_mcp_apps` 在开关
//!   开启时注入本层；其余装配点（TUI 默认、print、stdio、SDK）保持
//!   `session_authorization = None` ⇒ 六个受控方法一律 `EntitlementDenied`。
//! - **影响面**：仅 `attach` / `request` / `rerun` / `cancel` / `detach` / `close`
//!   六个受控方法（`apps/list` 本就不需要 entitlement）。`caller` 仍来自服务端
//!   连接事实（`ConnectionContext::id`），请求体自述字段不参与目标或身份选择；
//!   目标合法性仍由 facade 的会话归属 / 在线 endpoint / 附件纪元校验决定。
//!
//! 移除条件：部署侧为进程内 host 提供真实身份层（或 TUI 自带跨进程信任协议）
//! 后，本层随开关一起删除；它不应演化为通用授权实现。

use std::sync::Arc;

use peri_acp_types::session_authorization::{
    SessionAuthorizationPort, SessionEntitlementRequest, SessionGrant,
};

/// 授予「本进程能到达的授权请求」的本地适配器。
///
/// 只按请求本身铸造等值 grant：`covers` 的三元组复核因此必然通过，且不会
/// 越权覆盖别的 session / caller / capability（消费方的 `covers` 仍是最终判据）。
struct LocalInProcessAuthorizer;

impl SessionAuthorizationPort for LocalInProcessAuthorizer {
    fn grant(&self, request: &SessionEntitlementRequest) -> Option<SessionGrant> {
        Some(SessionGrant::new(
            request.session_id.clone(),
            request.caller.clone(),
            request.capability,
        ))
    }
}

/// 装配点唯一入口：注入后 `apps/*` 六个受控方法对**本进程内**连接可用。
///
/// 调用者必须已确认部署边界（进程内传输、无外部客户端）；本层无法也无意
/// 在运行期复核这一点。
pub(crate) fn local_in_process_authorizer() -> Arc<dyn SessionAuthorizationPort> {
    Arc::new(LocalInProcessAuthorizer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp_types::session_authorization::SessionCapability;

    #[test]
    fn grant_covers_the_requested_triple_and_nothing_else() {
        let port = local_in_process_authorizer();
        let request =
            SessionEntitlementRequest::new("session-1", "conn-1", SessionCapability::AppOperations);
        let grant = port.grant(&request).expect("本地层必须给出授权事实");
        assert!(grant.covers(&request), "grant 必须恰好覆盖本次请求");

        // 同端口不越权：别的 caller / capability 仍由消费方 `covers` 拒绝。
        let other_caller =
            SessionEntitlementRequest::new("session-1", "conn-2", SessionCapability::AppOperations);
        assert!(!grant.covers(&other_caller));
        let other_capability =
            SessionEntitlementRequest::new("session-1", "conn-1", SessionCapability::ControlApps);
        assert!(!grant.covers(&other_capability));
    }
}
