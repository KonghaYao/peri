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

use crate::kit::atoms::{self, PopupKind};
use crate::kit::popups::{
    ask_user_popup::AskUserPopup, hitl_popup::HitlPopup, oauth_popup::OAuthPopup,
    rewind_popup::RewindPopup,
};
use ratatui_kit::{
    prelude::*,
    ratatui::layout::{Constraint, Direction},
};

/// 弹窗覆盖层组件。
///
/// 订阅 `POPUP_KIND` atom，渲染当前激活弹窗。无弹窗时返回空 View。
#[component]
pub fn PopupOverlay(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let popup_store = hooks.use_store(*atoms::POPUP_KIND.get().unwrap());
    let kind = *popup_store.read();
    let _ = popup_store; // StoreState 是 Copy

    match kind {
        Some(PopupKind::Hitl) => render_popup(element!(HitlPopup()).into()),
        Some(PopupKind::AskUser) => render_popup(element!(AskUserPopup()).into()),
        Some(PopupKind::Rewind) => render_popup(element!(RewindPopup()).into()),
        Some(PopupKind::OAuth) => render_popup(element!(OAuthPopup()).into()),
        None => render_empty(),
    }
}

/// 包裹弹窗——返回原元素（弹窗自带 Border/居中尺寸）。
fn render_popup(p: AnyElement<'static>) -> AnyElement<'static> {
    let _ = (Direction::Vertical, Constraint::Fill(1));
    p
}

/// 空覆盖——无弹窗激活时返回。零尺寸 View。
fn render_empty() -> AnyElement<'static> {
    element!(View(
        flex_direction: Direction::Vertical,
        width: Constraint::Fill(1),
        height: Constraint::Fill(1),
    ))
    .into()
}

// ── 弹窗操作辅助函数（mutates POPUP_KIND atom） ──────────────────────────

/// 打开弹窗（覆盖式）。已打开其他弹窗会被替换。
pub fn open_popup(kind: PopupKind) {
    if let Some(atom) = atoms::POPUP_KIND.get() {
        *atom.write() = Some(kind);
    }
}

/// 关闭当前弹窗（如果有）。返回被关闭的 PopupKind（用于日志/状态反馈）。
pub fn close_popup() -> Option<PopupKind> {
    let atom = atoms::POPUP_KIND.get()?;
    let prev = *atom.read();
    *atom.write() = None;
    prev
}

/// 是否有弹窗激活。
pub fn is_popup_active() -> bool {
    atoms::POPUP_KIND
        .get()
        .map(|a| a.read().is_some())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn setup_atoms() {
        crate::kit::atoms::init_atoms();
        *atoms::POPUP_KIND.get().unwrap().write() = None;
    }

    #[test]
    #[serial]
    fn test_open_popup_sets_atom() {
        setup_atoms();
        open_popup(PopupKind::Hitl);
        assert_eq!(
            *atoms::POPUP_KIND.get().unwrap().read(),
            Some(PopupKind::Hitl)
        );
    }

    #[test]
    #[serial]
    fn test_open_popup_replaces_previous() {
        setup_atoms();
        open_popup(PopupKind::Hitl);
        open_popup(PopupKind::OAuth);
        assert_eq!(
            *atoms::POPUP_KIND.get().unwrap().read(),
            Some(PopupKind::OAuth)
        );
    }

    #[test]
    #[serial]
    fn test_close_popup_returns_previous() {
        setup_atoms();
        open_popup(PopupKind::AskUser);
        let closed = close_popup();
        assert_eq!(closed, Some(PopupKind::AskUser));
        assert_eq!(*atoms::POPUP_KIND.get().unwrap().read(), None);
    }

    #[test]
    #[serial]
    fn test_close_popup_when_empty_returns_none() {
        setup_atoms();
        assert_eq!(close_popup(), None);
    }

    #[test]
    #[serial]
    fn test_is_popup_active() {
        setup_atoms();
        assert!(!is_popup_active());
        open_popup(PopupKind::Rewind);
        assert!(is_popup_active());
        close_popup();
        assert!(!is_popup_active());
    }

    /// 编译期断言：PopupKind 实现 Copy（atom 读取需要 Copy）。
    #[test]
    fn test_popup_kind_is_copy() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<PopupKind>();
    }
}
