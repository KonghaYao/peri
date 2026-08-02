//! Rewind 候选查询——双击 Esc 时向服务端实时查询 user 消息候选。
//!
//! 替代旧的"每轮 turn 结束推送 preview"模型：候选由打开面板时的
//! `session/rewind-candidates` RPC 一次性获取，写入 `REWIND_PREVIEW` atom，
//! 弹窗组件订阅渲染。

use fluent_bundle::FluentValue;
use serde_json::Value;

use crate::i18n;
use crate::kit::atoms::{ACP_CLIENT_HANDLE, RENDER_HEARTBEAT, REWIND_PREVIEW, REWIND_QUERY_ERROR};
use peri_acp_types::event_data::{RewindMessage, RewindPreview};

/// 单个回退候选。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewindCandidate {
    pub id: String,
    pub preview: String,
}

/// 解析 `session/rewind-candidates` 响应中的消息列表。
pub fn parse_candidates_response(resp: &Value) -> Result<Vec<RewindCandidate>, String> {
    let messages = resp
        .get("messages")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "rewind-candidates 响应缺少 messages 数组".to_string())?;

    messages
        .iter()
        .map(|m| {
            Ok(RewindCandidate {
                id: m
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "候选缺少 id".to_string())?
                    .to_string(),
                preview: m
                    .get("preview")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect()
}

/// 将候选写入 REWIND_PREVIEW atom（role 恒 "user"），触发弹窗重渲染。
pub fn apply_candidates(candidates: &[RewindCandidate]) {
    let preview = RewindPreview {
        files: vec![],
        messages: candidates
            .iter()
            .map(|c| RewindMessage {
                id: c.id.clone(),
                role: "user".to_string(),
                preview: c.preview.clone(),
            })
            .collect(),
    };
    *REWIND_PREVIEW.state().write() = Some(preview);
    RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
}

/// 异步发送候选查询（双击 Esc 时 spawn）。
///
/// 成功：`apply_candidates` + 清 REWIND_QUERY_ERROR；
/// 失败：写 REWIND_QUERY_ERROR 错误文案（弹窗显示）。
pub fn spawn_candidates_query() {
    let Some(client) = ACP_CLIENT_HANDLE.get().cloned() else {
        *REWIND_QUERY_ERROR.state().write() = Some(i18n::tr("rewind-error-no-client"));
        RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
        return;
    };
    let sid = crate::kit::atoms::ACTIVE_SESSION_ID.state().read().clone();
    if sid.is_empty() {
        *REWIND_QUERY_ERROR.state().write() = Some(i18n::tr("rewind-error-no-session"));
        RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
        return;
    }

    // P1 竞态防护：捕获代次，响应到达时比对——过期响应丢弃。
    let query_gen = crate::kit::atoms::REWIND_QUERY_GEN.get().wrapping_add(1);
    crate::kit::atoms::REWIND_QUERY_GEN.set(query_gen);

    tokio::spawn(async move {
        let resp = client
            .send_raw_request(
                "session/rewind-candidates",
                serde_json::json!({ "sessionId": sid }),
            )
            .await;
        match resp {
            Ok(value) => match parse_candidates_response(&value) {
                Ok(candidates) => {
                    if crate::kit::atoms::REWIND_QUERY_GEN.get() != query_gen {
                        return; // 已有新查询，丢弃过期响应
                    }
                    *REWIND_QUERY_ERROR.state().write() = None;
                    apply_candidates(&candidates);
                }
                Err(e) => {
                    if crate::kit::atoms::REWIND_QUERY_GEN.get() != query_gen {
                        return;
                    }
                    *REWIND_QUERY_ERROR.state().write() = Some(e);
                    RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
                }
            },
            Err(e) => {
                if crate::kit::atoms::REWIND_QUERY_GEN.get() != query_gen {
                    return;
                }
                *REWIND_QUERY_ERROR.state().write() = Some(i18n::tr_args(
                    "rewind-error-query-failed",
                    &[("error".into(), FluentValue::from(e.to_string()))],
                ));
                RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
            }
        }
    });
}

#[cfg(test)]
#[path = "rewind_candidates_test.rs"]
mod tests;
