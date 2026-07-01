//! ratatui-kit SessionColumn layout component.

// element! 宏展开触发 clippy::needless_update（ratatui-kit 上游问题），模块级抑制。
#![allow(clippy::needless_update)]

use crate::kit::atoms;
use crate::kit::input_area::InputArea;
use crate::kit::message_area::MessageArea;
use ratatui_kit::{
    prelude::*,
    ratatui::layout::{Constraint, Direction},
};

#[component]
pub fn SessionColumn(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // 订阅数据
    let vms = hooks.use_store(*atoms::VIEW_MODELS.get().unwrap());
    let acp = hooks.use_store(*atoms::ACP_STATE.get().unwrap());

    // 克隆出 Props 拥有的数据（RwLockReadGuard 不能跨 element! 边界）
    let snapshot = vms.read().clone();
    let loading = acp.read().is_loading;

    // I18-A：响应式终端宽度——替代硬编码 width=100，让 markdown 折行准。
    // 减去 4 列内边距（左右各 2）保留视觉透气感；下限 20 防极窄终端溢出。
    let (term_w, _) = hooks.use_terminal_size();
    let width: usize = (term_w as usize).saturating_sub(4).max(20);

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            // 消息区（吃剩余高度）—— 自管 ScrollViewState，Ctrl+Up/Down 滚动
            MessageArea(
                view_models: snapshot.committed,
                current_turn: snapshot.current_turn,
                loading: loading,
                width: width,
            )

            // 输入区（底部固定高度）
            InputArea(loading: loading)
        }
    )
}
