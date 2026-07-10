//! ratatui-kit AppShell root component.

use crate::kit::atoms;
use crate::kit::bg_task_area::BgTaskArea;
use crate::kit::event_handlers;
use crate::kit::layout::SessionColumn;
use crate::kit::popup_overlay::PopupOverlay;
use crate::kit::setup_wizard::SetupWizard;
use crate::kit::status_bar::StatusBar;
use peri_theme::atoms::PALETTE_ATOM;
use ratatui_kit::{
    prelude::*,
    ratatui::layout::{Constraint, Direction},
};

#[component]
pub fn AppShell(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // 订阅全局状态
    let acp_state = hooks.use_atom(&atoms::ACP_STATE);
    let popup_kind = hooks.use_atom(&atoms::POPUP_KIND);
    let wizard_active = hooks.use_atom(&atoms::WIZARD_ACTIVE);
    // 订阅渲染心跳：即使终端无输入，heartbeat 也能周期性唤醒 render loop，
    // 防止窗口切换后 EventStream 永久阻塞。
    let _heartbeat = hooks.use_atom(&atoms::RENDER_HEARTBEAT);
    // 触发 read 确保组件被注册为订阅者；read 返回的 u64 值没在其他地方用，
    // 仅用于保证 ratatui-kit 在 wait() 时将本组件 waker 注入 heartbeat 的订阅表。
    let _heartbeat_val = *_heartbeat.read();

    // 禁用 ratatui-kit 的 Ctrl+C 自动退出——peri 自行管理三级优先级链
    // （Cancel→FirstQuit→Quit）。SystemContext 控制 fullscreen event loop 的
    // Ctrl+C 行为，设为 false 后 Ctrl+C 键盘事件会经 dispatch 流入 Global handler，
    // 由 event_handlers.rs 处理双击退出和 agent 取消。
    {
        let mut ctx = hooks.use_context_mut::<SystemContext>();
        ctx.set_auto_quit_on_ctrl_c(false);
    }

    // 注册事件处理器
    let mut exit_fn = hooks.use_exit();
    event_handlers::register_global_handlers(&mut hooks, Handler::from(move |_: ()| exit_fn()));
    event_handlers::register_root_handlers(&mut hooks);

    // 读取状态值（AcpStateSnapshot 非 Copy，用 .read()）
    let state = acp_state.read();
    let wizard_active = *wizard_active.read();
    let _popup_open = popup_kind.read().is_some();
    let _ = state; // AcpStateSnapshot 借用解除

    // 设置向导覆盖（最高优先级）；否则显示主布局 + 弹窗覆盖层。
    // 面板由 SessionColumn 放在消息流与输入区之间，不再作为根级浮层渲染。
    let palette_handle = hooks.use_atom(&PALETTE_ATOM);
    let palette = palette_handle.read().clone();

    if wizard_active {
        element! {
            PaletteProvider(palette) {
                View(
                    flex_direction: Direction::Vertical,
                    width: Constraint::Fill(1),
                    height: Constraint::Fill(1),
                ) {
                    SetupWizard()
                }
            }
        }
    } else {
        element! {
            PaletteProvider(palette) {
                View(
                    width: Constraint::Fill(1),
                    height: Constraint::Fill(1),
                ) {
                    View(
                        flex_direction: Direction::Vertical,
                        width: Constraint::Fill(1),
                        height: Constraint::Fill(1),
                    ) {
                        SessionColumn()
                        StatusBar()
                        BgTaskArea()
                    }
                    PopupOverlay()
                }
            }
        }
    }
}
