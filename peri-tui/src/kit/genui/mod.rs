//! GenUI 显示适配的应用包装。
//!
//! 渲染实现位于 `peri-tui-components`（纯函数）；交互状态（`Set`/`Reset` 的
//! `EvalContext`）属于会话，留在本 crate 的 [`view_state`]，渲染时注入。
//! 组件类型直接从 `peri_tui_components::genui` 引用，本模块不再转发。

pub mod view_state;

use peri_theme::theme::ThemeDefinition;

use crate::kit::markdown::GenUiSegment;
use peri_tui_components::genui::GenUiRender;

/// 渲染一个 OpenUI 段，并注入该块当前的交互状态。
pub fn render(segment: &GenUiSegment, width: usize, theme: &ThemeDefinition) -> GenUiRender {
    let context = view_state::context(segment.block_id());
    peri_tui_components::genui::render(segment, width, theme, context.as_ref())
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
