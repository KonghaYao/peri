//! URL 模式 MCP elicitation 的宿主端口与池内投递。
//!
//! MCP server 可以在工具调用期间发 `elicitation/create`（`mode: "url"`）把
//! 授权 / 确认链接交给用户。前端（TUI）不在 MCP 客户端进程里，因此宿主在
//! 建连前把[`UrlElicitationPort`]注入池；`PeriClientHandler` 只负责按连接所在
//! 池解析端口，不自行猜测会话归属。
//!
//! 未注入端口（host 级池 / 无会话的直连 / 测试）时调用方一律回 `decline`，
//! 与「用户拒绝」在 MCP 结果上同形——MCP 协议没有「客户端不支持」的结果码，
//! 能力声明阶段已经表达过支持范围。
//!
//! 端口调用直接 await：rmcp 对每个对端请求单独 spawn service task，等待用户
//! 决策不会阻塞连接读循环。这不是 OAuth 的 pending-callback 模式——那条链路
//! 需要跨 RPC 回传授权码，这里响应仍走在同一条 MCP 请求上，没有第二个投递面。

use std::sync::Arc;
use std::sync::Weak;

pub use peri_acp_types::ports::{UrlElicitationOutcome, UrlElicitationPort, UrlElicitationRequest};

use super::McpClientPool;

/// 连接上的 elicitation 归属：连接所在池 + 连接名。
///
/// 会话级 MCP 池本身只有一个归属会话（`SessionEnvironment` 每会话一份池），
/// 所以「从连接身份解析归属会话」在池这一层已经成立；handler 只需把请求交回
/// 池，由池当前注入的端口决定投递面。
#[derive(Clone)]
pub(crate) struct UrlElicitationSource {
    pool: Weak<McpClientPool>,
    server_name: String,
}

impl UrlElicitationSource {
    pub(crate) fn new(pool: Weak<McpClientPool>, server_name: impl Into<String>) -> Self {
        Self {
            pool,
            server_name: server_name.into(),
        }
    }

    pub(crate) async fn elicit(
        &self,
        message: String,
        url: String,
        elicitation_id: String,
    ) -> UrlElicitationOutcome {
        let Some(pool) = self.pool.upgrade() else {
            tracing::debug!(
                server = %self.server_name,
                "URL elicitation 丢弃：所属 MCP 池已释放"
            );
            return UrlElicitationOutcome::Declined;
        };
        pool.elicit_url(UrlElicitationRequest {
            server_name: self.server_name.clone(),
            message,
            url,
            elicitation_id,
        })
        .await
    }
}

impl McpClientPool {
    /// 注入 URL elicitation 端口（宿主装配面在 `run_initialize` 前调用）。
    ///
    /// 与 OAuth 事件回调同一窗口语义：池已进入关闭事务后注入无效。
    pub fn set_url_elicitation_port(&self, port: Arc<dyn UrlElicitationPort>) {
        let _admission = self.lifecycle_registration.lock();
        if self.is_open() {
            *self.url_elicitation_port.write() = Some(port);
        }
    }

    /// 读取当前端口（每次请求现取：装配可在建连前完成，端口不绑定到单个连接）。
    pub(crate) fn url_elicitation_port(&self) -> Option<Arc<dyn UrlElicitationPort>> {
        self.url_elicitation_port.read().clone()
    }

    /// 投递一次 URL elicitation；无端口时回 `declined` 并留一条 info。
    pub(crate) async fn elicit_url(&self, request: UrlElicitationRequest) -> UrlElicitationOutcome {
        let Some(port) = self.url_elicitation_port() else {
            tracing::info!(
                server = %request.server_name,
                "URL elicitation 无宿主端口（无会话归属），按 decline 处理"
            );
            return UrlElicitationOutcome::Declined;
        };
        port.elicit_url(request).await
    }
}

#[cfg(test)]
#[path = "url_elicitation_test.rs"]
mod tests;
