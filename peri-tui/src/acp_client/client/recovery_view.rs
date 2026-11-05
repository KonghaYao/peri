//! `session/load` 响应中的恢复结果：解码与用户可见呈现。
//!
//! 客户端不复制服务端的执行阻断状态；这里只把 `_meta.peri.recovery` 投影成通知。
//! 真正的执行拒绝以服务端 `-32017` 为准。

use peri_acp_types::event_data::SystemNotification;
use peri_acp_types::recovery::{RecoveryView, RecoveryViewStatus};
use serde_json::Value;

use crate::kit::acp_types::{AcpEventData, AcpEventWithEpoch};
use crate::kit::atoms::{BRIDGE_RESET_COUNTER, LOCAL_EVENT_TX, NOTIFICATION, Notification};

const RECOVERY_NOTICE_DURATION: std::time::Duration = std::time::Duration::from_secs(30);

/// 解析 `_meta["peri.recovery"]`；格式不认识只告警，不影响 load 成败。
pub(super) fn decode(loaded: &Value) -> Option<RecoveryView> {
    let value = loaded.get("_meta")?.get("peri.recovery")?;
    match serde_json::from_value(value.clone()) {
        Ok(view) => Some(view),
        Err(error) => {
            tracing::warn!(%error, "ignoring malformed _meta.peri.recovery");
            None
        }
    }
}

/// 恢复不是完全成功时给出持久可见的系统注记（同时保留 30s toast）。
pub(super) fn present(session_id: &str, view: &RecoveryView) {
    let (key, args, level) = match view.status {
        RecoveryViewStatus::Ok => return,
        RecoveryViewStatus::Failed => {
            let kind = view
                .error
                .as_ref()
                .map(|error| error.kind.as_str())
                .unwrap_or("unavailable");
            let detail = view
                .error
                .as_ref()
                .map(|error| error.detail.as_str())
                .unwrap_or_default();
            let key = if view.execution_blocked {
                "session-recovery-blocked"
            } else {
                "session-recovery-failed-retryable"
            };
            (
                key,
                vec![
                    ("kind".to_owned(), kind.into()),
                    ("detail".to_owned(), detail.into()),
                ],
                if view.execution_blocked {
                    "error"
                } else {
                    "warning"
                },
            )
        }
        RecoveryViewStatus::Partial => (
            "session-recovery-partial",
            vec![(
                "count".to_owned(),
                view.failures.len().max(1).to_string().into(),
            )],
            "warning",
        ),
    };
    let message = crate::i18n::tr_args(key, &args);
    NOTIFICATION.set(Some(Notification {
        message: message.clone(),
        until: peri_time::monotonic_now() + RECOVERY_NOTICE_DURATION,
    }));
    // 启动早期 bridge 可能尚未接线：此时只保留 toast，不阻塞 load。
    if let Some(tx) = LOCAL_EVENT_TX.get() {
        let _ = tx.send(AcpEventWithEpoch {
            child_origin: None,
            view_epoch: BRIDGE_RESET_COUNTER.get(),
            event: AcpEventData::SystemNotification(SystemNotification {
                text: message,
                level: level.to_owned(),
            }),
            active_session_id: session_id.to_owned(),
        });
    }
}

#[cfg(test)]
#[path = "recovery_view_test.rs"]
mod tests;
