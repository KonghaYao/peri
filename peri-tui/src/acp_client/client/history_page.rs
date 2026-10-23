//! History V2 RPC 出口：`_peri/history` 与 V2 `session/load`。
//!
//! 旧私有历史协议（`session/load + cursor` 分页、`peri.historyWindowV1` 窗口、
//! `session/history/body`）已删除；TUI 不再保留任何降级出口。

use peri_acp::transport::{AcpTransport, types::AcpError};

use super::AcpTuiClient;

impl AcpTuiClient {
    /// History V2 的唯一 RPC 出口（open/page/seek/body/close）。
    ///
    /// 参数由 `history_v2::consumer` 或详情面板按 reducer 分配的身份构造；本方法
    /// 不做二次校验，也不在失败时降级到旧协议。
    pub(crate) async fn history_v2_request(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, AcpError> {
        self.transport
            .send_request(peri_acp_types::history_v2::HISTORY_METHOD, params)
            .await
    }

    /// `session/load` 的唯一发送出口：由 V2 出站调度器串行调用，生命周期仍在
    /// `session.rs` 中完成。
    pub(crate) async fn send_session_load(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, AcpError> {
        self.transport.send_request("session/load", params).await
    }
}
