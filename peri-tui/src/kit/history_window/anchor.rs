//! 窗口锚点捕获/重定位、gap 合并与 seek 目标选择（纯函数 + 最小窗口状态更新）。

use std::collections::{HashMap, VecDeque};

use super::model::{
    GapId, GapPlaceholder, HistoryGap, HistorySource, HistoryWindow, PersistentEntryKey, PinReason,
    RegionKey, SegmentKey, WindowAnchor,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnchorRelocation {
    Resident {
        segment: SegmentKey,
        entry: PersistentEntryKey,
        sub_item: Option<u32>,
    },
    Gap {
        gap_id: GapId,
        seek: Option<GapSeek>,
    },
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GapSeekDirection {
    Before,
    After,
}

/// 缺口重读目标：中间缺口优先按稳定身份 seek；单向缺口用页游标。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GapSeek {
    Page {
        gap_id: GapId,
        direction: GapSeekDirection,
        cursor: String,
    },
    Seek {
        gap_id: GapId,
        anchor: PersistentEntryKey,
        source: HistorySource,
    },
}

/// 合并布局中相邻且同源的 gap。保留前一个 gap 的身份，边界取并集。
pub fn merge_adjacent_gaps(
    layout: &mut VecDeque<RegionKey>,
    gaps: &mut HashMap<GapId, HistoryGap>,
) {
    let mut index = 0usize;
    while index + 1 < layout.len() {
        let left_id = match &layout[index] {
            RegionKey::Gap(id) => Some(*id),
            RegionKey::Resident(_) => None,
        };
        let right_id = match &layout[index + 1] {
            RegionKey::Gap(id) => Some(*id),
            RegionKey::Resident(_) => None,
        };
        let (Some(left_id), Some(right_id)) = (left_id, right_id) else {
            index += 1;
            continue;
        };
        let Some(right) = gaps.remove(&right_id) else {
            index += 1;
            continue;
        };
        let Some(left) = gaps.get_mut(&left_id) else {
            index += 1;
            continue;
        };
        if left.source != right.source {
            // 不同来源不能合并；把右 gap 放回，继续扫描。
            gaps.insert(right_id, right);
            index += 1;
            continue;
        }
        left.before = left.before.clone().or(right.before);
        left.after = right.after.clone().or(left.after.clone());
        // 游标取合并后缺口的**内侧**边界：上端来自更晚 gap 的 before 游标，
        // 下端保留更早 gap 的 after 游标。外侧游标锚在已被淘汰的段上，无法
        // 回填合并后的缺口；对应方向没有驻留邻居时保持 None。
        left.before_cursor = right.before_cursor;
        left.placeholder = left.placeholder.merge(right.placeholder);
        layout.remove(index + 1);
        // 不前进 index：合并后的左 gap 可能与下一个 gap 继续相邻。
    }
}

/// 缺失锚点是否落在该 gap 的边界内。
///
/// 只有边界身份可以直接判定；中间缺口没有逐条目录，若两侧边界都存在则只有锚点恰好
/// 等于边界时才判定覆盖。调用方在只有一个同源 gap 时可把它视为唯一候选。
pub fn gap_covers_anchor(gap: &HistoryGap, anchor: &WindowAnchor) -> bool {
    if gap.source != anchor.entry.source {
        return false;
    }
    if gap.before.as_ref() == Some(&anchor.entry) || gap.after.as_ref() == Some(&anchor.entry) {
        return true;
    }
    match (&gap.before, &gap.after) {
        (None, Some(_)) | (Some(_), None) | (None, None) => true,
        (Some(_), Some(_)) => false,
    }
}

/// 选择缺口重读目标。中间缺口用 seek；单边缺口优先用页游标。
pub fn select_gap_seek(gap: &HistoryGap, anchor: &WindowAnchor) -> Option<GapSeek> {
    if gap.placeholder == GapPlaceholder::MissingMiddle {
        return Some(GapSeek::Seek {
            gap_id: gap.id,
            anchor: anchor.entry.clone(),
            source: anchor.entry.source.clone(),
        });
    }
    if let Some(cursor) = &gap.before_cursor {
        return Some(GapSeek::Page {
            gap_id: gap.id,
            direction: GapSeekDirection::Before,
            cursor: cursor.clone(),
        });
    }
    if let Some(cursor) = &gap.after_cursor {
        return Some(GapSeek::Page {
            gap_id: gap.id,
            direction: GapSeekDirection::After,
            cursor: cursor.clone(),
        });
    }
    (gap.before.is_some() || gap.after.is_some()).then(|| GapSeek::Seek {
        gap_id: gap.id,
        anchor: anchor.entry.clone(),
        source: anchor.entry.source.clone(),
    })
}

impl HistoryWindow {
    /// 设置/清除窗口锚点，并维护 `Viewport` pin。返回锚点当前落点。
    pub fn set_anchor(&mut self, anchor: Option<WindowAnchor>) -> AnchorRelocation {
        let keys: Vec<SegmentKey> = self.segments().keys().copied().collect();
        for key in keys {
            self.unpin(key, PinReason::Viewport);
        }
        self.anchor = anchor;
        let relocation = self.relocate_anchor();
        if let AnchorRelocation::Resident { segment, .. } = relocation {
            self.pin(segment, PinReason::Viewport);
        }
        relocation
    }

    /// 按持久身份重定位锚点，不修改窗口。锚点条目缺失时转为 gap 定位请求。
    pub fn relocate_anchor(&self) -> AnchorRelocation {
        let Some(anchor) = self.anchor() else {
            return AnchorRelocation::Unavailable;
        };
        if let Some(locator) = self.index().locate_entry(&anchor.entry) {
            return AnchorRelocation::Resident {
                segment: locator.segment,
                entry: anchor.entry.clone(),
                sub_item: anchor.sub_item,
            };
        }
        let mut matching = self
            .layout()
            .iter()
            .filter_map(|region| match region {
                RegionKey::Gap(id) => self.gaps().get(id),
                RegionKey::Resident(_) => None,
            })
            .filter(|gap| gap_covers_anchor(gap, anchor));
        let first = matching.next();
        let second = matching.next();
        // 有多个候选时不能猜；只有唯一同源 gap 才允许把它当作锚点容器。
        let gap = match (first, second) {
            (Some(gap), None) => gap,
            _ => {
                let same_source: Vec<&HistoryGap> = self
                    .gaps()
                    .values()
                    .filter(|gap| gap.source == anchor.entry.source)
                    .collect();
                if same_source.len() == 1 {
                    same_source[0]
                } else {
                    return AnchorRelocation::Unavailable;
                }
            }
        };
        AnchorRelocation::Gap {
            gap_id: gap.id,
            seek: select_gap_seek(gap, anchor),
        }
    }

    pub fn set_follow_bottom(&mut self, follow_bottom: bool) {
        if let Some(anchor) = self.anchor.as_mut() {
            anchor.follow_bottom = follow_bottom;
        }
    }

    pub fn follow_bottom(&self) -> bool {
        self.anchor().is_some_and(|anchor| anchor.follow_bottom)
    }
}
