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
    let event_sink: Arc<dyn crate::session::event_sink::EventSink> =
        Arc::new(crate::session::event_sink::TransportEventSink::new(
            transport.clone(), // transport: &Arc<dyn AcpTransport>（签名改动见下方实现注记）
            cfg.session_manager.caps_registry(),
        ));
    let peri_config_snapshot = Arc::new(cfg.peri_config.read().clone());
    dispatch::rewind_execute(
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
    .await
    .inspect(|resp| {
        // P1：回写截断后的 history——SessionState.history 是后续
        // session/rewind-candidates 与 session/rewind-preview 的数据源，
        // 必须与 RewindCompleted 事件中的结果一致。
        if let (Some(h), Some(s)) = (
            resp.get("history").and_then(|v| v.as_array()),
            sessions.get_mut(&session_id),
        ) {
            let h = h.clone();
            if let Ok(msgs) = serde_json::from_value::<Vec<peri_acp_types::messages::BaseMessage>>(
                serde_json::Value::Array(h),
            ) {
                s.history = msgs;
            }
        }
    })
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
