//! 提交管线测试（`submit.rs`）：`submit_text` / `dispatch_submit_request`。
//!
//! 与 input_area 的其余测试拆开：本模块覆盖「提交出口」——slash 路由、history、
//! 旧服务端排队缓冲与上传式附件的随行/不丢弃语义。

use super::super::reset_submit_side_effect_state;
use super::*;
use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{PENDING_ATTACHMENTS, VIEW_MODELS};
use serial_test::serial;

fn make_submit_recorder() -> std::sync::Arc<parking_lot::Mutex<Vec<SubmitRequest>>> {
    std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()))
}

fn recorded_submit(
    recorder: &std::sync::Arc<parking_lot::Mutex<Vec<SubmitRequest>>>,
) -> Option<SubmitRequest> {
    recorder.lock().pop()
}

#[test]
#[serial]
fn test_submit_text_model_opens_panel_without_history_or_bubble() {
    reset_submit_side_effect_state();
    submit_text("/model".to_string());
    assert_eq!(
        *crate::kit::atoms::ACTIVE_PANEL.state().read(),
        Some(PanelKind::Model)
    );
    assert!(crate::kit::atoms::INPUT_HISTORY.state().read().is_empty());
    assert!(VIEW_MODELS.state().read().items.is_empty());
}

#[test]
#[serial]
fn test_submit_text_clear_sends_session_control_without_history_or_bubble() {
    reset_submit_side_effect_state();
    let recorder = make_submit_recorder();
    dispatch_submit_request(parse_submit_request("/clear").unwrap(), false, |request| {
        recorder.lock().push(request)
    });
    assert!(crate::kit::atoms::INPUT_HISTORY.state().read().is_empty());
    assert!(VIEW_MODELS.state().read().items.is_empty());
    assert_eq!(
        recorded_submit(&recorder),
        Some(SubmitRequest::SessionControl(
            crate::kit::submit_request::SessionControlRequest::Clear,
        ))
    );
}

#[test]
#[serial]
fn test_submit_text_provider_sends_view_action_without_history_or_bubble() {
    reset_submit_side_effect_state();
    let recorder = make_submit_recorder();
    dispatch_submit_request(
        parse_submit_request("/provider").unwrap(),
        false,
        |request| recorder.lock().push(request),
    );
    assert!(crate::kit::atoms::INPUT_HISTORY.state().read().is_empty());
    assert!(VIEW_MODELS.state().read().items.is_empty());
    assert_eq!(
        recorded_submit(&recorder),
        Some(SubmitRequest::ViewAction(
            crate::kit::submit_request::ViewActionRequest::CycleProvider,
        ))
    );
}

#[test]
#[serial]
fn test_submit_text_compact_appends_bubble_and_history_and_sends_agent_text() {
    reset_submit_side_effect_state();
    let recorder = make_submit_recorder();
    dispatch_submit_request(
        parse_submit_request("/compact").unwrap(),
        false,
        |request| recorder.lock().push(request),
    );
    assert_eq!(crate::kit::atoms::INPUT_HISTORY.state().read().len(), 1);
    // UserBubble 通过 LOCAL_EVENT_TX 异步发送，不在此断言
    assert_eq!(
        recorded_submit(&recorder),
        Some(SubmitRequest::AgentText {
            text: "/compact".to_string(),
            attachments: Vec::new(),
        })
    );
}

#[test]
#[serial]
fn test_submit_text_unknown_slash_appends_bubble_and_history_and_sends_agent_text() {
    reset_submit_side_effect_state();
    let recorder = make_submit_recorder();
    dispatch_submit_request(parse_submit_request("/foo").unwrap(), false, |request| {
        recorder.lock().push(request)
    });
    assert_eq!(crate::kit::atoms::INPUT_HISTORY.state().read().len(), 1);
    assert_eq!(
        recorded_submit(&recorder),
        Some(SubmitRequest::AgentText {
            text: "/foo".to_string(),
            attachments: Vec::new(),
        })
    );
}

#[test]
#[serial]
fn test_submit_text_loading_unknown_slash_buffers_agent_text() {
    reset_submit_side_effect_state();
    ACP_STATE.state().write().is_loading = true;
    submit_text("/foo".to_string());
    assert_eq!(crate::kit::atoms::INPUT_HISTORY.state().read().len(), 1);
    // Slice 3 D4（§10 queued 反转）：loading 提交只入队，**不**发本地气泡——
    // transcript 不得提前出现 user bubble（drain 后才恰一次）。
    assert_eq!(INPUT_BUFFER.state().read().len(), 1);
    assert!(
        VIEW_MODELS.state().read().items.is_empty(),
        "loading 提交不提前进 transcript（排队项显示在 composer 上方队列）"
    );
}

/// Slice 3 D4：排队上限 32 条——超出时队首被挤出（VecDeque FIFO 上限）。
#[test]
#[serial]
fn test_submit_text_loading_queue_caps_at_32() {
    reset_submit_side_effect_state();
    ACP_STATE.state().write().is_loading = true;
    for i in 0..33 {
        submit_text(format!("/c{i}"));
    }
    let state = INPUT_BUFFER.state();
    let buf = state.read();
    assert_eq!(buf.len(), 32, "排队上限 32 条");
    assert_eq!(
        buf.front().unwrap().text,
        "/c1",
        "超出上限时队首（最旧）被挤出"
    );
    assert_eq!(buf.back().unwrap().text, "/c32");
}

/// Slice 3 D4：非 loading 提交路径不变——本地气泡 + AgentText 双发。
#[test]
#[serial]
fn test_submit_text_not_loading_sends_bubble_and_agent_text() {
    reset_submit_side_effect_state();
    let recorder = make_submit_recorder();
    dispatch_submit_request(parse_submit_request("/foo").unwrap(), false, |request| {
        recorder.lock().push(request)
    });
    assert!(INPUT_BUFFER.state().read().is_empty(), "非 loading 不入队");
    assert_eq!(
        recorded_submit(&recorder),
        Some(SubmitRequest::AgentText {
            text: "/foo".to_string(),
            attachments: Vec::new(),
        })
    );
    // 本地气泡经 LOCAL_EVENT_TX 异步发送（send_local_user_bubble），
    // transcript 由 acp_bridge 异步写入——本测试不直接断言 VIEW_MODELS
    // （OnceLock 通道不可重置），由 acp_events_test 的 drain 测试覆盖。
}

// ── 上传式附件：提交链上任何一环都不得丢弃 ──────────────────────────────────

fn pending_png() -> crate::kit::atoms::PendingAttachment {
    crate::kit::atoms::PendingAttachment::image("image/png", "AQID")
}

/// loading 排队必须连附件一起排队——排队项只存文本会在 drain 时静默丢图。
#[test]
#[serial]
fn test_submit_text_loading_buffers_attachments_with_text() {
    reset_submit_side_effect_state();
    ACP_STATE.state().write().is_loading = true;
    PENDING_ATTACHMENTS.state().write().push(pending_png());

    submit_text("/foo".to_string());

    let state = INPUT_BUFFER.state();
    let buf = state.read();
    assert_eq!(buf.len(), 1);
    assert_eq!(buf.front().unwrap().text, "/foo");
    assert_eq!(
        buf.front().unwrap().attachments.len(),
        1,
        "附件随排队项保留"
    );
    assert_eq!(buf.front().unwrap().attachments[0].base64_data, "AQID");
    drop(buf);
    assert!(
        PENDING_ATTACHMENTS.state().read().is_empty(),
        "附件已被排队项接管，不得留在 composer 造成重复提交"
    );
}

/// 非 loading 提交：附件随 `AgentText` 一起下发（上传式，不再经 `@image` 文本）。
#[test]
#[serial]
fn test_submit_text_agent_text_carries_pending_attachments() {
    reset_submit_side_effect_state();
    PENDING_ATTACHMENTS.state().write().push(pending_png());

    let recorder = make_submit_recorder();
    let request = {
        let attachments = std::mem::take(&mut *PENDING_ATTACHMENTS.state().write());
        SubmitRequest::AgentText {
            text: "/foo".to_string(),
            attachments,
        }
    };
    dispatch_submit_request(request, false, |request| recorder.lock().push(request));

    match recorded_submit(&recorder) {
        Some(SubmitRequest::AgentText { attachments, .. }) => {
            assert_eq!(attachments.len(), 1, "附件必须随下发请求保留");
            assert_eq!(attachments[0].base64_data, "AQID");
        }
        other => panic!("应下发带附件的 AgentText, got {other:?}"),
    }
}

/// 控制类请求不得消费待发送附件（`/clear` 等不是文本提交）。
#[test]
#[serial]
fn test_submit_text_control_request_keeps_pending_attachments() {
    reset_submit_side_effect_state();
    PENDING_ATTACHMENTS.state().write().push(pending_png());

    submit_text("/clear".to_string());

    assert_eq!(
        PENDING_ATTACHMENTS.state().read().len(),
        1,
        "非 AgentText 提交不得消费附件"
    );
}

/// 用户输入队列可用时，图片-only（空文本 + 附件）提交必须真的入队，
/// 而不是被 parse 的空文本分支丢弃。
#[test]
#[serial]
fn test_submit_text_image_only_enqueues_when_steer_enabled() {
    reset_submit_side_effect_state();
    crate::kit::steer_state::STEERS.state().write().enabled = true;
    PENDING_ATTACHMENTS.state().write().push(pending_png());

    submit_text(String::new());

    assert!(
        PENDING_ATTACHMENTS.state().read().is_empty(),
        "图片-only 提交必须被接管（不得滞留在 composer）"
    );

    // STEER_TX 未初始化 ⇒ 入队命令被拒后进入可恢复待办：附件随原稿可恢复，
    // 不是静默丢失（take-back 经 attachments_from_content 还原附件）。
    let session_id = crate::kit::atoms::ACTIVE_SESSION_ID.state().read().clone();
    let pending = crate::kit::steer_state::STEERS
        .state()
        .read()
        .pending_recovery_ids(&session_id);
    assert_eq!(pending.len(), 1, "失败的入队必须留下可恢复的待办");

    crate::kit::steer_state::STEERS.state().write().enabled = false;
}

#[test]
#[serial]
fn test_submit_text_loading_clear_shows_notification_without_history_or_buffer() {
    reset_submit_side_effect_state();
    ACP_STATE.state().write().is_loading = true;
    submit_text("/clear".to_string());
    assert!(crate::kit::atoms::INPUT_HISTORY.state().read().is_empty());
    assert!(VIEW_MODELS.state().read().items.is_empty());
    assert!(INPUT_BUFFER.state().read().is_empty());
    assert!(crate::kit::atoms::NOTIFICATION.state().read().is_some());
}
