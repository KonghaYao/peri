//! Markdown parsing and block conversion for terminal rendering.
//!
//! 底层委托给 `ratatui_kit_markdown::parse_markdown`（公开 API），
//! 自行实现 `ParsedBlock` → `Line<'static>` 转换以适配渲染管线。
//! `ratatui_kit_markdown` 的 `RenderRow` / `render_rows_with_theme` 为
//! `pub(crate)`，外部不可用——此处复刻了 `style_spans` / `semantic_style`
//! 及块间距逻辑。
//!
//! 子模块组织：
//! - `types`：MarkdownSegment, TableData
//! - `span_style`：apply_span_styles, span_semantic_style
//! - `heading`：heading_line（不渲染 # 前缀）
//! - `list`：list_item_line、list_item_gap（列表项间空行范围）
//! - `code_block`：highlight_code_block, code_block_lines, syntect 单例
//! - `table`：compute_table_col_widths（最长不可断词约束）、table_data_to_lines
//!   （智能断词换行 + unicode 网格线渲染）
//! - `convert`：convert_to_segments（块级分发）
//! - `scan`：图片前置扫描 + 占位替换
//! - `fence`：原始 Markdown 的 Mermaid 围栏范围与闭合状态
//! - `safety`：终端控制字符过滤与 URL scheme 分类
//! - `metrics`：解析路径观测计数

mod boundary;
mod code_block;
mod convert;
pub mod fence;
mod heading;
mod list;
mod memory;
pub mod metrics;
pub mod safety;
mod scan;
mod span_style;
mod table;
pub mod types;

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

#[cfg(test)]
#[path = "list_test.rs"]
mod list_tests;

#[cfg(test)]
#[path = "cache_test.rs"]
mod cache_tests;

#[cfg(test)]
#[path = "table_parse_test.rs"]
mod table_parse_tests;

#[cfg(test)]
#[path = "wrap_test.rs"]
mod wrap_tests;

#[cfg(test)]
#[path = "image_parse_test.rs"]
mod image_parse_tests;

#[cfg(test)]
#[path = "cache_lifecycle_test.rs"]
mod cache_lifecycle_tests;

#[cfg(test)]
#[path = "boundary_test.rs"]
mod boundary_tests;

#[cfg(test)]
#[path = "fence_test.rs"]
mod fence_tests;

#[cfg(test)]
#[path = "genui_test.rs"]
mod genui_tests;

#[cfg(test)]
#[path = "memory_test.rs"]
mod memory_tests;

#[cfg(test)]
#[path = "workload_test.rs"]
mod workload_tests;

#[cfg(test)]
use ratatui::style::Color;
#[cfg(test)]
use ratatui_kit::prelude::Palette;
#[cfg(test)]
use ratatui_kit_markdown::ParsedBlock;
#[cfg(test)]
use std::borrow::Cow;
#[cfg(test)]
use std::sync::Arc;

mod parser;
pub use parser::*;

pub use fence::{
    FenceClose, FenceKind, MermaidFence, MermaidFenceKey, MermaidMessageIdentity,
    MermaidMessageRef, is_mermaid_info,
};
pub use safety::{UrlKind, classify_url, sanitize_for_terminal};
pub(crate) use table::buffer_to_lines;
pub use table::table_data_to_lines;
pub use types::{GenUiSegment, ImageSegment, MarkdownSegment, MermaidSegment, TableData};
