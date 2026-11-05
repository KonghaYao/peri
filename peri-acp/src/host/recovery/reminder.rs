//! 恢复提醒：可信 SystemReminder 的正文与接纳语义（设计 §4.1）。
//!
//! 提醒经 MQ 持久接纳、由既有 Receive 移交；它不是新的用户委派，也不是权限凭证。

use peri_acp_types::ports::PeripheralTasks;
use peri_acp_types::session::{
    MessageKind, MessagePolicy, MessageQueue, MessageSource, MqEntry, MqFilter, QueuedMessage,
};
use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminder, TrustedSystemReminderFactory,
    SYSTEM_REMINDER_VERSION,
};
use peri_acp_types::thread::ThreadId;
use serde_json::json;

/// 恢复提醒的来源字面量；MQ 去重按同一个来源查询。
pub(super) const RECOVERY_REMINDER_SOURCE: &str = "session_recovery";

/// 构造进入 thread 的恢复提醒。
///
/// 任务列表只写本 thread 直接 `working` 子候选的 thread id；外围任务按该 thread
/// 的直接 owner 查询结果如实表达——`Unknown` 是「无法确认」，不是「没有任务」。
pub(super) fn recovery_reminder(
    root_thread_id: &ThreadId,
    thread_id: &ThreadId,
    direct_working_children: &[ThreadId],
    peripheral_tasks: &PeripheralTasks,
) -> Result<TrustedSystemReminder, String> {
    let mut body = String::from("会话从中断后的已提交历史继续。旧工具调用不会自动重放。\n");
    if direct_working_children.is_empty() {
        body.push_str("本会话当前没有直接处于 working 的子任务。\n");
    } else {
        body.push_str("本会话当前仍为 working 的直接子任务：\n");
        for child in direct_working_children {
            body.push_str(&format!("- {child}\n"));
        }
    }
    let task_liveness = match peripheral_tasks {
        PeripheralTasks::Known(tasks) if tasks.is_empty() => {
            body.push_str("外围任务：已确认当前没有仍存活的 Shell 任务。\n");
            "known"
        }
        PeripheralTasks::Known(tasks) => {
            body.push_str(&format!("外围任务：{} 个仍存活。\n", tasks.len()));
            for task in tasks {
                body.push_str(&format!("- {}：{}\n", task.task_id, task.state));
            }
            "known"
        }
        PeripheralTasks::Unknown { .. } => {
            body.push_str("外围任务归属与存活状态：未知（无法确认外围任务是否仍在运行）。");
            "unknown"
        }
    };
    let reminder = SystemReminder {
        version: SYSTEM_REMINDER_VERSION,
        category: ReminderCategory::Lifecycle,
        source: ReminderSource(RECOVERY_REMINDER_SOURCE.to_owned()),
        kind: "entry".to_owned(),
        severity: ReminderSeverity::Warning,
        delivery: ReminderDelivery::Required,
        audiences: ReminderAudiences(vec![
            ReminderAudience::Model,
            ReminderAudience::Tui,
            ReminderAudience::Automation,
        ]),
        body,
        summary: Some("Session resumed from committed history".to_owned()),
        metadata: json!({
            "root_thread_id": root_thread_id,
            "thread_id": thread_id,
            "candidate_thread_ids": direct_working_children,
            "task_liveness": task_liveness,
            "peripheral_tasks": match peripheral_tasks {
                PeripheralTasks::Known(tasks) => serde_json::Value::Array(
                    tasks
                        .iter()
                        .map(|task| json!({
                            "task_id": task.task_id,
                            "owner": task.owner,
                            "state": task.state,
                        }))
                        .collect(),
                ),
                PeripheralTasks::Unknown { detail } => json!({ "unknown": detail }),
            },
        }),
    };
    TrustedSystemReminderFactory::for_recovery()
        .construct(reminder)
        .map_err(|error| error.to_string())
}

/// 逐项失败的诊断提醒：只给 Tui / Automation，不交给模型，也不唤醒执行。
pub(super) fn failure_notice(
    root_thread_id: &ThreadId,
    failures: &[peri_acp_types::recovery::RecoveryFailure],
) -> Result<TrustedSystemReminder, String> {
    let body = failures
        .iter()
        .map(|failure| {
            format!(
                "- {}：{}（{}）",
                failure.thread_id,
                failure.kind.as_str(),
                failure.detail
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let reminder = SystemReminder {
        version: SYSTEM_REMINDER_VERSION,
        category: ReminderCategory::Lifecycle,
        source: ReminderSource(RECOVERY_REMINDER_SOURCE.to_owned()),
        kind: "entry_failures".to_owned(),
        severity: ReminderSeverity::Warning,
        delivery: ReminderDelivery::Required,
        audiences: ReminderAudiences(vec![ReminderAudience::Tui, ReminderAudience::Automation]),
        body: format!("进入会话时有部分恢复准备失败，相关会话保留但不自动续跑：\n{body}"),
        summary: Some("Session recovery partially failed".to_owned()),
        metadata: json!({
            "root_thread_id": root_thread_id,
            "task_liveness": "unknown",
        }),
    };
    TrustedSystemReminderFactory::for_recovery()
        .construct(reminder)
        .map_err(|error| error.to_string())
}

/// 一条待消费的恢复 entry 提醒：只有 `Defer` + 本来源 + `kind = "entry"` 才算。
///
/// failure notice 是 `Info`，其它来源的提醒也不得复用为恢复资格。
pub(super) fn is_pending_recovery(entry: &MqEntry) -> bool {
    entry.kind == MessageKind::Defer
        && entry.source == MessageSource::SessionRecovery
        && matches!(
            &entry.payload,
            PersistedPayload::SystemReminder { reminder, .. }
                if reminder.as_reminder().kind == "entry"
        )
}

/// 本 thread MQ 中尚未被 Receive 消费的恢复 entry 提醒。
///
/// 存在只表示「本次可复用」，不再等价于「已注入」；防重复由协调器的 admitted 状态机
/// 负责。查询错误必须原样返回：调用方不能把「查不到」当成「没有提醒」。
pub(super) async fn find_pending_recovery(
    queue: &MessageQueue,
) -> SessionResourceResult<Option<MqEntry>> {
    // 不设行数上限：本线程的提醒可能有任意多条，固定上限会漏掉可复用的 entry
    // 并重复注入。来源与 Kind 已在存储侧收敛，扫描范围仍是本 thread 的恢复提醒。
    let rows = queue
        .query(MqFilter {
            sources: vec![MessageSource::SessionRecovery],
            kinds: vec![MessageKind::Defer],
            ..Default::default()
        })
        .await
        .map_err(|error| match error.downcast::<SessionResourceError>() {
            Ok(domain) => domain,
            Err(error) => SessionResourceError::new(SessionResourceErrorKind::Unavailable {
                detail: error.to_string(),
            }),
        })?;
    Ok(rows.into_iter().find(is_pending_recovery))
}

/// 提醒接纳：`Defer` + `ensure_processing`，由既有 Receive 移交。
pub(super) fn queued_recovery(trusted: TrustedSystemReminder) -> QueuedMessage {
    QueuedMessage::system_reminder(MessageKind::Defer, MessageSource::SessionRecovery, trusted)
        .with_policy(MessagePolicy::ensure_processing())
}

#[cfg(test)]
#[path = "reminder_test.rs"]
mod tests;
