//! Session rewind 命令 handler：rewind-candidates / rewind-preview / rewind
//! 与 rewind cap 校验（自 requests.rs 拆出，请求分发见 `host/requests.rs`）。

use std::collections::HashMap;
use std::sync::Arc;

use peri_acp_types::PeriCaps;
use serde_json::Value;

use super::super::{AcpServerConfig, SessionState};
use crate::{dispatch, transport::types::AcpError};

pub(super) fn handle_rewind_candidates(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    require_rewind_cap(&cfg.session_manager.get_caps(session_id))?;
    let history = sessions
        .get(session_id)
        .map(|s| s.history.clone())
        .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
    dispatch::rewind_candidates(&history)
}

pub(super) async fn handle_rewind_preview(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?
        .to_string();
    require_rewind_cap(&cfg.session_manager.get_caps(&session_id))?;
    let (cwd, history) = sessions
        .get(&session_id)
        .map(|s| (s.cwd.clone(), s.history.clone()))
        .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
    // Phase 5 Step 5：RewindError 变体删除，preview 为只读路径
    // 零事件——不再需要 event_sink。
    dispatch::rewind_preview(params, &history, &cwd, &session_id).await
}

fn apply_canonical_rewind(
    history_payloads: &mut Vec<peri_acp_types::store::PersistedPayload>,
    history: &mut Vec<peri_acp_types::messages::BaseMessage>,
    target_id: peri_acp_types::messages::MessageId,
) -> Result<(), AcpError> {
    let target_payload_idx = history_payloads
        .iter()
        .position(|payload| payload.id() == target_id)
        .ok_or_else(|| AcpError::new(-32603, "rewind target missing from canonical history"))?;
    history_payloads.truncate(target_payload_idx);
    *history = history_payloads
        .iter()
        .filter_map(|payload| payload.as_message().cloned())
        .collect();
    Ok(())
}

pub(super) async fn handle_rewind(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?
        .to_string();
    require_rewind_cap(&cfg.session_manager.get_caps(&session_id))?;
    let (cwd, history) = {
        let s = sessions
            .get_mut(&session_id)
            .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
        (s.cwd.clone(), s.history.clone())
    };
    let target_message_id = params
        .get("target_message_id")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::new(-32602, "missing target_message_id"))?;
    let target_id = history
        .iter()
        .find(|message| message.id().as_uuid().to_string() == target_message_id)
        .map(peri_acp_types::messages::BaseMessage::id)
        .ok_or_else(|| AcpError::new(-32602, "rewind target not found"))?;
    let event_sink: Arc<dyn crate::session::event_sink::EventSink> =
        Arc::new(crate::session::event_sink::TransportEventSink::new(
            transport.clone(), // transport: &Arc<dyn AcpTransport>（签名改动见下方实现注记）
            cfg.session_manager.caps_registry(),
        ));
    let peri_config_snapshot = Arc::new(cfg.peri_config.read().clone());
    let response = dispatch::rewind_execute(
        params,
        history,
        &cwd,
        &peri_config_snapshot,
        &event_sink,
        None, // auxiliary_model：RewindCommand 不使用
        &tokio_util::sync::CancellationToken::new(),
        cfg.controller.as_ref(),
        Some(session_id.clone()),
        None, // bg_event_tx
        None, // task_manager
        None,
        None,
        None,
        None, // frozen_*：RewindCommand 不使用
    )
    .await?;
    // Canonical payloads are authoritative. Locate the target by MessageId there, truncate them,
    // then derive the legacy message-only projection in one direction.
    if let Some(state) = sessions.get_mut(&session_id) {
        apply_canonical_rewind(&mut state.history_payloads, &mut state.history, target_id)?;
    }
    Ok(response)
}

fn require_rewind_cap(caps: &PeriCaps) -> Result<(), AcpError> {
    if caps.rewind {
        Ok(())
    } else {
        Err(AcpError::new(
            -32601,
            "peri.rewind capability not negotiated",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp_types::messages::BaseMessage;
    use peri_acp_types::store::PersistedPayload;
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };

    fn reminder(id: peri_acp_types::messages::MessageId) -> PersistedPayload {
        let reminder = TrustedSystemReminderFactory::for_producer()
            .construct(SystemReminder {
                version: SYSTEM_REMINDER_VERSION,
                category: ReminderCategory::Task,
                source: ReminderSource("rewind_test".into()),
                kind: "interleave".into(),
                severity: ReminderSeverity::Info,
                delivery: ReminderDelivery::Configurable,
                audiences: ReminderAudiences(vec![ReminderAudience::Model]),
                body: "reminder".into(),
                summary: None,
                metadata: serde_json::json!({}),
            })
            .unwrap();
        PersistedPayload::SystemReminder { id, reminder }
    }

    #[test]
    fn canonical_rewind_preserves_interleaved_reminder_and_derives_history() {
        let user1 = BaseMessage::human("user1");
        let ai1 = BaseMessage::ai("ai1");
        let user2 = BaseMessage::human("user2");
        let reminder_id = peri_acp_types::messages::MessageId::new();
        let mut history = vec![user1.clone(), ai1.clone(), user2.clone()];
        let mut history_payloads = vec![
            PersistedPayload::Message(user1.clone()),
            reminder(reminder_id),
            PersistedPayload::Message(ai1.clone()),
            PersistedPayload::Message(user2.clone()),
        ];

        apply_canonical_rewind(&mut history_payloads, &mut history, user2.id()).unwrap();

        assert_eq!(
            history_payloads
                .iter()
                .map(PersistedPayload::id)
                .collect::<Vec<_>>(),
            vec![user1.id(), reminder_id, ai1.id()]
        );
        assert_eq!(
            history.iter().map(BaseMessage::id).collect::<Vec<_>>(),
            vec![user1.id(), ai1.id()]
        );
    }

    #[test]
    fn canonical_rewind_with_only_reminder_interleave_keeps_projection_empty() {
        let target = BaseMessage::human("target");
        let reminder_id = peri_acp_types::messages::MessageId::new();
        let mut history = vec![target.clone()];
        let mut history_payloads = vec![
            reminder(reminder_id),
            PersistedPayload::Message(target.clone()),
        ];

        apply_canonical_rewind(&mut history_payloads, &mut history, target.id()).unwrap();

        assert_eq!(
            history_payloads
                .iter()
                .map(PersistedPayload::id)
                .collect::<Vec<_>>(),
            vec![reminder_id]
        );
        assert!(history.is_empty());
    }
}
