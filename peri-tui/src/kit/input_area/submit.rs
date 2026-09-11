use crate::i18n;
use crate::kit::acp_types::AcpEventWithEpoch;
use crate::kit::atoms::{ACP_STATE, INPUT_BUFFER, LOCAL_EVENT_TX, SUBMIT_TX, WIZARD_ACTIVE};
use crate::kit::input_history::{push_history, reset_history_cursor};
use crate::kit::panel_registry::open_panel;
use crate::kit::submit_request::{SessionControlRequest, SubmitRequest, parse_submit_request};

pub(super) fn submit_text(submitted: String) {
    if crate::kit::steer_state::is_enabled()
        && submitted.trim().is_empty()
        && !crate::kit::atoms::PENDING_ATTACHMENTS
            .state()
            .read()
            .is_empty()
    {
        let attachments =
            std::mem::take(&mut *crate::kit::atoms::PENDING_ATTACHMENTS.state().write());
        let _ = crate::kit::steer_state::enqueue(submitted, attachments);
        return;
    }
    let Some(request) = parse_submit_request(&submitted) else {
        return;
    };

    if crate::kit::steer_state::is_enabled()
        && matches!(request, SubmitRequest::AgentText(_))
        && !is_remote_command(&submitted)
    {
        push_history(&submitted);
        reset_history_cursor();
        let attachments =
            std::mem::take(&mut *crate::kit::atoms::PENDING_ATTACHMENTS.state().write());
        if let Err(error) = crate::kit::steer_state::enqueue(submitted, attachments) {
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
        SubmitRequest::AgentText(text) => {
            if crate::kit::steer_state::is_enabled() && !is_remote_command(&text) {
                let _ = crate::kit::steer_state::enqueue(text, Vec::new());
                return;
            }
            push_history(&text);
            reset_history_cursor();
            if is_loading {
                // 旧服务端兼容队列：运行期间延后提交，出队时再加入聊天记录。
                // 支持用户输入队列的服务端在上方分支处理普通输入。
                let input_buffer = INPUT_BUFFER.state();
                let mut guard = input_buffer.write();
                guard.push_back(text);
                while guard.len() > 32 {
                    guard.pop_front();
                }
            } else {
                // 通过 LOCAL_EVENT_TX 发送 LocalUserBubble 事件到 acp_bridge，
                // 统一走 dispatch_and_notify → push_view_models 写入路径。
                send_local_user_bubble(&text);
                send_request(SubmitRequest::AgentText(text));
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

pub(super) fn show_submit_blocked_notification(request: &SubmitRequest) {
    let message = match request {
        SubmitRequest::SessionControl(_)
        | SubmitRequest::ViewAction(_)
        | SubmitRequest::AgentText(_) => i18n::tr("submit-blocked"),
        _ => return,
    };
    *crate::kit::atoms::NOTIFICATION.state().write() = Some(crate::kit::atoms::Notification {
        message,
        until: std::time::Instant::now() + std::time::Duration::from_secs(3),
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
