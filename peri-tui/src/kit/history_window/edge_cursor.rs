//! 窗口边界游标：驻留布局两端与淘汰段两侧邻居的游标派生。
//!
//! 窗口边界不等于单页边界。页响应只描述它自己覆盖的区间；淘汰端段后，只有仍驻留
//! 的边缘段才能给出指向被淘汰区间的重读游标。

use super::model::{HistoryWindow, RegionKey, SegmentKey};

impl HistoryWindow {
    /// 窗口边界游标：最早驻留段的 `before_cursor` 与最晚驻留段的 `after_cursor`。
    ///
    /// 端部 gap 跳过：它外面的驻留段游标已经指向 gap 内的第一段可读区间。
    pub fn edge_cursors(&self) -> (Option<String>, Option<String>) {
        let before = self
            .layout
            .iter()
            .find_map(|region| match region {
                RegionKey::Resident(key) => Some(
                    self.segments
                        .get(key)
                        .and_then(|segment| segment.before_cursor.clone()),
                ),
                RegionKey::Gap(_) => None,
            })
            .flatten();
        let after = self
            .layout
            .iter()
            .rev()
            .find_map(|region| match region {
                RegionKey::Resident(key) => Some(
                    self.segments
                        .get(key)
                        .and_then(|segment| segment.after_cursor.clone()),
                ),
                RegionKey::Gap(_) => None,
            })
            .flatten();
        (before, after)
    }

    /// 被摘除段两侧仍驻留段的**内侧**游标。
    ///
    /// 返回 `(更早邻居的 after_cursor, 更晚邻居的 before_cursor)`：前者从缺口下端
    /// 向上补，后者从缺口上端向下补。相邻 gap 跳过：它们自身没有可读内容，外侧
    /// 驻留段的游标才指向被摘除区间。
    pub(crate) fn neighbor_cursors(&self, key: SegmentKey) -> (Option<String>, Option<String>) {
        let Some(index) = self.region_index_of_segment(key) else {
            return (None, None);
        };
        let earlier = self
            .layout
            .iter()
            .take(index)
            .rev()
            .find_map(|region| match region {
                RegionKey::Resident(segment) => self.segments.get(segment),
                RegionKey::Gap(_) => None,
            })
            .and_then(|segment| segment.after_cursor.clone());
        let later = self
            .layout
            .iter()
            .skip(index + 1)
            .find_map(|region| match region {
                RegionKey::Resident(segment) => self.segments.get(segment),
                RegionKey::Gap(_) => None,
            })
            .and_then(|segment| segment.before_cursor.clone());
        (earlier, later)
    }
}
