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
    confirm_popup::ConfirmPopup, hitl_popup::HitlPopup, oauth_popup::OAuthPopup,
    rewind_popup::RewindPopup,
};
use crate::kit::theme;
use ratatui_kit::{prelude::*, ratatui::layout::Constraint};

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
        None => render_empty(),
    }
}

/// 包裹弹窗——只定位和清除弹窗矩形，避免 Modal 整屏背景绘制导致白屏。
fn render_popup(p: AnyElement<'static>, term_w: u16, term_h: u16) -> AnyElement<'static> {
    let popup = &theme::component().popup;
    let width = term_w.saturating_sub(4).min(popup.modal_max_width).max(1);
    let height = term_h.saturating_sub(4).min(popup.modal_max_height).max(1);
    let x = term_w.saturating_sub(width) / 2;
    let y = term_h.saturating_sub(height) / 2;

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
            PopupKind::Rewind => *atoms::REWIND_PREVIEW.state().write() = None,
            PopupKind::OAuth => *atoms::OAUTH_INFO.state().write() = None,
            PopupKind::Confirm => *atoms::CONFIRM_PAYLOAD.state().write() = None,
        }
    }
    prev
}

/// 是否有弹窗激活。
pub fn is_popup_active() -> bool {
    atoms::POPUP_KIND.state().read().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn setup_atoms() {
        crate::kit::atoms::init_atoms();
        *atoms::POPUP_KIND.state().write() = None;
    }

    #[test]
    #[serial]
    fn test_open_popup_sets_atom() {
        setup_atoms();
        open_popup(PopupKind::Hitl);
        assert_eq!(*atoms::POPUP_KIND.state().read(), Some(PopupKind::Hitl));
        // 清理——避免全局 OnceLock atom 在测试间残留 POPUP_KIND 状态
        close_popup();
    }

    #[test]
    #[serial]
    fn test_close_popup_returns_previous() {
        setup_atoms();
        open_popup(PopupKind::AskUser);
        let closed = close_popup();
        assert_eq!(closed, Some(PopupKind::AskUser));
        assert_eq!(*atoms::POPUP_KIND.state().read(), None);
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

    /// I21-C 回归保护：close_popup 必须根据关闭的 kind 清空对应 payload atom。
    /// 防止未来重构破坏 atom 清空语义——用户打开 popup → 看到 payload → 关闭 →
    /// 下次再打开时不应看到陈旧 payload（即使没有新事件）。
    #[test]
    #[serial]
    fn test_close_popup_clears_payload_atoms() {
        use peri_acp_types::event_data::{
            AskUser, HitlPending, OauthNeeded, Question, RewindPreview,
        };

        setup_atoms();

        // 构造 4 种 popup 的 payload 写入对应 atom
        *atoms::HITL_PENDING.state().write() = Some(HitlPending {
            tool_name: "rm".to_string(),
            tool_input: serde_json::Value::Null,
            batch: None,
        });
        *atoms::ASK_USER_PENDING.state().write() = Some(AskUser {
            questions: vec![Question {
                id: "q1".to_string(),
                header: "h".to_string(),
                question: "q".to_string(),
                options: vec![],
                multi_select: false,
            }],
        });
        *atoms::REWIND_PREVIEW.state().write() = Some(RewindPreview {
            files: vec![],
            messages: vec![],
        });
        *atoms::OAUTH_INFO.state().write() = Some(OauthNeeded {
            server_name: "test".to_string(),
            auth_url: "https://example.com".to_string(),
        });

        // 逐一关闭每种 popup，验证对应 atom 被清空
        open_popup(PopupKind::Hitl);
        close_popup();
        assert!(
            atoms::HITL_PENDING.state().read().is_none(),
            "HITL_PENDING should be cleared after close_popup"
        );

        open_popup(PopupKind::AskUser);
        close_popup();
        assert!(
            atoms::ASK_USER_PENDING.state().read().is_none(),
            "ASK_USER_PENDING should be cleared after close_popup"
        );

        open_popup(PopupKind::Rewind);
        close_popup();
        assert!(
            atoms::REWIND_PREVIEW.state().read().is_none(),
            "REWIND_PREVIEW should be cleared after close_popup"
        );

        open_popup(PopupKind::OAuth);
        close_popup();
        assert!(
            atoms::OAUTH_INFO.state().read().is_none(),
            "OAUTH_INFO should be cleared after close_popup"
        );
    }
}
