//! Host-only completion facts. ACP response content remains unchanged.

use std::sync::Arc;

use serde_json::Value;

use crate::{host, session::executor, transport::types::AcpError};

pub(in crate::host) struct PromptOutcome {
    pub response: Value,
    pub execution_binding: Option<peri_acp_types::session::ExecutionBinding>,
}

/// 一次 prompt 尝试：响应结果 + 是否已把正文交给执行器并取得终态。
pub(in crate::host) struct PromptAttempt {
    pub outcome: Result<PromptOutcome, AcpError>,
    /// 执行器已启动并产出终态（成功与失败路径都保留该事实）。
    ///
    /// 恢复来源据此区分「从未启动」与「执行后才失败/被停止」：只有前者是启动
    /// 失败，后者必须按执行结果结算，不能跳过父级终态与结果投递。
    pub execution_started: bool,
}

#[allow(clippy::too_many_arguments)]
pub(in crate::host) async fn run_prompt_with_outcome(
    mut params: Value,
    sessions: &host::SharedSessions,
    deployment: &host::AcpServerConfig,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    pool: Arc<parking_lot::Mutex<crate::session::agent_pool::AgentPool>>,
    cont_tx: Option<tokio::sync::mpsc::UnboundedSender<executor::ContinuationRequest>>,
    continuation: bool,
    input_ticket: Option<host::user_input::UserInputRun>,
) -> PromptAttempt {
    let notifications = match host::execution::ExecutionNotifications::from_params(&mut params) {
        Ok(notifications) => notifications,
        Err(error) => {
            return PromptAttempt {
                outcome: Err(error),
                execution_started: false,
            }
        }
    };
    let session_id = host::extract_session_id(&params, "").to_owned();
    let attempt_activation = match deployment.session_manager.get_session(&session_id) {
        Some(runtime) => {
            let activation = Arc::clone(&runtime.activation);
            let queue = runtime.v2_message_queue.clone();
            let result_watermark = runtime.async_result_mailbox.admission_watermark();
            drop(runtime);
            let watermark = queue.admission_watermark().await.unwrap_or(None);
            Some((activation, watermark, result_watermark))
        }
        None => None,
    };
    let mut execution_started = false;
    let result = super::run_prompt_attempt(
        params,
        sessions,
        deployment,
        transport,
        pool,
        cont_tx,
        continuation,
        input_ticket,
        &notifications,
        attempt_activation.as_ref(),
        &mut execution_started,
    )
    .await;
    if result.is_err() {
        if let Some((activation, watermark, result_watermark)) = &attempt_activation {
            activation.record_failed_attempt(watermark.clone());
            activation.record_failed_result_watermark(*result_watermark);
        }
        notifications
            .finish_early(&session_id, "error", deployment, transport)
            .await;
    }
    PromptAttempt {
        outcome: result,
        execution_started,
    }
}
