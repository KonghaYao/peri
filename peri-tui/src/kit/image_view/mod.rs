//! 图片展示组件：来源身份、等比铺满、缓存/后台编码、上传仓库与共享绘制几何。
//!
//! 这是 TUI 内图片显示的唯一业务入口；`kit::image_preview` 仍是唯一
//! `ratatui-image` 边界，`kit::inline_image` 只保留「视图模型 → 请求」的扫描。
//!
//! 职责边界：
//! - 组件负责：协议选择（经 `image_preview`）、铺满几何（[`sizing`]）、编码/缓存、
//!   上传载荷仓库、失败降级（返回 `None`，调用方保留文本/meta 行）；
//! - 组件不负责：占位行插入、滚动/视觉行几何、点击命中与 hover——留在消息区、
//!   composer 与浮层各自实现。
//!
//! render body 只允许 [`lookup`]（纯查表）；[`request`] 与 [`retain_upload`] 必须在
//! 事件/effect 边界调用（TUI-RENDER-001）。

pub(crate) mod cache;
pub(crate) mod draw;
pub(crate) mod sizing;
pub(crate) mod source;

pub(crate) use cache::{
    decode_image, failed, lookup, release_all_uploads, request, retain_upload, revision,
};
pub(crate) use sizing::{Budget, Fill};

#[cfg(test)]
pub(crate) use cache::request_until_ready;
pub(crate) use source::{ImageSource, UploadId, UploadMedia, digest_of};

#[cfg(test)]
mod draw_test;
