//! Mermaid 边生成边预览（第 2 部分）：可信中间帧、闭合定稿与历史重建。
//!
//! 渲染只接收已确认的源码前缀；未确认部分始终以源码尾部显示。闭合后走与
//! 历史重建相同的纯函数，异步排版结果必须通过版本与布局参数校验才发布。

mod display;
mod frame;
mod preview;
mod store;
mod stream;

pub(crate) use display::{PreviewScope, display_lines, vm_might_have_mermaid};
#[cfg(test)]
pub(crate) use store::{drain_preview_jobs_now, reset_for_test};
pub(crate) use store::{init_preview_worker, revision};
