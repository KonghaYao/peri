use ratatui_kit::prelude::State;

use crate::components::textarea::TextAreaState;
use crate::i18n;
use crate::kit::acp_types::AcpEventWithEpoch;
use crate::kit::atoms::{ACP_STATE, INPUT_BUFFER, LOCAL_EVENT_TX, SUBMIT_TX, WIZARD_ACTIVE};
use crate::kit::input_history::{push_history, reset_history_cursor};
use crate::kit::panel_registry::open_panel;
use crate::kit::submit_request::{SessionControlRequest, SubmitRequest, parse_submit_request};

/// 取出待发送附件。附件只在「确定要提交 agent 文本」时消费——控制类请求
/// （`/clear` 等）不碰 `PENDING_ATTACHMENTS`，图片保留在 composer 供下次提交。
fn take_pending_attachments() -> Vec<crate::kit::atoms::PendingAttachment> {
    std::mem::take(&mut *crate::kit::atoms::PENDING_ATTACHMENTS.state().write())
}

pub(super) fn submit_text(submitted: String) {
    // 图片-only 提交（空文本 + 有附件）：不走 slash 解析。仅在对端支持用户输入
    // 队列时可用；旧服务端下附件原样留在 composer（不静默丢弃，用户补文本即可）。
    let attachment_only = submitted.trim().is_empty()
        && crate::kit::steer_state::is_enabled()
        && !crate::kit::atoms::PENDING_ATTACHMENTS
            .state()
            .read()
            .is_empty();

    let request = if attachment_only {
        Some(SubmitRequest::AgentText {
            text: submitted.clone(),
            attachments: take_pending_attachments(),
        })
    } else {
        parse_submit_request(&submitted).map(|request| match request {
            SubmitRequest::AgentText { text, .. } => SubmitRequest::AgentText {
                text,
                attachments: take_pending_attachments(),
            },
            other => other,
        })
    };
    let Some(request) = request else {
        return;
    };

    if crate::kit::steer_state::is_enabled()
        && !is_remote_command(&submitted)
        && let SubmitRequest::AgentText { text, attachments } = request
    {
        push_history(&submitted);
        reset_history_cursor();
        if let Err(error) = crate::kit::steer_state::enqueue(text, attachments) {
            tracing::warn!(error = %error, "user input queue submission rejected locally");
        }
        return;
    }
    let is_loading = ACP_STATE.state().read().is_loading;
    dispatch_submit_request(request, is_loading, |request| {
        if let Some(tx) = SUBMIT_TX.get() {
            let _ = tx.send(request);
        }
    });
}

pub(super) fn dispatch_submit_request<F>(
    request: SubmitRequest,
    is_loading: bool,
    mut send_request: F,
) where
    F: FnMut(SubmitRequest),
{
    match request {
        SubmitRequest::OpenPanel(kind) => open_panel(kind),
        SubmitRequest::SessionControl(SessionControlRequest::ToggleSetup) => {
            *WIZARD_ACTIVE.state().write() = true;
        }
        SubmitRequest::AgentText { text, attachments } => {
            if crate::kit::steer_state::is_enabled() && !is_remote_command(&text) {
                let _ = crate::kit::steer_state::enqueue(text, attachments);
                return;
            }
            push_history(&text);
            reset_history_cursor();
            if is_loading {
                // 旧服务端兼容队列：运行期间延后提交，出队时再加入聊天记录。
                // 支持用户输入队列的服务端在上方分支处理普通输入。
                // 附件随排队项一起存——只存文本会在 drain 时静默丢弃图片。
                let input_buffer = INPUT_BUFFER.state();
                let mut guard = input_buffer.write();
                guard.push_back(crate::kit::atoms::BufferedInput::with_attachments(
                    text,
                    attachments,
                ));
                while guard.len() > 32 {
                    guard.pop_front();
                }
            } else {
                // 通过 LOCAL_EVENT_TX 发送 LocalUserBubble 事件到 acp_bridge，
                // 统一走 dispatch_and_notify → push_view_models 写入路径。
                send_local_user_bubble(&text);
                send_request(SubmitRequest::AgentText { text, attachments });
            }
        }
        request @ (SubmitRequest::SessionControl(_)
        | SubmitRequest::ViewAction(_)
        | SubmitRequest::KeepGoing) => {
            if is_loading {
                show_submit_blocked_notification(&request);
            } else {
                send_request(request);
            }
        }
    }
}

/// Enter 提交落点（输入区 Enter 分支与 SlashCompletion「无候选 Confirm」
/// 共用同一实现）：steer 阻断提示 → 取出文本 → [`submit_text`] 统一落点 →
/// 清理弹窗与预测态。
///
/// [Why 单一实现] 空候选弹窗按 Enter 走提交分支后，必须与输入区 Enter 完全
/// 同路，否则两条提交路径会各自漂移（附件消费、steer 阻断、历史记录任一处
/// 不一致都会变成静默行为差异）。
pub(super) fn commit_input(state: State<TextAreaState>) {
    super::exit_entry_focus_on_edit();
    let mut s = state.write();
    if crate::kit::steer_state::is_enabled()
        && crate::kit::atoms::ACP_STATE.state().read().is_loading
        && is_remote_command(&s.text)
    {
        show_submit_blocked_notification(&crate::kit::submit_request::SubmitRequest::AgentText {
            text: s.text.clone(),
            attachments: Vec::new(),
        });
        return;
    }
    let submitted = s.take_text();
    drop(s);

    submit_text(submitted);
    super::reset_mention_popup();
    super::reset_slash_popup();
    *crate::kit::atoms::PREDICTION.state().write() = crate::kit::atoms::PredictionState::default();
}

pub(super) fn show_submit_blocked_notification(request: &SubmitRequest) {
    let message = match request {
        SubmitRequest::SessionControl(_)
        | SubmitRequest::ViewAction(_)
        | SubmitRequest::AgentText { .. } => i18n::tr("submit-blocked"),
        _ => return,
    };
    *crate::kit::atoms::NOTIFICATION.state().write() = Some(crate::kit::atoms::Notification {
        message,
        until: peri_time::monotonic_now() + std::time::Duration::from_secs(3),
    });
    crate::kit::atoms::RENDER_HEARTBEAT
        .set(crate::kit::atoms::RENDER_HEARTBEAT.get().wrapping_add(1));
}

pub(crate) fn is_remote_command(text: &str) -> bool {
    let Some(name) = text
        .split_whitespace()
        .next()
        .and_then(|token| token.strip_prefix('/'))
    else {
        return false;
    };
    crate::kit::atoms::AVAILABLE_SLASH_COMMANDS
        .state()
        .read()
        .iter()
        .any(|entry| {
            name == entry.fullname
                || entry.aliases.iter().any(|alias| name == alias)
                || (entry.level == 1 && name.strip_prefix("core:") == Some(entry.fullname.as_str()))
        })
}

/// 发送本地 user bubble 事件（`LocalUserBubble`）到 acp_bridge。
///
/// pub(crate)：非 loading 提交路径与 `acp_events::render::drain_input_buffer`
/// （Slice 3 D4）共用——drain 排队项时镜像非 loading 路径，先本地气泡再提交。
pub(crate) fn send_local_user_bubble(text: &str) {
    use crate::kit::acp_types::AcpEventData;
    if let Some(tx) = LOCAL_EVENT_TX.get() {
        let _ = tx.send(AcpEventWithEpoch {
            event: AcpEventData::LocalUserBubble {
                text: text.to_string(),
            },
            active_session_id: String::new(),
        });
    }
}

/// 退出 history 浏览模式（如果当前正在浏览）。
///
/// 任何改变编辑文本的 handler 都应在写入前调用：保留当前编辑内容作为新草稿，
/// 但清掉 `INPUT_HISTORY_INDEX` 指针，避免下一次 history_up 复用陈旧的浏览位置。
/// 非历史模式下调用为 no-op。
pub(super) fn exit_history_mode_if_active() {
    use crate::kit::atoms::INPUT_HISTORY_INDEX;
    if INPUT_HISTORY_INDEX.state().read().is_some() {
        reset_history_cursor();
    }
}

#[cfg(test)]
#[path = "submit_test.rs"]
mod tests;
