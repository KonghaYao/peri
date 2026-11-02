//! Read a descendant's canonical timeline without loading an executable session.

use peri_acp::transport::{AcpTransport, types::AcpError};
use serde_json::{Value, json};

use crate::kit::history_paging::{HistoryBodyRequest, WindowRequest};

use super::AcpTuiClient;

impl AcpTuiClient {
    /// The response belongs to `child_thread_id` and can contain multiple runs.
    pub(crate) async fn load_child_history_window(
        &self,
        parent_session_id: &str,
        child_thread_id: &str,
        request: &WindowRequest,
    ) -> Result<Value, AcpError> {
        request
            .validate()
            .map_err(|error| AcpError::new(-32602, error.to_string()))?;
        self.transport
            .send_request(
                "session/history/child",
                json!({
                    "parentSessionId": parent_session_id,
                    "childThreadId": child_thread_id,
                    "history": request.to_value(),
                }),
            )
            .await
    }

    pub(crate) async fn load_child_history_body(
        &self,
        parent_session_id: &str,
        child_thread_id: &str,
        request: &HistoryBodyRequest,
    ) -> Result<Value, AcpError> {
        request
            .validate()
            .map_err(|error| AcpError::new(-32602, error.to_string()))?;
        let mut params = request.to_value();
        params["parentSessionId"] = json!(parent_session_id);
        params["childThreadId"] = json!(child_thread_id);
        self.transport
            .send_request("session/history/child/body", params)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kit::history_paging::{
        WINDOW_DEFAULT_BYTE_BUDGET, WindowDirection, WindowOpenRequest, WindowRequestAnchor,
    };
    use crate::kit::history_window::HistorySource;
    use peri_acp::transport::{mpsc::mpsc_transport_pair, types::IncomingMessage};

    #[tokio::test]
    async fn child_history_requests_never_use_session_load() {
        let (client_transport, server_transport) = mpsc_transport_pair();
        let (client, _tx, _rx) = AcpTuiClient::new(client_transport);
        let server = tokio::spawn(async move {
            for expected in ["session/history/child", "session/history/child/body"] {
                let IncomingMessage::Request {
                    id, method, params, ..
                } = server_transport.recv().await.unwrap()
                else {
                    panic!("expected request");
                };
                assert_eq!(method, expected);
                assert_eq!(params["parentSessionId"], "parent");
                assert_eq!(params["childThreadId"], "child");
                for field in ["sessionId", "cwd", "mcpServers", "loadGeneration"] {
                    assert!(
                        params.get(field).is_none(),
                        "unexpected setup field {field}"
                    );
                }
                server_transport
                    .send_response(id, Ok(json!({"ok": true})))
                    .await
                    .unwrap();
            }
        });
        let request = WindowRequest::Open(WindowOpenRequest {
            anchor: WindowRequestAnchor::Latest,
            direction: WindowDirection::Before,
            limit: 20,
            before_limit: None,
            after_limit: None,
            byte_budget: WINDOW_DEFAULT_BYTE_BUDGET,
        });
        client
            .load_child_history_window("parent", "child", &request)
            .await
            .unwrap();
        client
            .load_child_history_body(
                "parent",
                "child",
                &HistoryBodyRequest {
                    entry_id: "child:message".into(),
                    source: HistorySource::Own,
                    snapshot_token: "snapshot".into(),
                    read_ticket: "ticket".into(),
                    offset: 0,
                    max_bytes: 1024,
                },
            )
            .await
            .unwrap();
        server.await.unwrap();
    }
}
