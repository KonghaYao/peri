//! ratatui-kit AppShell root component.

use crate::kit::atoms;
use crate::kit::event_handlers;
use crate::kit::layout::SessionColumn;
use crate::kit::panel_overlay::PanelOverlay;
use crate::kit::popup_overlay::PopupOverlay;
use crate::kit::setup_wizard::SetupWizard;
use crate::kit::status_bar::StatusBar;
use ratatui_kit::{
    prelude::*,
    ratatui::layout::{Constraint, Direction},
};

#[component]
pub fn AppShell(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // 订阅全局状态
    let acp_state = hooks.use_store(*atoms::ACP_STATE.get().unwrap());
    let popup_active = hooks.use_store(*atoms::POPUP_ACTIVE.get().unwrap());
    let wizard_active_atom = hooks.use_store(*atoms::WIZARD_ACTIVE.get().unwrap());

    // 注册事件处理器
    let mut exit_fn = hooks.use_exit();
    event_handlers::register_global_handlers(&mut hooks, Handler::from(move |_: ()| exit_fn()));
    event_handlers::register_root_handlers(&mut hooks);

    // 读取状态值 (AcpStateSnapshot 非 Copy，用 .read())
    let state = acp_state.read();
    let wizard_active = *wizard_active_atom.read();
    let _ = popup_active.get();
    let _ = state; // AcpStateSnapshot 借用解除

    // 设置向导覆盖（最高优先级）；否则显示主布局 + 面板覆盖层 + 弹窗覆盖层
    // 叠加顺序：SessionColumn/StatusBar → PanelOverlay → PopupOverlay
    // （后到子节点覆盖前节点；弹窗在面板之上）
    if wizard_active {
        element!(
            View(
                flex_direction: Direction::Vertical,
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                SetupWizard()
            }
        )
    } else {
        element!(
            View(
                flex_direction: Direction::Vertical,
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                SessionColumn()
                StatusBar()
                PanelOverlay()
                PopupOverlay()
            }
        )
    }
}
