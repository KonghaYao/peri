//! ratatui-kit AppShell root component.

use ratatui_kit::{
    prelude::*,
    ratatui::layout::{Constraint, Direction},
};
use crate::kit::atoms;
use crate::kit::event_handlers;
use crate::kit::layout::SessionColumn;
use crate::kit::setup_wizard::SetupWizard;
use crate::kit::status_bar::StatusBar;

#[component]
pub fn AppShell(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // 订阅全局状态
    let acp_state = hooks.use_store(*atoms::ACP_STATE.get().unwrap());
    let popup_active = hooks.use_store(*atoms::POPUP_ACTIVE.get().unwrap());

    // 注册事件处理器
    let mut exit_fn = hooks.use_exit();
    event_handlers::register_global_handlers(&mut hooks, Handler::from(move |_: ()| exit_fn()));
    event_handlers::register_root_handlers(&mut hooks);

    // 读取状态值 (AcpStateSnapshot 非 Copy，用 .read())
    let state = acp_state.read();
    let wizard_active = state.wizard_active;
    let _ = popup_active.get();
    drop(state);

    // 设置向导覆盖（最高优先级）；否则显示主布局
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
            }
        )
    }
}
