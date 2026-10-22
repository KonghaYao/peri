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

use crate::kit::history_paging::{HistoryBodyRequest, WindowRequest};

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

    /// `peri.historyWindowV1` 的 open/page/seek。`history` 与旧顶层
    /// `cursor/limit/loadGeneration/cwd/mcpServers` 互斥，不携带任何旧字段。
    pub(crate) async fn load_history_window(
        &self,
        session_id: &str,
        request: &WindowRequest,
    ) -> Result<serde_json::Value, AcpError> {
        request
            .validate()
            .map_err(|error| AcpError::new(-32602, error.to_string()))?;
        let params = json!({
            "sessionId": session_id,
            "history": request.to_value(),
        });
        self.transport.send_request("session/load", params).await
    }

    /// `session/history/body`：有界范围正文块（不整取超大条目）。
    pub(crate) async fn load_history_body(
        &self,
        session_id: &str,
        request: &HistoryBodyRequest,
    ) -> Result<serde_json::Value, AcpError> {
        request
            .validate()
            .map_err(|error| AcpError::new(-32602, error.to_string()))?;
        let mut params = request.to_value();
        params["sessionId"] = serde_json::json!(session_id);
        self.transport
            .send_request("session/history/body", params)
            .await
    }

    /// History V2 的唯一 RPC 出口（open/page/seek/body/close）。
    ///
    /// 参数由 `history_v2::consumer` 或详情面板按 reducer 分配的身份构造；本方法
    /// 不做二次校验，也不在失败时降级到 V1。
    pub(crate) async fn history_v2_request(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, AcpError> {
        self.transport
            .send_request(peri_acp_types::history_v2::HISTORY_METHOD, params)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kit::history_paging::{
        HistoryBodyRequest, WINDOW_DEFAULT_BYTE_BUDGET, WindowDirection, WindowOpenRequest,
        WindowRequestAnchor,
    };
    use crate::kit::history_window::HistorySource;
    use peri_acp::transport::{mpsc::mpsc_transport_pair, types::IncomingMessage};
    use serde_json::json;

    #[tokio::test]
    async fn load_history_window_sends_only_history_field() {
        let (client_transport, server_transport) = mpsc_transport_pair();
        let (client, _tx, _rx) = AcpTuiClient::new(client_transport);
        let request = WindowRequest::Open(WindowOpenRequest {
            anchor: WindowRequestAnchor::Latest,
            direction: WindowDirection::Before,
            limit: 50,
            before_limit: None,
            after_limit: None,
            byte_budget: WINDOW_DEFAULT_BYTE_BUDGET,
        });
        let server = tokio::spawn(async move {
            let IncomingMessage::Request {
                id, method, params, ..
            } = server_transport.recv().await.expect("request")
            else {
                panic!("expected request");
            };
            assert_eq!(method, "session/load");
            assert_eq!(params["sessionId"], "s1");
            assert_eq!(params["history"]["kind"], "open");
            for legacy in ["cursor", "limit", "loadGeneration", "cwd", "mcpServers"] {
                assert!(
                    params.get(legacy).is_none(),
                    "legacy field leaked: {legacy}"
                );
            }
            server_transport
                .send_response(id, Ok(json!({ "sessionId": "s1" })))
                .await
                .expect("response");
        });
        let response = client
            .load_history_window("s1", &request)
            .await
            .expect("window request");
        assert_eq!(response["sessionId"], "s1");
        server.await.expect("server");
    }

    #[tokio::test]
    async fn load_history_body_sends_bounded_range_request() {
        let (client_transport, server_transport) = mpsc_transport_pair();
        let (client, _tx, _rx) = AcpTuiClient::new(client_transport);
        let request = HistoryBodyRequest {
            entry_id: "s1:m1".into(),
            source: HistorySource::Own,
            snapshot_token: "snap-1".into(),
            read_ticket: "ticket-1".into(),
            offset: 0,
            max_bytes: 65536,
        };
        let server = tokio::spawn(async move {
            let IncomingMessage::Request {
                id, method, params, ..
            } = server_transport.recv().await.expect("request")
            else {
                panic!("expected request");
            };
            assert_eq!(method, "session/history/body");
            assert_eq!(params["sessionId"], "s1");
            assert_eq!(params["entryId"], "s1:m1");
            assert_eq!(params["source"], "own");
            assert_eq!(params["snapshotToken"], "snap-1");
            assert_eq!(params["readTicket"], "ticket-1");
            assert_eq!(params["offset"], 0);
            assert_eq!(params["maxBytes"], 65536);
            server_transport
                .send_response(id, Ok(json!({ "entryId": "s1:m1", "offset": 0, "bytes": [], "nextOffset": null, "complete": true })))
                .await
                .unwrap();
        });
        let response = client
            .load_history_body("s1", &request)
            .await
            .expect("body");
        assert_eq!(response["complete"], true);
        server.await.expect("server");
    }

    #[tokio::test]
    async fn load_history_body_rejects_invalid_bounds() {
        let (client_transport, _server_transport) = mpsc_transport_pair();
        let (client, _tx, _rx) = AcpTuiClient::new(client_transport);
        let request = HistoryBodyRequest {
            entry_id: "s1:m1".into(),
            source: HistorySource::Own,
            snapshot_token: "snap-1".into(),
            read_ticket: "ticket-1".into(),
            offset: 0,
            max_bytes: 0,
        };
        assert_eq!(
            client
                .load_history_body("s1", &request)
                .await
                .unwrap_err()
                .code,
            -32602
        );
    }

    #[tokio::test]
    async fn load_history_window_rejects_invalid_shape_before_dispatch() {
        let (client_transport, _server_transport) = mpsc_transport_pair();
        let (client, _tx, _rx) = AcpTuiClient::new(client_transport);
        let request = WindowRequest::Open(WindowOpenRequest {
            anchor: WindowRequestAnchor::Latest,
            direction: WindowDirection::Before,
            limit: 0,
            before_limit: None,
            after_limit: None,
            byte_budget: WINDOW_DEFAULT_BYTE_BUDGET,
        });
        let error = client
            .load_history_window("s1", &request)
            .await
            .expect_err("invalid limit");
        assert_eq!(error.code, -32602);
    }
}
