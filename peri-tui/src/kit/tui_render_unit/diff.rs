//! 应用侧 diff 展示类型：统一复用 `peri-tui-components` 的有预算解析模型。

pub use peri_tui_components::diff::{
    DiffBlock as TuiDiffBlock, DiffHunk as TuiHunk, DiffHunkLine as TuiHunkLine,
    DiffHunkLineKind as TuiHunkLineKind,
};

/// diff 的 change 行计数（Add/Del 总数）——
/// header `+N −M` 渲染与 [`TuiToolCard::diff_code`](crate::kit::tui_render_unit::TuiToolCard::diff_code) hash 共用。
/// 顶层字段由构造点填充（unified 解析 = hunk 内统计；摘要解析 = 文本计数）。
#[must_use]
pub fn diff_change_counts(diff: &TuiDiffBlock) -> (usize, usize) {
    (diff.adds, diff.dels)
}
