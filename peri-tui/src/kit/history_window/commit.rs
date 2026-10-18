//! `peri/history_committed` 展示提交事实：校验、按 checkpoint 幂等、封印持久段。
//!
//! 本模块只改窗口模型（durable/sealed/水位），不释放正文、不触发渲染，也不碰任何
//! 执行状态。真正的段释放仍由 S6.2 统一事务消费 `ReleasePlan`。

use std::collections::HashSet;

use serde_json::Value;

use super::model::{
    DurableWatermark, HistoryWindow, PinReason, RegionKey, ReleasePlan, WindowIdentity,
};
use super::{AnchorRelocation, SegmentKey};
use peri_acp_types::history_window::HistoryCommittedTail;
use peri_acp_types::thread::ThreadId;

/// 私有通知 `peri/history_committed` 的客户端视图。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryCommitInfo {
    pub session_id: String,
    pub session_generation: String,
    pub structure_version: u64,
    pub checkpoint_id: String,
    pub summary_entry_id: Option<String>,
    /// 不透明 committed tail；只在应用时解码。
    pub committed_tail: String,
}

impl HistoryCommitInfo {
    pub fn decode(value: &Value) -> Option<Self> {
        let session_id = value.get("sessionId")?.as_str()?.to_string();
        let session_generation = value.get("sessionGeneration")?.as_str()?.to_string();
        let structure_version = value.get("structureVersion")?.as_u64()?;
        let checkpoint_id = value.get("checkpointId")?.as_str()?.to_string();
        let summary_entry_id = value
            .get("summaryEntryId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let committed_tail = value.get("committedTail")?.as_str()?.to_string();
        if session_id.is_empty()
            || session_generation.is_empty()
            || checkpoint_id.is_empty()
            || committed_tail.is_empty()
        {
            return None;
        }
        Some(Self {
            session_id,
            session_generation,
            structure_version,
            checkpoint_id,
            summary_entry_id,
            committed_tail,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitOutcome {
    Applied { sealed_segments: usize },
    Duplicate,
    StaleGeneration,
    UnknownWindow,
    InvalidTail,
}

impl HistoryWindow {
    /// 应用一次提交事实。旧 generation/structure 丢弃；同 checkpoint 幂等。
    ///
    /// `DisplayCommitted`/`TurnCommitted` 不在本阶段消费：没有持久身份回执的
    /// `current_turn` 不封印、不淘汰。
    pub fn apply_commit(&mut self, info: &HistoryCommitInfo) -> CommitOutcome {
        let Some(identity) = self.identity() else {
            return CommitOutcome::UnknownWindow;
        };
        if !commit_matches_identity(identity, info) {
            return CommitOutcome::StaleGeneration;
        }
        let thread = ThreadId::from(info.session_id.clone());
        let Ok(tail) = HistoryCommittedTail::decode(&info.committed_tail, &thread) else {
            return CommitOutcome::InvalidTail;
        };
        if !self.note_checkpoint(info.checkpoint_id.clone()) {
            return CommitOutcome::Duplicate;
        }
        let sealed_segments = self.seal_through_position(tail.position);
        self.set_durable_watermark(DurableWatermark {
            checkpoint_id: Some(info.checkpoint_id.clone()),
            committed_tail: Some(info.committed_tail.clone()),
        });
        CommitOutcome::Applied { sealed_segments }
    }

    /// 标记所有条目位置 `<= position` 的段为 durable + sealed。
    ///
    /// 没有 `position` 的 live/未确认条目永远不满足条件，因此不会被 turn 结束或
    /// compact 完成本身误封印。
    fn seal_through_position(&mut self, position: u64) -> usize {
        let mut sealed = 0usize;
        let mut promoted = Vec::new();
        for (key, segment) in self.segments.iter_mut() {
            let all_committed = !segment.entries.is_empty()
                && segment
                    .entries
                    .iter()
                    .all(|entry| entry.position.is_some_and(|value| value <= position));
            if !all_committed {
                continue;
            }
            segment.sealed = true;
            if !segment.durable {
                segment.durable = true;
                promoted.push(*key);
            }
            sealed += 1;
        }
        for key in promoted {
            // 未持久化 pin 在 durable 确认后解除；其余 pin（viewport/running 等）保留。
            self.unpin(key, PinReason::Unpersisted);
        }
        sealed
    }

    pub fn sealed_segment_count(&self) -> usize {
        self.segments
            .values()
            .filter(|segment| segment.sealed)
            .count()
    }

    /// 提交后的保留决策：视口段与其相邻段保留；其余已封印、可淘汰段生成释放计划。
    /// 本阶段只返回计划，不执行释放（S6.2 消费）。
    pub fn retention_plan_after_commit(&self) -> Vec<ReleasePlan> {
        let keep = self.viewport_and_adjacent_segments();
        let mut plans = Vec::new();
        for region in self.layout() {
            let RegionKey::Resident(key) = *region else {
                continue;
            };
            if keep.contains(&key) {
                continue;
            }
            let Some(segment) = self.segment(key) else {
                continue;
            };
            if !segment.durable || !segment.sealed || segment.pins.is_pinned() {
                continue;
            }
            let (before, after) = self.neighbor_entries(key);
            plans.push(ReleasePlan {
                segment: key,
                source: segment.source.clone(),
                entries: segment.entry_keys(),
                unit_count: segment.unit_count,
                body_bytes: segment.body_bytes,
                reserved_bytes: segment.reserved_bytes,
                slot_range: self.index().slot_range(key),
                before,
                after,
                before_cursor: segment.before_cursor.clone(),
                after_cursor: segment.after_cursor.clone(),
            });
        }
        plans
    }

    fn viewport_and_adjacent_segments(&self) -> HashSet<SegmentKey> {
        let mut keep = HashSet::new();
        let AnchorRelocation::Resident { segment, .. } = self.relocate_anchor() else {
            return keep;
        };
        keep.insert(segment);
        let Some(index) = self.region_index_of_segment(segment) else {
            return keep;
        };
        let previous = self
            .layout()
            .iter()
            .take(index)
            .rev()
            .find_map(|region| match region {
                RegionKey::Resident(key) => Some(*key),
                RegionKey::Gap(_) => None,
            });
        let next = self
            .layout()
            .iter()
            .skip(index + 1)
            .find_map(|region| match region {
                RegionKey::Resident(key) => Some(*key),
                RegionKey::Gap(_) => None,
            });
        if let Some(previous) = previous {
            keep.insert(previous);
        }
        if let Some(next) = next {
            keep.insert(next);
        }
        keep
    }
}

fn commit_matches_identity(identity: &WindowIdentity, info: &HistoryCommitInfo) -> bool {
    identity.session_id == info.session_id
        && identity.session_generation == info.session_generation
        && identity.structure_version == info.structure_version
}
