//! `NO_COLOR` 根级单点（§12 / Q5）：整帧绘制完成后一次性剥离颜色。
//!
//! `AppShell` 是 ratatui-kit 根组件，其 `post_component_draw` 在所有子组件
//! （主消息区、SubAgent 面板、弹窗等）绘制之后执行，此时 `drawer.buffer_mut()`
//! 就是整帧缓冲区。本模块只决定「何时剥」；剥离规则由
//! [`peri_tui_components::style::strip_buffer_colors`] 持有。

use peri_tui_components::style::strip_buffer_colors;
use ratatui_kit::prelude::*;

use crate::kit::atoms::TERMINAL_CAPS;

/// 根级 `NO_COLOR` 剥离 hook。
///
/// 每帧读取 `TERMINAL_CAPS`：hook 跨帧存活，若在 update 期把能力位捕获进
/// 结构体，探测值变化后仍会用旧值决定是否剥离。
pub(crate) struct NoColorFrameHook;

impl NoColorFrameHook {
    pub(crate) const fn new() -> Self {
        Self
    }
}

impl Hook for NoColorFrameHook {
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        let caps = *TERMINAL_CAPS.state().read();
        if !caps.color {
            strip_buffer_colors(drawer.buffer_mut());
        }
    }
}

#[cfg(test)]
#[path = "no_color_test.rs"]
mod tests;
