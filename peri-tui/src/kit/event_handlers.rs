//! 事件处理器——Global + Root 层事件监听注册。
//!
//! 替代 `event/keyboard/normal_keys.rs` 的键盘 fallback。
//! 分为 Global Layer（不可阻断）和 Root Layer（被子层阻断）。
//!
//! ## 保留快捷键
//!
//! | 快捷键 | 功能 |
//! |--------|------|
//! | Ctrl+C | 三级优先级链（中断→双击退出） |
//! | Shift+Tab（BackTab） | 权限模式循环 |
//! | Esc    | 关闭 popup / 面板 / mention / slash |

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
};

use peri_acp_types::permission::PermissionMode;

use super::atoms::{
    ACP_STATE, CANCEL_TX, INPUT_AREA_ESC_PREFIX, LAST_CTRL_C_PROCESSED, LAST_ESC_TIME,
    MODE_HIGHLIGHT_UNTIL, MODEL_HIGHLIGHT_UNTIL, NOTIFICATION, PERMISSION_MODE_HANDLE,
    PROVIDER_HIGHLIGHT_UNTIL, QUIT_PENDING_SINCE, SERVICE_SNAPSHOT,
};
use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::ask_user_action::AskUserResponseAction;
use crate::kit::atoms::{
    ACTIVE_PANEL, ASK_USER_PENDING, ASK_USER_RESPONSE_TX, Notification, POPUP_KIND, PopupKind,
};
use crate::kit::focus_router::{
    FocusLayer, GlobalShortcut, active_layer, classify_global_shortcut,
};
use crate::kit::panel_registry::close_active_panel;
use crate::kit::popup_overlay::{close_popup, open_popup};
use tracing::info;

/// Ctrl+C 的行为由状态纯函数决定，确保可测试。
#[derive(Debug, PartialEq, Eq)]
enum CtrlCAction {
    /// 发送取消请求给 agent（loading 状态）
    Cancel,
    /// 启动或重置退出倒计时
    FirstQuit,
    /// 1 秒内双击——立即退出
    Quit,
}

/// 根据 loading 状态和退出计时器决定 Ctrl+C 的行为。
///
/// 纯函数——不依赖全局状态，输入决定输出。
fn determine_ctrl_c_action(
    loading: bool,
    quit_pending: Option<std::time::Instant>,
    now: std::time::Instant,
) -> CtrlCAction {
    if loading {
        CtrlCAction::Cancel
    } else {
        match quit_pending {
            None => CtrlCAction::FirstQuit,
            Some(t) if now.duration_since(t) < std::time::Duration::from_secs(1) => {
                CtrlCAction::Quit
            }
            Some(_) => CtrlCAction::FirstQuit,
        }
    }
}

/// Global Layer: 不可阻断的快捷键。
///
/// 注册监听 Ctrl+C 等顶级快捷键。
pub fn register_global_handlers(hooks: &mut Hooks, mut exit: Handler<'static, ()>) {
    hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
        tracing::info!(?event, "kit raw input event");
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }

        // popup 模态优先：popup 激活时全局快捷键让路——popup 内的按键
        // （如 OAuth popup 的 Ctrl+O 打开浏览器 / Ctrl+C 复制链接）由 popup
        // 自己的 handler 处理；否则 Ctrl+O 被全局 ToggleDiff 抢占、Ctrl+C
        // 直接触发退出。
        if POPUP_KIND.state().read().is_some() {
            return EventResult::Ignored;
        }

        match classify_global_shortcut(&key) {
            Some(GlobalShortcut::Quit) => {
                // 防重入：同一 Ctrl+C 事件在 200ms 内只能处理一次。
                // ratatui-kit 在事件处理中写 atom 后可能触发重渲染并二次分发同一事件，
                // 导致 FirstQuit 写入 QUIT_PENDING_SINCE 后第二次进入立即命中 Quit 分支。
                // 200ms 远小于人类双击间隔（~500ms），仅屏蔽框架级重放。
                let now = std::time::Instant::now();
                let last_processed = *LAST_CTRL_C_PROCESSED.state().read();
                const REENTRY_GUARD_MS: u64 = 200;
                if let Some(last) = last_processed
                    && now.duration_since(last) < std::time::Duration::from_millis(REENTRY_GUARD_MS)
                {
                    tracing::warn!(
                        elapsed_ms = now.duration_since(last).as_millis(),
                        "Ctrl+C reentrant guard: skipping duplicate dispatch"
                    );
                    return EventResult::Consumed;
                }
                *LAST_CTRL_C_PROCESSED.state().write() = Some(now);

                let loading = ACP_STATE.state().read().is_loading;
                let pending = *QUIT_PENDING_SINCE.state().read();

                match determine_ctrl_c_action(loading, pending, now) {
                    CtrlCAction::Cancel => {
                        *QUIT_PENDING_SINCE.state().write() = None;
                        if let Some(tx) = CANCEL_TX.get() {
                            let _ = tx.send(());
                            show_cancel_notification(now);
                        }
                    }
                    CtrlCAction::FirstQuit => {
                        *QUIT_PENDING_SINCE.state().write() = Some(now);
                        // 提示由 StatusBarRow2 订阅 QUIT_PENDING_SINCE 直接渲染，不走 NOTIFICATION
                        info!("再次按 Ctrl+C 退出");
                        info!("再次按 Ctrl+C 退出");
                    }
                    CtrlCAction::Quit => {
                        *QUIT_PENDING_SINCE.state().write() = None;
                        exit(());
                    }
                }
                EventResult::Consumed
            }
            Some(GlobalShortcut::CycleModel) => {
                *MODEL_HIGHLIGHT_UNTIL.state().write() =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(2));
                EventResult::Consumed
            }
            Some(GlobalShortcut::CycleProvider) => {
                *PROVIDER_HIGHLIGHT_UNTIL.state().write() =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(2));
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    });
}

/// Root Layer: 可被子层阻断的快捷键。
///
/// 注册：
/// - Esc → 关闭 popup / @mention / slash_hint / 当前激活面板
/// - Shift+Tab(BackTab)  → cycle permission mode
pub fn register_root_handlers(hooks: &mut Hooks) {
    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }

        match classify_global_shortcut(&key) {
            Some(GlobalShortcut::CyclePermissionMode) => {
                *MODE_HIGHLIGHT_UNTIL.state().write() =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(2));
                // 执行权限模式循环，并即时推送 SERVICE_SNAPSHOT 避免等待 2s 后台轮询
                if let Some(mode_handle) = PERMISSION_MODE_HANDLE.get() {
                    let new_mode = mode_handle.cycle();
                    let label = match new_mode {
                        PermissionMode::Default => "default",
                        PermissionMode::AcceptEdit => "accept-edit",
                        PermissionMode::AutoMode => "auto-mode",
                        PermissionMode::Bypass => "bypass",
                    };
                    let handle = SERVICE_SNAPSHOT.state();
                    let mut snap = handle.read().clone();
                    snap.permission_mode = label.to_string();
                    *handle.write() = snap;
                }
                EventResult::Consumed
            }
            _ => match key.code {
                // Esc: 双击触发 Rewind popup，否则走关闭优先级链
                KeyCode::Esc => {
                    // 跳过由 InputArea 检测到的 Alt+key ESC 前缀
                    let is_alt_prefix = *INPUT_AREA_ESC_PREFIX.state().read();
                    if is_alt_prefix {
                        return EventResult::Ignored;
                    }

                    match active_layer() {
                        FocusLayer::Popup(_) => {
                            close_popup();
                            return EventResult::Consumed;
                        }
                        FocusLayer::InlineCompletion => return EventResult::Ignored,
                        FocusLayer::Panel => {
                            // 防御性 guard：如果活跃面板是 AskUser，发送 Cancel 响应
                            // 防止因优先级回归导致 agent 永久挂起。正常情况下此分支不会执行
                            // （AskUserPanel handler 使用 High 优先级，会先消费 ESC）。
                            if *ACTIVE_PANEL.state().read() == Some(PanelKind::AskUser)
                                && let Some(snapshot) = ASK_USER_PENDING.state().read().clone()
                                && let Some(tx) = ASK_USER_RESPONSE_TX.get()
                            {
                                let _ = tx.send(AskUserResponseAction::Cancel {
                                    request_id_str: snapshot.request_id_json.clone(),
                                });
                                crate::kit::panel_registry::close_ask_user_panel_for_request(
                                    &snapshot.request_id_json,
                                );
                            } else {
                                close_active_panel();
                            }
                            return EventResult::Consumed;
                        }
                        FocusLayer::Input | FocusLayer::Message => {}
                    }

                    let now = std::time::Instant::now();
                    let last_esc = *LAST_ESC_TIME.state().read();
                    let is_double_esc = last_esc
                        .map(|t| now.duration_since(t) < std::time::Duration::from_millis(500))
                        .unwrap_or(false);

                    *LAST_ESC_TIME.state().write() = Some(now);

                    if is_double_esc {
                        // Rewind v2：打开面板 + 实时查询候选（查询响应写 REWIND_PREVIEW atom，
                        // 弹窗订阅渲染；查询失败写 REWIND_QUERY_ERROR）。
                        crate::kit::rewind_candidates::spawn_candidates_query();
                        open_popup(PopupKind::Rewind);
                        return EventResult::Consumed;
                    }

                    EventResult::Ignored
                }
                _ => EventResult::Ignored,
            },
        }
    });
}

fn show_cancel_notification(now: std::time::Instant) {
    *NOTIFICATION.state().write() = Some(Notification {
        message: i18n::tr("cancel-request-sent"),
        until: now + std::time::Duration::from_secs(2),
    });
}

#[cfg(test)]
#[path = "event_handlers_test.rs"]
mod tests;
