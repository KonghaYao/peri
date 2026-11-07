//! Session deletion and its local identity cleanup.

use peri_acp::transport::{AcpTransport, types::AcpError};
use serde_json::json;

use crate::acp_client::client::{AcpTuiClient, ClientProjectionMode};
use crate::acp_client::interaction_lifecycle::TransitionKind;

impl AcpTuiClient {
    /// Delete a session from history (standard ACP `session/delete`).
    ///
    /// 遵守 agentclientprotocol.com/protocol/v1/session-delete：`{ sessionId }`
    /// 请求、`{}` 响应；删除后会话不再出现在 `session/list` 中且无法
    /// `session/load`。若删除的是当前活跃会话，本地事实源一并清空
    /// （服务端会 cancel 该会话的 in-flight turn 并级联删除消息）。
    ///
    /// M3：删除的会话 id 记入黑名单，pump 过滤其延迟通知（`current_session_id`
    /// 置 None 后"首次连接放行"语义会让已删除会话的事件回写 UI）。
    pub async fn delete_session(&self, session_id: &str) -> Result<(), AcpError> {
        let _operation = self.lifecycle.operation_gate().lock().await;
        let is_current = self
            .lifecycle
            .stable_identity()
            .as_ref()
            .is_some_and(|(id, _)| id == session_id);
        let transition = if is_current {
            self.project_execution_cwd(None);
            let start = self
                .lifecycle
                .begin_transition(TransitionKind::DeleteCurrent, None)
                .map_err(|message| AcpError::new(-32603, message))?;
            let lease = self.lifecycle.arm_transition(start.generation);
            if self.projection_mode == ClientProjectionMode::Interactive {
                crate::kit::session_boundary::project_session_boundary(None);
            }
            self.settle_claims_owned(start.claims).await;
            Some((start.generation, lease))
        } else {
            None
        };
        let params = json!({ "sessionId": session_id });
        let result = self.transport.send_request("session/delete", params).await;
        match transition {
            Some((generation, lease)) => {
                self.lifecycle.fail_transition(generation);
                lease.disarm();
                if result.is_ok() {
                    self.lifecycle.mark_deleted(session_id);
                }
            }
            None if result.is_ok() => self.lifecycle.mark_deleted(session_id),
            None => {}
        }
        result.map(|_| ())
    }
}
