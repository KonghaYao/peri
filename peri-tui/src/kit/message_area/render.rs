//! vm_to_lines + 各变体渲染函数 + 辅助函数。
//!
//! [Slice 3] 统一水平网格（§3.1）：所有 entry 共享 `outer + accent + gap + content`
//! 前缀结构——块首行 `[outer 空][accent 符号][gap]`，续行 `[outer 空][dim 竖线][gap]`；
//! Narrow 断点 accent 符号退化为 bullet（§11）。正文全部从 content 列起点对齐。

mod error;
mod group;
mod helpers;
mod interaction;
mod reasoning;
mod semantic;
mod system;
mod tool_card;
mod user;

#[cfg(test)]
#[path = "render_test.rs"]
mod tests;

mod renderer;
pub(crate) use renderer::CopyButtonInfo;
pub(crate) use renderer::ImageLineInfo;
pub(crate) use renderer::is_step_row;
pub(crate) use renderer::running_header_index;
#[cfg(test)]
pub(crate) use renderer::vm_to_lines;
pub(crate) use renderer::vm_to_lines_cached;
pub(crate) use renderer::vm_to_lines_cached_with_layout;

pub(crate) use self::error::{copy_text_at, subagent_error_buttons};
pub(crate) use self::interaction::InteractionLayout;
pub(crate) use self::reasoning::render_running_reasoning_header;
pub(crate) use self::semantic::semantic_line_text;
pub(crate) use self::user::{parse_image_line, render_image_hover_line};
