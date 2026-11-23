//! URL 模式 MCP elicitation 的会话级投递面。
//!
//! MCP server 在工具调用期间可以发 `elicitation/create`（`mode: "url"`）请用户
//! 在浏览器里完成授权。MCP 客户端在 `peri-middlewares`，前端（TUI）在 ACP 连接
//! 的另一端：本模块是两者之间的**唯一**桥，把端口请求投影成 ACP
//! `elicitation/create`（URL 模式），再把用户决策映射回端口结果。
//!
//! 归属由 [`SessionInteractionRouter`] 的 Host 分支决定，不在这里另选连接：
//! 请求作用域连接优先，否则本会话**唯一**在线 endpoint；无 endpoint 或多
//! endpoint 一律未投递（按 `decline` 返回），绝不广播。会话级 MCP 池本身只
//! 服务一个会话，因此「连接 → 会话」的解析在注入端口时就已经成立。
//!
//! 与 OAuth 的 pending-callback 链路**不是**同一事实：那条链路要跨 RPC 回传
//! 授权码，这里响应仍走在同一条 MCP 请求上，没有第二个投递面。

use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::ports::{UrlElicitationOutcome, UrlElicitationPort, UrlElicitationRequest};

use super::interaction_router::SessionInteractionRouter;
use crate::broker::UrlElicitationDecision;

/// 把 URL elicitation 投给本会话交互 router 的端口实现。
pub(crate) struct SessionUrlElicitation {
    router: Arc<SessionInteractionRouter>,
}

impl SessionUrlElicitation {
    pub(crate) fn new(router: Arc<SessionInteractionRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl UrlElicitationPort for SessionUrlElicitation {
    async fn elicit_url(&self, request: UrlElicitationRequest) -> UrlElicitationOutcome {
        match self
            .router
            .request_url_elicitation(
                &request.message,
                &request.url,
                &request.elicitation_id,
                Some(&request.server_name),
            )
            .await
        {
            Ok(UrlElicitationDecision::Accepted) => UrlElicitationOutcome::Accepted,
            Ok(UrlElicitationDecision::Declined) => UrlElicitationOutcome::Declined,
            Ok(UrlElicitationDecision::Cancelled) => UrlElicitationOutcome::Cancelled,
            Err(cause) => {
                // 本会话没有可投递的在线 endpoint（或归属歧义）：不广播、不阻塞，
                // 按「用户未确认」结算。原因按事实记录，便于对照 TUI 是否在线。
                tracing::info!(
                    server = %request.server_name,
                    ?cause,
                    reason = cause.reason_text(),
                    "URL elicitation 无在线交互 endpoint，按 decline 返回"
                );
                UrlElicitationOutcome::Declined
            }
        }
    }
}

#[cfg(test)]
#[path = "mcp_url_elicitation_test.rs"]
mod tests;
