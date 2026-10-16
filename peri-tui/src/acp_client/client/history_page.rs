//! 历史页 RPC：`session/load` + `cursor` 的只读浏览出口（REQ §5.1 `HistoryPage` 模式）。
//!
//! 与 [`AcpTuiClient::load_session`] 的关键差别，全部来自服务端的分流契约
//! （`peri-acp/src/host/history_page.rs`）：
//!
//! - **不进**生命周期操作闸门、不装配会话、不发 `session/close`、不改绑定——历史页
//!   可与正在执行的 turn 并行（REQ §5.1、A21）；
//! - 参数只有 `sessionId` / `cursor` / `limit`：`cwd`、`mcpServers` 与
//!   `loadGeneration` 在历史页上是明确拒绝的；
//! - 响应形状与初始 load 不同（`items` 而不是回放通知），错误码：`-32011` 跨会话、
//!   `-32012` 范围失效（客户端据此停止对本次浏览继续翻页）。

use serde_json::json;

use peri_acp::transport::{AcpTransport, types::AcpError};

use super::AcpTuiClient;

impl AcpTuiClient {
    /// 拉取一页更早历史。返回原始响应体，由调用方（`history_page_consumer`）
    /// 按契约解码——本方法不猜测、不降级（解不开就丢弃整页）。
    pub(crate) async fn load_history_page(
        &self,
        session_id: &str,
        cursor: &str,
        limit: usize,
    ) -> Result<serde_json::Value, AcpError> {
        let params = json!({
            "sessionId": session_id,
            "cursor": cursor,
            "limit": limit,
        });
        self.transport.send_request("session/load", params).await
    }
}
