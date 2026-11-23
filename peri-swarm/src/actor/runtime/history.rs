//! Read-only ACP windows for the actor history helper. An initial setup
//! (uncursored session/load) would re-enter the recovery coordinator.
use super::AcpTransport;
use super::{AcpChild, ChildError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const CURSOR_PREFIX: &str = "window:";

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct BeforePage {
    snapshot_token: String,
    read_ticket: String,
    before_cursor: String,
}

impl AcpChild {
    pub async fn load_initial(&self, limit: Option<u32>) -> Result<Value, ChildError> {
        if limit.is_some() {
            return Err(ChildError::Rpc {
                code: -32602,
                message: "limit requires a cursor".into(),
            });
        }
        self.history_window(json!({"kind":"open","direction":"before","anchor":{"kind":"latest"}}))
            .await
    }

    pub async fn load_page(&self, cursor: &str, limit: Option<u32>) -> Result<Value, ChildError> {
        if let Some(encoded) = cursor.strip_prefix(CURSOR_PREFIX) {
            let page: BeforePage = serde_json::from_str(encoded).map_err(|_| ChildError::Rpc {
                code: -32602,
                message: "invalid history page cursor".into(),
            })?;
            return self
                .history_window(json!({
                    "kind":"page","snapshotToken":page.snapshot_token,"readTicket":page.read_ticket,
                    "beforeCursor":page.before_cursor,"limit":limit.unwrap_or(50),
                }))
                .await;
        }
        // A cursor issued by the older pagination contract remains read-only.
        let mut params = json!({ "sessionId": self.session_id, "cursor": cursor });
        if let Some(limit) = limit {
            params["limit"] = json!(limit);
        }
        self.client
            .send_request("session/load", params)
            .await
            .map_err(ChildError::rpc)
    }

    async fn history_window(&self, history: Value) -> Result<Value, ChildError> {
        let mut response = self
            .client
            .send_request(
                "session/load",
                json!({"sessionId":self.session_id,"history":history}),
            )
            .await
            .map_err(ChildError::rpc)?;
        if let Some(before) = response["beforeCursor"].as_str() {
            let page = BeforePage {
                snapshot_token: response["snapshotToken"]
                    .as_str()
                    .ok_or_else(|| ChildError::Local("window has no snapshot token".into()))?
                    .to_owned(),
                read_ticket: response["readTicket"]
                    .as_str()
                    .ok_or_else(|| ChildError::Local("window has no read ticket".into()))?
                    .to_owned(),
                before_cursor: before.to_owned(),
            };
            let encoded =
                serde_json::to_string(&page).map_err(|err| ChildError::Local(err.to_string()))?;
            response["nextCursor"] = json!(format!("{CURSOR_PREFIX}{encoded}"));
        }
        Ok(response)
    }
}
