//! 冷 child 恢复的终态结算：本次 turn 结束后把 child 终态、工作意图与结果通知
//! 一次提交到父 thread 的持久队列（设计 §5.3）。
//!
//! 通知按普通后台结果的 `ensure_processing` 策略入队：它既是结果事实，也允许
//! 把收到结果的父级从 `idle` 推进到 `working`。live 且已激活的父级在入队后收到
//! 唤醒；其余父级下一次进入时消费同一份持久行。收件人固定为 child 的直接父
//! `parent_thread_id`（保留的持久父链），不按显式 resume 的调用者改投。

use peri_acp_types::messages::MessageId;
use peri_acp_types::session::{MessageKind, MessagePolicy, MessageSource, QueuedMessage};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminder, TrustedSystemReminderFactory,
    SYSTEM_REMINDER_VERSION,
};
use peri_acp_types::thread::{AgentStatus, ThreadId, WorkState};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::super::{AcpServerConfig, SharedSessions};
use super::runtime_binding::{self, RecoveryTaskTerminal};
use crate::session::RecoveryDelivery;

/// 本次恢复 turn 的结束事实（来自 continuation dispatch 的结果）。
pub(crate) enum ChildTurnOutcome<'a> {
    /// turn 已结束；`stop_reason` 是 ACP 停止原因的 snake_case 字面量。
    Finished {
        stop_reason: &'a str,
    },
    Failed {
        code: i64,
        message: &'a str,
    },
}

impl ChildTurnOutcome<'_> {
    /// 通知里的 `state`：正常结束写 `completed`，其余如实写停止原因。
    fn state(&self) -> &str {
        match self {
            Self::Finished { stop_reason } if *stop_reason == "end_turn" => "completed",
            Self::Finished { stop_reason } => stop_reason,
            Self::Failed { .. } => "failed",
        }
    }

    /// child 的持久终态（设计 §5.3）：取消保留停止意图，失败是 Error，其余收尾为
    /// Done；`work_state` 由 [`WorkState::after_agent_terminal`] 统一派生。
    fn agent_status(&self) -> AgentStatus {
        match self {
            Self::Finished { stop_reason } if *stop_reason == "cancelled" => AgentStatus::Cancelled,
            Self::Finished { .. } => AgentStatus::Done,
            Self::Failed { .. } => AgentStatus::Error,
        }
    }
}

/// 一次 prompt dispatch 的结果 → 通知的结束事实。
///
/// 停止原因取自 ACP 响应（`stopReason`），失败保留协议错误码与消息。
pub(crate) fn turn_outcome(
    result: &Result<serde_json::Value, crate::transport::types::AcpError>,
) -> ChildTurnOutcome<'_> {
    match result {
        Ok(value) => ChildTurnOutcome::Finished {
            stop_reason: value
                .get("stopReason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown"),
        },
        Err(error) => ChildTurnOutcome::Failed {
            code: error.code,
            message: &error.message,
        },
    }
}

/// 把一次恢复 turn 的结语结算到父 thread 的持久队列。
///
/// 终态、`work_state` 与结果消息共用一次 `finalize_child_terminal`
/// （设计 §5.3）：提交成功前不宣称已投递，重复结算按稳定消息身份幂等。
/// 收件人固定为持久父链上的直接父（`delivery.parent`），不按显式 resume 的
/// 调用者改投；结算后父级任务登记也必须收尾。
pub(crate) async fn deliver_child_turn(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    child_session_id: &str,
    delivery: &RecoveryDelivery,
    outcome: ChildTurnOutcome<'_>,
) {
    let parent = &delivery.parent;
    let child_thread_id = {
        let states = sessions.lock().await;
        states
            .get(child_session_id)
            .map(|state| state.thread_id.clone())
    }
    .unwrap_or_else(|| child_session_id.to_owned());
    let agent_name = cfg
        .session_resources
        .load_session_meta(&child_thread_id)
        .await
        .ok()
        .and_then(|meta| meta.title);
    let reminder =
        match child_result_reminder(&child_thread_id, parent, agent_name.as_deref(), &outcome) {
            Ok(reminder) => reminder,
            Err(detail) => {
                tracing::error!(
                    child_thread_id = %child_thread_id,
                    parent_thread_id = %parent,
                    %detail,
                    "recovered child result notification was rejected"
                );
                runtime_binding::finish_recovered_child(cfg, delivery, None);
                return;
            }
        };
    let status = outcome.agent_status();
    let message = QueuedMessage::system_reminder_with_delivery_id(
        MessageKind::Defer,
        MessageSource::SubAgentComplete,
        reminder,
        settlement_delivery_id(&child_thread_id, parent, delivery.seed),
    )
    .with_policy(MessagePolicy::ensure_processing());
    let committed = match cfg
        .session_resources
        .finalize_child_terminal(
            &child_thread_id,
            status,
            WorkState::after_agent_terminal(status),
            parent,
            message.clone().into_mq_input(),
        )
        .await
    {
        Ok(_) => true,
        Err(error) => {
            tracing::error!(
                child_thread_id = %child_thread_id,
                parent_thread_id = %parent,
                %error,
                "recovered child terminal settlement was not committed"
            );
            false
        }
    };
    if committed {
        // live 父级沿普通后台结果的唤醒路径处理同一份持久行；父级未 live 时
        // 保留 durable 行，由下一次进入消费。
        wake_live_parent(cfg, parent, message).await;
    }
    let terminal = if !committed {
        None
    } else {
        match &outcome {
            ChildTurnOutcome::Finished { stop_reason } if *stop_reason == "cancelled" => {
                Some(RecoveryTaskTerminal::Cancelled)
            }
            ChildTurnOutcome::Finished { stop_reason } => Some(RecoveryTaskTerminal::Completed {
                success: *stop_reason == "end_turn",
                output: format!("recovered child finished: {stop_reason}"),
            }),
            ChildTurnOutcome::Failed { code, message } => Some(RecoveryTaskTerminal::Completed {
                success: false,
                output: format!("recovered child failed: {code}: {message}"),
            }),
        }
    };
    runtime_binding::finish_recovered_child(cfg, delivery, terminal);
}

/// 唤醒 live 父级：同一稳定 ID 的重放只会得到 `Existing`/`AlreadyHandedOff`，
/// 但会让父级 MQ 发布一次唤醒（普通后台结果的回调路径相同）。
async fn wake_live_parent(cfg: &AcpServerConfig, parent: &ThreadId, message: QueuedMessage) {
    let Some(parent_session_id) = cfg.session_manager.session_id_for_thread(parent) else {
        return;
    };
    let Some(queue) = cfg.session_manager.v2_queue_for(&parent_session_id) else {
        return;
    };
    if let Err(error) = queue.enqueue(message).await {
        tracing::warn!(
            parent_thread_id = %parent,
            %error,
            "recovered child result wake was not published"
        );
    }
}

/// 同一次恢复 turn 的稳定投递身份：重复结算不产生第二条结果。
///
/// 身份取 child、直接父 thread 与本次恢复提醒的稳定 ID，不取正文/停止原因。
/// 同一 child 再次被恢复时会拿到新提醒，结果身份随之改变；同一恢复的重复结算
/// 才复用同一 ID（设计 §5.3）。
fn settlement_delivery_id(
    child_thread_id: &str,
    parent: &ThreadId,
    seed: Option<MessageId>,
) -> MessageId {
    let mut hasher = Sha256::new();
    for part in ["peri-recovered-child-terminal", child_thread_id, parent] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    if let Some(seed) = seed {
        hasher.update(seed.as_uuid().as_bytes());
    }
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    MessageId::from(uuid::Uuid::from_bytes(bytes))
}

fn child_result_reminder(
    child_thread_id: &str,
    parent: &ThreadId,
    agent_name: Option<&str>,
    outcome: &ChildTurnOutcome<'_>,
) -> Result<TrustedSystemReminder, String> {
    let state = outcome.state();
    let mut body = String::from("后台任务结果\n");
    body.push_str(&format!("task_id: {child_thread_id}\n"));
    body.push_str(&format!("agent: {}\n", agent_name.unwrap_or("subagent")));
    body.push_str(&format!("state: {state}\n"));
    match outcome {
        ChildTurnOutcome::Finished { stop_reason } => {
            body.push_str(&format!("stop_reason: {stop_reason}\n"));
        }
        ChildTurnOutcome::Failed { code, message } => {
            body.push_str(&format!("diagnostic: {code}: {message}\n"));
        }
    }
    body.push_str(&format!(
        "child_thread_id: {child_thread_id} (resume with Agent(resume_thread_id: {child_thread_id}))\n"
    ));
    body.push_str(
        "notice: 该子会话在进入会话的恢复中继续执行，本轮已结束；完整正文在其自身 thread 的历史中。",
    );
    let (kind, severity) = match outcome {
        ChildTurnOutcome::Finished { stop_reason } if *stop_reason == "end_turn" => {
            ("completed", ReminderSeverity::Info)
        }
        // 取消、上限等非正常终止如实成为 kind，不折叠成「完成」。
        ChildTurnOutcome::Finished { stop_reason } => (*stop_reason, ReminderSeverity::Warning),
        ChildTurnOutcome::Failed { .. } => ("failed", ReminderSeverity::Error),
    };
    let reminder = SystemReminder {
        version: SYSTEM_REMINDER_VERSION,
        category: ReminderCategory::Task,
        source: ReminderSource("subagent".to_owned()),
        kind: kind.to_owned(),
        severity,
        delivery: ReminderDelivery::Configurable,
        audiences: ReminderAudiences(vec![
            ReminderAudience::Model,
            ReminderAudience::Tui,
            ReminderAudience::Automation,
        ]),
        body,
        summary: Some(format!(
            "Recovered child thread {child_thread_id} finished a turn"
        )),
        metadata: json!({
            "task_id": child_thread_id,
            "child_thread_id": child_thread_id,
            "parent_thread_id": parent,
            "state": state,
            "recovered": true,
        }),
    };
    TrustedSystemReminderFactory::for_producer()
        .construct(reminder)
        .map_err(|error| error.to_string())
}
