//! Popup overlay——根据 `POPUP_KIND` atom 渲染当前激活的交互弹窗。
//!
//! 这是 kit 路径"弹窗系统"的渲染入口——与 PanelOverlay 平级但优先级更高
//! （Esc 链：popup → @mention/slash → panel）。订阅 `POPUP_KIND`：
//!
//! - `None`：渲染空 View，不消耗布局
//! - `Some(kind)`：渲染对应 `#[component]` 弹窗
//!
//! ## 触发源
//!
//! 4 种弹窗都由 `kit/acp_events.rs::dispatch_and_notify` 在收到对应 AcpEvent
//! 时写入 `POPUP_KIND`：
//! - `HitlPending` → `PopupKind::Hitl`
//! - `AskUser`     → `PopupKind::AskUser`
//! - `RewindPreview` → `PopupKind::Rewind`
//! - `OauthNeeded` → `PopupKind::OAuth`
//!
//! ## Esc 关闭
//!
//! 全局 Esc 由 `event_handlers::register_root_handlers` 处理——优先级最高，
//! 即使面板或 @mention 也开着，先关弹窗。

use crate::kit::atoms::{self, DownloadProgressPayload, PopupKind};
use crate::kit::popups::{
    confirm_popup::ConfirmPopup, download_progress::DownloadProgressPopup, hitl_popup::HitlPopup,
    model_quick_switch::ModelQuickSwitchPopup, oauth_popup::OAuthPopup, rewind_popup::RewindPopup,
};
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    prelude::*,
    ratatui::layout::{Constraint, Rect},
};

/// 弹窗覆盖层组件。
///
/// 订阅 `POPUP_KIND` atom，渲染当前激活弹窗。无弹窗时返回空 View。
#[component]
pub fn PopupOverlay(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let popup_store = hooks.use_atom(&atoms::POPUP_KIND);
    let kind = *popup_store.read();
    let (term_w, term_h) = hooks.use_terminal_size();

    match kind {
        Some(PopupKind::Hitl) => render_popup(element!(HitlPopup()).into(), term_w, term_h),
        Some(PopupKind::AskUser) => render_empty(), // AskUser 已迁移为 Panel
        Some(PopupKind::Rewind) => render_popup(element!(RewindPopup()).into(), term_w, term_h),
        Some(PopupKind::OAuth) => render_popup(element!(OAuthPopup()).into(), term_w, term_h),
        Some(PopupKind::Confirm) => render_popup(element!(ConfirmPopup()).into(), term_w, term_h),
        Some(PopupKind::Download) => {
            render_popup(element!(DownloadProgressPopup()).into(), term_w, term_h)
        }
        // 小弹出层：组件内部按 MODEL_SWITCH_ANCHOR 自定位（锚定在状态栏模型段上方），
        // 不走居中 render_popup。
        Some(PopupKind::ModelQuickSwitch) => {
            // 自定位小层不登记矩形——POPUP_AREA=None，滚轮保持保守遮挡
            *atoms::POPUP_AREA.state().write_no_update() = None;
            element!(ModelQuickSwitchPopup()).into()
        }
        None => render_empty(),
    }
}

/// 包裹弹窗——只定位和清除弹窗矩形，避免 Modal 整屏背景绘制导致白屏。
fn render_popup(p: AnyElement<'static>, term_w: u16, term_h: u16) -> AnyElement<'static> {
    let state = THEME_ATOM.state();
    let guard = state.read();
    let popup = &guard.component.popup;
    let width = term_w.saturating_sub(4).min(popup.modal_max_width).max(1);
    let height = term_h.saturating_sub(4).min(popup.modal_max_height).max(1);
    let x = term_w.saturating_sub(width) / 2;
    let y = term_h.saturating_sub(height) / 2;

    // 登记弹窗屏幕矩形——mouse_router::occludes_scroll 按坐标区分「弹窗内/外」，
    // 弹窗外滚轮放行给消息区（HITL 审批弹窗打开时可滚动 chat 查看上下文）。
    // 渲染路径写 atom 用 write_no_update（判定读取不依赖订阅唤醒），
    // 与 PANEL_SCROLL_OWNER 同模式，避免渲染中 wake 自激。
    *atoms::POPUP_AREA.state().write_no_update() = Some(Rect::new(x, y, width, height));

    element!(
        Positioned(x: x, y: y, width: width, height: height, clear: true) {
            Center(width: Constraint::Fill(1), height: Constraint::Fill(1)) {
                { p }
            }
        }
    )
    .into()
}

/// 空覆盖——无弹窗激活时返回零尺寸 Positioned，避免默认 View/Fragment 布局参与父级 flex。
fn render_empty() -> AnyElement<'static> {
    // 清除弹窗矩形登记，防止旧矩形残留错误放行滚轮
    *atoms::POPUP_AREA.state().write_no_update() = None;
    element!(Positioned(x: 0u16, y: 0u16, width: 0u16, height: 0u16, clear: false)).into()
}

// ── 弹窗操作辅助函数（mutates POPUP_KIND atom） ──────────────────────────

/// 打开弹窗（覆盖式）。已打开其他弹窗会被替换。
pub fn open_popup(kind: PopupKind) {
    *atoms::POPUP_KIND.state().write() = Some(kind);
}

/// 关闭当前弹窗（如果有）。返回被关闭的 PopupKind（用于日志/状态反馈）。
///
/// I21-C：同步清空对应 payload atom——避免下次打开 popup 仍显示陈旧数据。
/// 例如 HitlPopup 关闭后，HITL_PENDING 应为 None；下次 agent 触发新的
/// HitlPending 事件时 dispatch_and_notify 会重新写入。但若用户在两次事件
/// 之间手动 open_popup（如未来加快捷键），不会看到上次的工具调用信息。
pub fn close_popup() -> Option<PopupKind> {
    let prev = *atoms::POPUP_KIND.state().read();
    *atoms::POPUP_KIND.state().write() = None;
    // I21-C：根据关闭的 popup 类型清空对应 payload atom
    if let Some(kind) = prev {
        match kind {
            PopupKind::Hitl => {
                *atoms::HITL_PENDING.state().write() = None;
                *atoms::HITL_REQUEST_ID.state().write() = None;
            }
            PopupKind::AskUser => {
                *atoms::ASK_USER_PENDING.state().write() = None;
                *atoms::ASK_USER_REQUEST_ID.state().write() = None;
            }
            // Rewind：REWIND_PREVIEW 保留（候选跟随会话生命周期，关闭后可再开），
            // 但预算状态 / 目标文本 / 查询错误随弹窗关闭清空（下次打开重新查询）。
            // 会话边界（/clear、thread 切换）由 submit_consumer / thread_load_consumer
            // 额外清空候选。
            PopupKind::Rewind => {
                *atoms::REWIND_BUDGET_STATE.state().write() = atoms::RewindBudgetState::Idle;
                *atoms::REWIND_TARGET_TEXT.state().write() = None;
                *atoms::REWIND_QUERY_ERROR.state().write() = None;
            }
            PopupKind::OAuth => *atoms::OAUTH_INFO.state().write() = None,
            PopupKind::Confirm => *atoms::CONFIRM_PAYLOAD.state().write() = None,
            PopupKind::Download => {
                *atoms::DOWNLOAD_PROGRESS.state().write() = DownloadProgressPayload::default()
            }
            // ModelQuickSwitch 无 payload atom（数据即读自 PERI_CONFIG_HANDLE）
            PopupKind::ModelQuickSwitch => {}
        }
    }
    prev
}

/// 是否有弹窗激活。
pub fn is_popup_active() -> bool {
    atoms::POPUP_KIND.state().read().is_some()
}

#[cfg(test)]
#[path = "popup_overlay_test.rs"]
mod tests;
