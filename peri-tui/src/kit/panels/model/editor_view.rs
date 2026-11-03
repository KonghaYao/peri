//! Model 面板右栏 K/V 行的鼠标命中辅助。
//!
//! 右栏对所有 provider 都是同一张 K/V 表；命中行号即 `FIELD_*` 索引。

/// 右栏 K/V 行的鼠标语义目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PaneHit {
    /// 普通 K/V 行（值为 `FIELD_*` 索引）。
    Field(usize),
}

/// 右栏 K/V 行的命中表：行号即 `FIELD_*` 索引，与渲染行同序。
pub(super) fn kv_row_hits(field_count: usize) -> Vec<Option<PaneHit>> {
    (0..field_count)
        .map(|field| Some(PaneHit::Field(field)))
        .collect()
}
