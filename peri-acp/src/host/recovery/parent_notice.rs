//! 冷 child 恢复的结果通知：本次 turn 结束后投递给父 thread 的持久队列。
//!
//! 通知按 `SubAgentComplete` 语义入队，但使用 `passive` 策略：它是结果事实，
//! 不是执行授权。父级 `idle` 不因它被唤醒；父级下次执行时由既有 Receive 消费
//! 同一份持久行（父级已有活实例时也是这份队列）。

use std::sync::Arc;

use peri_acp_types::session::{
    MessageKind, MessagePolicy, MessageQueue, MessageSource, QueuedMessage,
};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminder, TrustedSystemReminderFactory,
    SYSTEM_REMINDER_VERSION,
};
use peri_acp_types::thread::ThreadId;
use serde_json::json;

use super::super::{AcpServerConfig, SharedSessions};

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

/// 把一次恢复 turn 的结语投递到父 thread 的 MQ。
///
/// 投递失败只告警：结果已提交在 child 自己的历史里，父级下次进入仍可读到。
pub(crate) async fn deliver_child_turn(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    child_session_id: &str,
    parent: &ThreadId,
    outcome: ChildTurnOutcome<'_>,
) {
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
                tracing::warn!(
                    child_thread_id = %child_thread_id,
                    parent_thread_id = %parent,
                    %detail,
                    "recovered child result notification was rejected"
                );
                return;
            }
        };
    let message = QueuedMessage::system_reminder(
        MessageKind::Defer,
        MessageSource::SubAgentComplete,
        reminder,
    )
    .with_policy(MessagePolicy::passive());
    let queue = parent_queue(cfg, parent);
    if let Err(error) = queue.enqueue(message).await {
        tracing::warn!(
            child_thread_id = %child_thread_id,
            parent_thread_id = %parent,
            %error,
            "recovered child result notification was not admitted"
        );
    }
}

/// 父 thread 的持久队列：父级有活实例时复用同一份门面，否则只绑定同一 thread。
fn parent_queue(cfg: &AcpServerConfig, parent: &ThreadId) -> MessageQueue {
    cfg.session_manager
        .session_id_for_thread(parent)
        .and_then(|session_id| cfg.session_manager.v2_queue_for(&session_id))
        .unwrap_or_else(|| MessageQueue::bind(Arc::clone(&cfg.session_resources), parent.clone()))
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
