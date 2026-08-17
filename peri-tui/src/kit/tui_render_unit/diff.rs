/// Inline diff preview (for Write / Edit tool results).
#[derive(Debug, Clone, PartialEq)]
pub struct TuiDiffBlock {
    /// File path the diff applies to.
    pub path: String,
    pub hunks: Vec<TuiHunk>,
    /// Binary file -- cannot display diff.
    pub is_binary: bool,
    /// Diff content exceeded safe size limit.
    pub is_too_large: bool,
    /// New file (Write, or Edit with empty old_string) -- cap at 6 lines.
    pub is_new_file: bool,
    /// [G-Diff] 首个 hunk 之后所有未展示 hunk 的 change（`+`/`-`）行总数——
    /// 渲染层在首个 hunk 后显示 `… +N more lines`（§6.5）。
    pub more_change_lines: usize,
    /// [G-Diff] 顶层 `+`/`-` 总计数（header `+N −M` 渲染与 hash 共用）：
    /// unified diff 时 = 全部 hunk 内 change 行数；摘要解析
    /// （`parse_edit_write_summary`，无 hunk）时 = 摘要提取的行数。
    pub adds: usize,
    pub dels: usize,
}

/// A single diff hunk.
#[derive(Debug, Clone, PartialEq)]
pub struct TuiHunk {
    /// Header range string for the old side (e.g. "@@ -1,3 +1,4 @@").
    pub old_range: String,
    /// Header range string for the new side.
    pub new_range: String,
    pub lines: Vec<TuiHunkLine>,
    /// [G-Diff] 本 hunk 内超出上限（§6.5「最多 8 个 change 行」）被截断的
    /// change 行数——渲染层追加 `… +N more lines`。
    pub truncated_lines: usize,
}

/// One line inside a diff hunk.
#[derive(Debug, Clone, PartialEq)]
pub struct TuiHunkLine {
    pub kind: TuiHunkLineKind,
    /// Content text (without the leading +/- or space prefix).
    pub text: String,
    /// Line number on the old side (None for pure-add lines).
    pub old_no: Option<u32>,
    /// Line number on the new side (None for pure-delete lines).
    pub new_no: Option<u32>,
}

/// Classification of a single diff line.
#[derive(Debug, Clone, PartialEq)]
pub enum TuiHunkLineKind {
    /// Unchanged context line.
    Context,
    /// Added line.
    Add,
    /// Deleted line.
    Del,
}

/// [G-Diff] diff 的 change 行计数（Add/Del 总数）——
/// header `+N −M` 渲染与 [`TuiToolCard::diff_code`] hash 共用。
/// 顶层字段由构造点填充（unified 解析 = hunk 内统计；摘要解析 = 文本计数）。
pub fn diff_change_counts(diff: &TuiDiffBlock) -> (usize, usize) {
    (diff.adds, diff.dels)
}
