//! ratatui-kit SessionColumn layout component.

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
    let scroll = hooks.use_store(*atoms::SCROLL_OFFSET.get().unwrap());
    let acp = hooks.use_store(*atoms::ACP_STATE.get().unwrap());

    // 克隆出 Props 拥有的数据（RwLockReadGuard 不能跨 element! 边界）
    let snapshot = vms.read().clone();
    let scroll_offset = scroll.get();
    let loading = acp.read().is_loading;

    // TODO(S6): 通过 use_layout_query 获取真实 area.width；目前先固定 100，
    // mock 阶段折行够用。S6 接入 ratatui-kit 的 area 查询 hook 后改为动态宽度。
    let width: usize = 100;

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            // 消息区（吃剩余高度）
            MessageArea(
                view_models: snapshot.committed,
                current_turn: snapshot.current_turn,
                scroll_offset: scroll_offset,
                loading: loading,
                width: width,
            )

            // 输入区（底部固定高度）
            InputArea(loading: loading)
        }
    )
}
