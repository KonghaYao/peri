//! Edit/Write 的 unified diff 应用入口（§6.5，G-Diff）。
//!
//! 解析与保留上限由 `peri-tui-components::diff` 持有；本模块只固定应用默认预算
//! 并保持既有公开签名。调用方（`parse_tool_diff`）传文本与路径提示，非法 /
//! 二进制 / 超限输入返回 `None`，回落到 `diff_change_summary`。
//!
//! 事件流中 Edit/Write 的 `output_summary` 是摘要文本（`Added 3 lines to P` /
//! `Wrote 2 lines to P` / `Replaced 1 line to P`），不含 unified diff；因此
//! `parse_tool_diff` 在 unified 解析失败后回退到 [`parse_edit_write_summary`]。

pub use peri_tui_components::diff::{
    DiffBlock as TuiDiffBlock, DiffBudget, DiffHunk as TuiHunk, DiffHunkLine as TuiHunkLine,
    DiffHunkLineKind as TuiHunkLineKind, MAX_CHANGE_LINES_NEW_FILE, MAX_CHANGE_LINES_PER_HUNK,
    MAX_HUNKS, MAX_LINES_PER_HUNK, MAX_PARSE_LINES,
};

/// 解析 unified diff 文本。`path_hint` 优先（Edit/Write 的 `file_path`），
/// 缺失时从 `+++ b/…` / `diff --git a/… b/…` 头提取。
pub fn parse_unified_diff(text: &str, path_hint: Option<&str>) -> Option<TuiDiffBlock> {
    peri_tui_components::diff::parse_unified_diff(text, path_hint, &DiffBudget::default())
}

/// 解析 Edit/Write 工具的真实摘要输出（`Added 3 lines to P` 等）。
pub fn parse_edit_write_summary(text: &str, path_hint: Option<&str>) -> Option<TuiDiffBlock> {
    peri_tui_components::diff::parse_edit_write_summary(text, path_hint)
}

#[cfg(test)]
#[path = "diff_parser_test.rs"]
mod tests;
