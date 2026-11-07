//! Closing the previous session and remembering incomplete closes.

use peri_acp::transport::{AcpTransport, types::AcpError};
use serde_json::json;

use crate::acp_client::client::AcpTuiClient;

/// ACP 明示的 close incomplete：旧会话资源尚未释放，同一 `session/close` 可重试。
const INCOMPLETE_CLOSE_CODE: i64 = -32010;

/// 一个尚未关闭成功的旧会话，以及失败时被打断的切换目标。
///
/// 关闭失败后 lifecycle 只留下 `NoSession`，旧身份随之消失；没有这份记录，下一次
/// 输入会绕过关闭直接 `session/new`——服务端的旧会话可能仍在 Closing、资源未释放，
/// 而用户原本要加载的历史会被静默换成空会话。只有 `-32010` 属于可重试语义，其余
/// 错误没有可行路径，不保留身份（`from_close_failure`）。
#[derive(Debug)]
pub(in crate::acp_client::client) struct PendingClose {
    /// 必须先关闭成功才能建立新会话的旧身份。
    pub(super) session_id: String,
    /// 失败时用户原本要加载的会话；`Some` 时下一次建立会话先完成这次加载，
    /// 而不是新建空会话。新建路径为 `None`：重试同一路径自然回到「新建」。
    pub(super) resume_load: Option<String>,
}

impl PendingClose {
    fn from_close_failure(
        session_id: &str,
        resume_load: Option<String>,
        error: &AcpError,
    ) -> Option<Self> {
        (error.code == INCOMPLETE_CLOSE_CODE).then(|| Self {
            session_id: session_id.to_owned(),
            resume_load,
        })
    }
}

impl AcpTuiClient {
    /// Close the previous session before switching identity.
    ///
    /// `-32010`（close incomplete）是 ACP 明示的可重试错误：在预算内重放同一
    /// 请求，与 `@peri-sdk` 的 `closeUntilSettled` 同一语义。重放耗尽仍不完整
    /// 时返回最后一次错误；调用方保留待关闭身份并保持可重试，不把它写成粘滞
    /// `restore_error`。
    pub(super) async fn close_previous_session(&self, session_id: &str) -> Result<(), AcpError> {
        const INCOMPLETE_CLOSE_CODE: i64 = -32010;
        const DRAIN_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);
        const RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
        let deadline = std::time::Instant::now() + DRAIN_BUDGET;
        loop {
            let params = json!({ "sessionId": session_id });
            match self.transport.send_request("session/close", params).await {
                Ok(_) => return Ok(()),
                Err(error) => {
                    let now = std::time::Instant::now();
                    if error.code != INCOMPLETE_CLOSE_CODE || now >= deadline {
                        return Err(error);
                    }
                    tokio::time::sleep(RETRY_INTERVAL.min(deadline - now)).await;
                }
            }
        }
    }

    /// 记录一次关闭失败：只有 `-32010` 保留待关闭身份（其余错误没有重试路径）。
    pub(super) fn remember_close_failure(
        &self,
        session_id: &str,
        resume_load: Option<String>,
        error: &AcpError,
    ) {
        *self.pending_close.lock().unwrap() =
            PendingClose::from_close_failure(session_id, resume_load, error);
    }

    pub(super) fn clear_pending_close(&self) {
        *self.pending_close.lock().unwrap() = None;
    }

    /// 尚未关闭成功的旧会话身份；存在时下一次建立会话必须先关闭它。
    pub(super) fn pending_close_target(&self) -> Option<String> {
        self.pending_close
            .lock()
            .unwrap()
            .as_ref()
            .map(|pending| pending.session_id.clone())
    }

    /// 被关闭失败打断的加载目标；存在时下一次建立会话先完成这次加载。
    pub(super) fn pending_resume_load(&self) -> Option<String> {
        self.pending_close
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|pending| pending.resume_load.clone())
    }
}
