use ratatui_kit::prelude::State;

use crate::i18n;
use crate::kit::acp_types::AcpEventWithEpoch;
use crate::kit::atoms::{ACP_STATE, INPUT_BUFFER, SUBMIT_TX, WIZARD_ACTIVE, local_event_tx};
use crate::kit::submit_request::{SessionControlRequest, SubmitRequest};
use peri_tui_components::textarea::TextAreaState;

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
            plugin_selections: Vec::new(),
        })
    } else {
        crate::kit::submit_request::parse_submit_request(&submitted).map(|request| match request {
            SubmitRequest::AgentText { text, .. } => SubmitRequest::AgentText {
                text,
                attachments: take_pending_attachments(),
                plugin_selections: Vec::new(),
            },
            other => other,
        })
    };
    let Some(request) = request else {
        return;
    };
    // AgentText 提交把待提交插件选择一并冻结；控制类请求保留选择给下一次任务。
    let request = match request {
        SubmitRequest::AgentText {
            text, attachments, ..
        } => SubmitRequest::AgentText {
            text,
            attachments,
            plugin_selections: std::mem::take(
                &mut *crate::kit::atoms::PENDING_PLUGIN_SELECTIONS.state().write(),
            ),
        },
        other => other,
    };

    if crate::kit::steer_state::is_enabled()
        && !is_remote_command(&submitted)
        && let SubmitRequest::AgentText {
            text,
            attachments,
            plugin_selections,
        } = request
    {
        crate::kit::input_history::push_history(&submitted);
        crate::kit::input_history::reset_history_cursor();
        if let Err(error) = crate::kit::steer_state::enqueue(text, attachments, plugin_selections) {
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
        SubmitRequest::OpenPanel(kind) => crate::kit::panel_registry::open_panel(kind),
        SubmitRequest::SessionControl(SessionControlRequest::ToggleSetup) => {
            *WIZARD_ACTIVE.state().write() = true;
        }
        SubmitRequest::AgentText {
            text,
            attachments,
            plugin_selections,
        } => {
            if crate::kit::steer_state::is_enabled() && !is_remote_command(&text) {
                let _ = crate::kit::steer_state::enqueue(text, attachments, plugin_selections);
                return;
            }
            crate::kit::input_history::push_history(&text);
            crate::kit::input_history::reset_history_cursor();
            if is_loading {
                // 旧服务端兼容队列：运行期间延后提交，出队时再加入聊天记录。
                // 支持用户输入队列的服务端在上方分支处理普通输入。
                // 附件与插件选择随排队项一起存——只存文本会在 drain 时静默丢弃
                // 图片或插件意图。
                let input_buffer = INPUT_BUFFER.state();
                let mut guard = input_buffer.write();
                guard.push_back(
                    crate::kit::atoms::BufferedInput::with_attachments(text, attachments)
                        .with_plugin_selections(plugin_selections),
                );
                while guard.len() > 32 {
                    guard.pop_front();
                }
            } else {
                // 通过 LOCAL_EVENT_TX 发送 LocalUserBubble 事件到 acp_bridge，
                // 统一走 dispatch_and_notify → push_view_models 写入路径。
                // 气泡带附件展示记录——发送后消息区仍能看到「这条消息带了图片」。
                send_local_user_bubble(&text, &attachments);
                send_request(SubmitRequest::AgentText {
                    text,
                    attachments,
                    plugin_selections,
                });
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
    if let Some(blocked) = commit_block_reason(&s.text) {
        handle_commit_block(blocked, &s.text);
        return;
    }
    let submitted = s.take_text();
    drop(s);

    submit_text(submitted);
    super::popup::reset_mention_popup();
    super::popup::reset_slash_popup();
    *crate::kit::atoms::PREDICTION.state().write() = crate::kit::atoms::PredictionState::default();
}

/// `commit_input` 的提交前阻断分支；返回 `Some` 时草稿与选择必须原样保留。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommitBlock {
    RemoteCommand,
    PluginSelectionUnavailable,
}

/// 本次 Enter 是否作为 AgentText 下发（含图片-only 提交）。
///
/// 能力闸门只约束会携带选择发送的请求；本地命令（如 `/setup`）不经过
/// 入队协议，不能被升级后残留的选择阻断。
fn submits_agent_text(text: &str, has_attachments: bool) -> bool {
    match crate::kit::submit_request::parse_submit_request(text) {
        Some(SubmitRequest::AgentText { .. }) => true,
        Some(_) => false,
        None => crate::kit::steer_state::is_enabled() && has_attachments,
    }
}

/// 旧 Host 的 EnqueueUserInputRequest 拒绝 pluginSelections：能力未协商时
/// 必须在取走草稿前阻断，不能静默丢弃标签后照常提交。
pub(super) fn commit_block_reason(text: &str) -> Option<CommitBlock> {
    if crate::kit::steer_state::is_enabled()
        && crate::kit::atoms::ACP_STATE.state().read().is_loading
        && is_remote_command(text)
    {
        return Some(CommitBlock::RemoteCommand);
    }
    if submits_agent_text(
        text,
        !crate::kit::atoms::PENDING_ATTACHMENTS
            .state()
            .read()
            .is_empty(),
    ) && !crate::kit::atoms::PENDING_PLUGIN_SELECTIONS
        .state()
        .read()
        .is_empty()
        && !crate::kit::steer_state::supports_plugin_selections()
    {
        return Some(CommitBlock::PluginSelectionUnavailable);
    }
    None
}

pub(super) fn handle_commit_block(blocked: CommitBlock, text: &str) {
    match blocked {
        CommitBlock::RemoteCommand => {
            show_submit_blocked_notification(
                &crate::kit::submit_request::SubmitRequest::AgentText {
                    text: text.to_owned(),
                    attachments: Vec::new(),
                    plugin_selections: Vec::new(),
                },
            );
        }
        CommitBlock::PluginSelectionUnavailable => notify_plugin_selection_unavailable(),
    }
}

fn notify_plugin_selection_unavailable() {
    *crate::kit::atoms::NOTIFICATION.state().write() = Some(crate::kit::atoms::Notification {
        message: i18n::tr("submit-plugin-selection-unavailable"),
        until: peri_time::monotonic_now() + std::time::Duration::from_secs(3),
    });
    crate::kit::atoms::RENDER_HEARTBEAT
        .set(crate::kit::atoms::RENDER_HEARTBEAT.get().wrapping_add(1));
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

#[must_use]
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
/// `attachments` 只用于消息区的附件记录（图片序号 + 大小），不改变上行内容——
/// 上行载荷仍由 [`SubmitRequest::AgentText`] 携带。
///
/// pub(crate)：非 loading 提交路径与 `acp_events::render::drain_input_buffer`
/// （Slice 3 D4）共用——drain 排队项时镜像非 loading 路径，先本地气泡再提交。
pub(crate) fn send_local_user_bubble(
    text: &str,
    attachments: &[crate::kit::atoms::PendingAttachment],
) {
    use crate::kit::acp_types::AcpEventData;
    if let Some(tx) = local_event_tx() {
        let _ = tx.send(AcpEventWithEpoch {
            child_origin: None,
            view_epoch: crate::kit::atoms::BRIDGE_RESET_COUNTER.get(),
            event: AcpEventData::LocalUserBubble {
                text: text.to_string(),
                attachments: crate::kit::steer_state::attachment_displays(attachments),
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
        crate::kit::input_history::reset_history_cursor();
    }
}

#[cfg(test)]
#[path = "submit_test.rs"]
mod tests;
