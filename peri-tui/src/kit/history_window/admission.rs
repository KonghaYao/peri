//! 页预留、接纳、pin 与淘汰决策。
//!
//! 这里的“释放”只更新窗口模型与账本，并生成 [`ReleasePlan`]；`TuiRenderUnit`、
//! 折叠投影、渲染缓存等正文持有者的删除属于 S6.2 的统一释放事务。`admit_page`
//! 在预算压力下会调用 [`HistoryWindow::apply_release_plan`] 移出模型段，但不会删除
//! 任何渲染对象；调用方必须把返回的 `released` 计划交给 S6.2 接入点。

use std::collections::HashSet;

use super::anchor::merge_adjacent_gaps;
use super::model::{
    GapId, GapPlaceholder, HistoryGap, HistoryWindow, PageReservation, PendingMerge,
    PersistentEntryKey, PinReason, RegionKey, ReleasePlan, SegmentDraft, SegmentKey,
    SegmentPlacement, WindowIdentity, WindowModelError,
};

/// 页在 `committed` 中的插入位置。`BeforeEntry` 由模型解析成所在段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PagePlacement {
    Tail,
    BeforeSegment(SegmentKey),
    BeforeEntry(PersistentEntryKey),
    /// 原位补回缺口：段落在缺口所在的布局位置，成功后移除该缺口。
    FillGap(GapId),
}

/// 一次页接纳的输入。`segments` 已经过投影构造，但尚未登记进窗口索引。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageDraft {
    pub request_id: String,
    pub window_generation: u64,
    pub identity: WindowIdentity,
    pub placement: PagePlacement,
    pub segments: Vec<SegmentDraft>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReserveOutcome {
    Granted(PageReservation),
    Disabled,
    Rejected(ReserveRejection),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReserveRejection {
    Degraded {
        reason: String,
    },
    DuplicateRequest,
    TooManyInFlight {
        active: usize,
        limit: usize,
    },
    ReservedBudgetExceeded {
        reserved: usize,
        limit: usize,
        requested: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedReport {
    pub reasons: Vec<PinReason>,
    pub pinned_segments: usize,
    pub pinned_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvictionBlocked {
    pub needed_body_bytes: usize,
    pub freed_body_bytes: usize,
    pub pinned: PinnedReport,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct EvictionPlan {
    pub plans: Vec<ReleasePlan>,
    pub needed_body_bytes: usize,
    pub freed_body_bytes: usize,
    pub freed_reserved_bytes: usize,
    pub blocked: Option<EvictionBlocked>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ReleaseSummary {
    pub plans: Vec<ReleasePlan>,
    pub released_segments: usize,
    pub released_entries: usize,
    pub body_bytes: usize,
    pub reserved_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmissionDecision {
    Accepted {
        segments: Vec<SegmentKey>,
        released: Vec<ReleasePlan>,
        body_bytes: usize,
        pending_merge_bytes: usize,
    },
    Rejected(AdmissionRejection),
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmissionRejection {
    pub reason: AdmissionRejectionReason,
    pub released: Vec<ReleasePlan>,
    pub reservation_released: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmissionRejectionReason {
    Degraded {
        reason: String,
    },
    StaleIdentity {
        expected_generation: u64,
        actual_generation: u64,
    },
    MissingReservation,
    PendingMergeLimit,
    PlacementUnavailable,
    InvalidSegment(WindowModelError),
    PageExceedsBudget {
        incoming: usize,
        limit: usize,
    },
    EvictionBlocked(EvictionBlocked),
}

impl HistoryWindow {
    /// 页请求发起前预留容量下界。预留只占 reserved，不占 body。
    pub fn reserve_page(
        &mut self,
        request_id: impl Into<String>,
        estimated_bytes: usize,
    ) -> ReserveOutcome {
        if !self.is_enabled() {
            return ReserveOutcome::Disabled;
        }
        if let super::WindowStatus::Degraded { reason } = self.status() {
            return ReserveOutcome::Rejected(ReserveRejection::Degraded {
                reason: reason.clone(),
            });
        }
        let request_id = request_id.into();
        if self.reservations.contains_key(&request_id) {
            return ReserveOutcome::Rejected(ReserveRejection::DuplicateRequest);
        }
        let active = self.reservations.len();
        if active >= self.budget.limits.max_in_flight_pages {
            return ReserveOutcome::Rejected(ReserveRejection::TooManyInFlight {
                active,
                limit: self.budget.limits.max_in_flight_pages,
            });
        }
        let reserved = self.budget.page_reserved_bytes;
        if reserved.saturating_add(estimated_bytes) > self.budget.limits.reserved_bytes {
            return ReserveOutcome::Rejected(ReserveRejection::ReservedBudgetExceeded {
                reserved,
                limit: self.budget.limits.reserved_bytes,
                requested: estimated_bytes,
            });
        }
        let reservation = PageReservation {
            request_id: request_id.clone(),
            window_generation: self.generation(),
            bytes: estimated_bytes,
        };
        self.budget.page_reserved_bytes = self
            .budget
            .page_reserved_bytes
            .saturating_add(estimated_bytes);
        self.reservations.insert(request_id, reservation.clone());
        ReserveOutcome::Granted(reservation)
    }

    pub fn release_page_reservation(&mut self, request_id: &str) -> Option<PageReservation> {
        let reservation = self.reservations.remove(request_id)?;
        self.budget.page_reserved_bytes = self
            .budget
            .page_reserved_bytes
            .saturating_sub(reservation.bytes);
        Some(reservation)
    }

    pub fn pending_merge_bytes(&self) -> usize {
        self.budget.pending_merge_bytes
    }

    pub fn pending_merge_request_id(&self) -> Option<String> {
        self.pending_merge
            .as_ref()
            .map(|pending| pending.request_id.clone())
    }

    /// 发布完成后结算待合并页账目（S5.4 在 bridge 发布点调用）。
    pub fn settle_pending_merge(&mut self) -> usize {
        let Some(pending) = self.pending_merge.take() else {
            return 0;
        };
        self.budget.pending_merge_bytes = self
            .budget
            .pending_merge_bytes
            .saturating_sub(pending.body_bytes);
        pending.body_bytes
    }

    /// 接纳一页。预算不足时先按最远段制定并应用模型层释放计划，再登记段。
    pub fn admit_page(&mut self, draft: PageDraft) -> AdmissionDecision {
        if !self.is_enabled() {
            return AdmissionDecision::Disabled;
        }
        let degraded_reason = match self.status() {
            super::WindowStatus::Degraded { reason } => Some(reason.clone()),
            super::WindowStatus::Ready => None,
        };
        if let Some(reason) = degraded_reason {
            let reservation_released = self.release_page_reservation(&draft.request_id).is_some();
            self.stats_mut().admission_blocked += 1;
            return AdmissionDecision::Rejected(AdmissionRejection {
                reason: AdmissionRejectionReason::Degraded { reason },
                released: Vec::new(),
                reservation_released,
            });
        }
        let reservation_released = self.release_page_reservation(&draft.request_id).is_some();
        let identity_matches = self
            .identity()
            .is_some_and(|identity| identity == &draft.identity)
            && draft.window_generation == self.generation();
        if !identity_matches {
            self.stats_mut().stale_pages += 1;
            return AdmissionDecision::Rejected(AdmissionRejection {
                reason: AdmissionRejectionReason::StaleIdentity {
                    expected_generation: self.generation(),
                    actual_generation: draft.window_generation,
                },
                released: Vec::new(),
                reservation_released,
            });
        }
        if !reservation_released {
            return AdmissionDecision::Rejected(AdmissionRejection {
                reason: AdmissionRejectionReason::MissingReservation,
                released: Vec::new(),
                reservation_released: false,
            });
        }
        if self.pending_merge.is_some() {
            return AdmissionDecision::Rejected(AdmissionRejection {
                reason: AdmissionRejectionReason::PendingMergeLimit,
                released: Vec::new(),
                reservation_released: true,
            });
        }
        if let Err(error) = self.preflight(&draft) {
            return AdmissionDecision::Rejected(AdmissionRejection {
                reason: AdmissionRejectionReason::InvalidSegment(error),
                released: Vec::new(),
                reservation_released: true,
            });
        }
        let fill_gap = match &draft.placement {
            PagePlacement::FillGap(gap_id) => Some(*gap_id),
            _ => None,
        };
        let Some(placement) = self.resolve_placement(&draft.placement) else {
            return AdmissionDecision::Rejected(AdmissionRejection {
                reason: AdmissionRejectionReason::PlacementUnavailable,
                released: Vec::new(),
                reservation_released: true,
            });
        };
        if draft.segments.is_empty() {
            return AdmissionDecision::Accepted {
                segments: Vec::new(),
                released: Vec::new(),
                body_bytes: 0,
                pending_merge_bytes: 0,
            };
        }
        let incoming_body: usize = draft
            .segments
            .iter()
            .map(|segment| segment.body_bytes)
            .sum();
        if incoming_body > self.budget.limits.body_bytes {
            self.stats_mut().admission_blocked += 1;
            return AdmissionDecision::Rejected(AdmissionRejection {
                reason: AdmissionRejectionReason::PageExceedsBudget {
                    incoming: incoming_body,
                    limit: self.budget.limits.body_bytes,
                },
                released: Vec::new(),
                reservation_released: true,
            });
        }
        let mut released = Vec::new();
        if !self.budget.has_body_capacity_for(incoming_body) {
            let plan = self.plan_eviction(incoming_body);
            if plan.blocked.is_some() || plan.freed_body_bytes < plan.needed_body_bytes {
                self.stats_mut().evict_blocked_pinned += 1;
                self.stats_mut().admission_blocked += 1;
                let blocked = plan.blocked.unwrap_or_else(|| EvictionBlocked {
                    needed_body_bytes: plan.needed_body_bytes,
                    freed_body_bytes: plan.freed_body_bytes,
                    pinned: self.pinned_report(),
                });
                return AdmissionDecision::Rejected(AdmissionRejection {
                    reason: AdmissionRejectionReason::EvictionBlocked(blocked),
                    released: Vec::new(),
                    reservation_released: true,
                });
            }
            for release in &plan.plans {
                if let Some(applied) = self.apply_release_plan(release) {
                    released.push(applied.plan);
                }
            }
            if !self.budget.has_body_capacity_for(incoming_body) {
                self.stats_mut().evict_blocked_pinned += 1;
                self.stats_mut().admission_blocked += 1;
                return AdmissionDecision::Rejected(AdmissionRejection {
                    reason: AdmissionRejectionReason::EvictionBlocked(EvictionBlocked {
                        needed_body_bytes: plan.needed_body_bytes,
                        freed_body_bytes: plan.freed_body_bytes,
                        pinned: self.pinned_report(),
                    }),
                    released,
                    reservation_released: true,
                });
            }
        }
        let mut segments = Vec::with_capacity(draft.segments.len());
        let mut next_placement = placement;
        for segment in draft.segments {
            match self.register_segment(segment, next_placement) {
                Ok(key) => {
                    next_placement = SegmentPlacement::AfterSegment(key);
                    segments.push(key);
                }
                Err(error) => {
                    self.stats_mut().admission_blocked += 1;
                    return AdmissionDecision::Rejected(AdmissionRejection {
                        reason: AdmissionRejectionReason::InvalidSegment(error),
                        released,
                        reservation_released: true,
                    });
                }
            }
        }
        if let Some(gap_id) = fill_gap {
            // 段已落到缺口位置；缺口不再需要占位。只移除被本次请求补齐的那个缺口。
            if let Some(position) = self
                .layout
                .iter()
                .position(|region| *region == RegionKey::Gap(gap_id))
            {
                self.layout.remove(position);
            }
            self.gaps.remove(&gap_id);
            self.rebuild_slots();
        }
        self.pending_merge = Some(PendingMerge {
            request_id: draft.request_id,
            body_bytes: incoming_body,
        });
        self.budget.pending_merge_bytes = self
            .budget
            .pending_merge_bytes
            .saturating_add(incoming_body);
        AdmissionDecision::Accepted {
            segments,
            released,
            body_bytes: incoming_body,
            pending_merge_bytes: incoming_body,
        }
    }

    fn preflight(&self, draft: &PageDraft) -> Result<(), WindowModelError> {
        let mut entries = HashSet::new();
        let mut sub_items = HashSet::new();
        for segment in &draft.segments {
            if segment.entries.is_empty() {
                return Err(WindowModelError::EmptySegment);
            }
            if segment
                .entries
                .iter()
                .any(|entry| entry.key.source != segment.source)
            {
                return Err(WindowModelError::MixedSource);
            }
            for entry in &segment.entries {
                if self.index.contains_entry(&entry.key) || !entries.insert(entry.key.clone()) {
                    return Err(WindowModelError::DuplicateEntry(entry.key.clone()));
                }
                for sub_item in &entry.sub_items {
                    if self.index.contains_sub_item(sub_item) || !sub_items.insert(sub_item.clone())
                    {
                        return Err(WindowModelError::DuplicateSubItem(sub_item.clone()));
                    }
                }
            }
        }
        Ok(())
    }

    fn resolve_placement(&self, placement: &PagePlacement) -> Option<SegmentPlacement> {
        match placement {
            PagePlacement::Tail => Some(SegmentPlacement::Tail),
            PagePlacement::FillGap(gap_id) => self.resolve_gap_placement(*gap_id),
            PagePlacement::BeforeSegment(key) => self
                .segments()
                .contains_key(key)
                .then_some(SegmentPlacement::BeforeSegment(*key)),
            PagePlacement::BeforeEntry(entry) => {
                let locator = self.index().locate_entry(entry)?;
                Some(SegmentPlacement::BeforeSegment(locator.segment))
            }
        }
    }

    /// 缺口原位的段插入位置：优先插到缺口后的首个驻留段之前；缺口在尾部时
    /// 插到前一个驻留段之后；窗口为空时即 Tail。
    fn resolve_gap_placement(&self, gap_id: GapId) -> Option<SegmentPlacement> {
        let position = self
            .layout
            .iter()
            .position(|region| *region == RegionKey::Gap(gap_id))?;
        if let Some(next) = self
            .layout
            .iter()
            .skip(position + 1)
            .find_map(|region| match region {
                RegionKey::Resident(key) => Some(*key),
                RegionKey::Gap(_) => None,
            })
        {
            return Some(SegmentPlacement::BeforeSegment(next));
        }
        if let Some(previous) =
            self.layout
                .iter()
                .take(position)
                .rev()
                .find_map(|region| match region {
                    RegionKey::Resident(key) => Some(*key),
                    RegionKey::Gap(_) => None,
                })
        {
            return Some(SegmentPlacement::AfterSegment(previous));
        }
        Some(SegmentPlacement::Tail)
    }

    /// 计算淘汰计划，不修改窗口。`needed_body_bytes` 为释放到水位所需的字节。
    pub fn plan_eviction(&self, incoming_body_bytes: usize) -> EvictionPlan {
        if !self.is_enabled()
            || self.is_degraded()
            || self.budget.has_body_capacity_for(incoming_body_bytes)
        {
            return EvictionPlan::default();
        }
        let current = self.budget.resident_body_bytes;
        let target = self.budget.release_target_bytes(incoming_body_bytes);
        let needed = current
            .saturating_add(incoming_body_bytes)
            .saturating_sub(target);
        if needed == 0 {
            return EvictionPlan::default();
        }
        let anchor_region = match self.relocate_anchor() {
            super::AnchorRelocation::Resident { segment, .. } => {
                self.region_index_of_segment(segment)
            }
            super::AnchorRelocation::Gap { gap_id, .. } => self
                .layout()
                .iter()
                .position(|region| *region == RegionKey::Gap(gap_id)),
            super::AnchorRelocation::Unavailable => None,
        };
        let mut candidates: Vec<(SegmentKey, usize, u64, u64)> = self
            .segments()
            .iter()
            .filter(|(_, segment)| segment.durable && segment.pins.is_empty())
            .filter_map(|(key, segment)| {
                let region = self.region_index_of_segment(*key)?;
                let distance = anchor_region
                    .map(|anchor| anchor.abs_diff(region))
                    .unwrap_or(0);
                Some((*key, distance, segment.last_access, segment.created_seq))
            })
            .collect();
        candidates.sort_by(|left, right| {
            right
                .1
                .cmp(&left.1)
                .then(left.2.cmp(&right.2))
                .then(left.3.cmp(&right.3))
        });
        let mut plans = Vec::new();
        let mut freed_body = 0usize;
        let mut freed_reserved = 0usize;
        for (key, _, _, _) in candidates {
            if freed_body >= needed {
                break;
            }
            let Some(segment) = self.segment(key) else {
                continue;
            };
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
            freed_body = freed_body.saturating_add(segment.body_bytes);
            freed_reserved = freed_reserved.saturating_add(segment.reserved_bytes);
        }
        let blocked = (freed_body < needed).then(|| EvictionBlocked {
            needed_body_bytes: needed,
            freed_body_bytes: freed_body,
            pinned: self.pinned_report(),
        });
        EvictionPlan {
            plans,
            needed_body_bytes: needed,
            freed_body_bytes: freed_body,
            freed_reserved_bytes: freed_reserved,
            blocked,
        }
    }

    /// 应用一个释放计划：模型层摘除段、建立/合并 gap、登记 pending release。
    /// 不删除 `committed`/折叠投影/渲染缓存中的对象（S6.2 接入点）。
    pub fn apply_eviction_plan(&mut self, plan: &EvictionPlan) -> ReleaseSummary {
        let mut summary = ReleaseSummary::default();
        for release in &plan.plans {
            if let Some(applied) = self.apply_release_plan(release) {
                summary.released_segments += 1;
                summary.released_entries += applied.released_entries;
                summary.body_bytes = summary.body_bytes.saturating_add(applied.body_bytes);
                summary.reserved_bytes = summary
                    .reserved_bytes
                    .saturating_add(applied.reserved_bytes);
                summary.plans.push(applied.plan);
            }
        }
        summary
    }

    pub(crate) fn apply_release_plan(&mut self, plan: &ReleasePlan) -> Option<AppliedRelease> {
        let candidate = self.segment(plan.segment)?;
        if !candidate.durable || !candidate.pins.is_empty() {
            // 受 pin 保护或未持久化的段不能被低层释放入口绕过。
            return None;
        }
        // 计划里的槽位是决策时快照；同一轮多次释放会移动后面的段，应用时重新取一次。
        let mut applied_plan = plan.clone();
        applied_plan.slot_range = self.index().slot_range(plan.segment);
        let (segment, position) = self.detach_segment(plan.segment)?;
        let gap_id = self.next_gap_id();
        let placeholder = if plan.before.is_none() {
            GapPlaceholder::OlderHistory
        } else {
            GapPlaceholder::MissingMiddle
        };
        let gap = HistoryGap::new(
            gap_id,
            plan.before.clone(),
            plan.after.clone(),
            plan.before_cursor
                .clone()
                .or_else(|| segment.before_cursor.clone()),
            plan.after_cursor
                .clone()
                .or_else(|| segment.after_cursor.clone()),
            segment.source.clone(),
            placeholder,
        );
        self.layout.insert(position, super::RegionKey::Gap(gap_id));
        self.gaps.insert(gap_id, gap);
        merge_adjacent_gaps(&mut self.layout, &mut self.gaps);
        self.rebuild_slots();
        let evicted_bytes = self
            .stats()
            .evicted_bytes
            .saturating_add(segment.body_bytes as u64);
        let stats = self.stats_mut();
        stats.evicted_bytes = evicted_bytes;
        stats.evicted_segments += 1;
        stats.evicted_entries += segment.entries.len() as u64;
        self.record_release_plan(applied_plan.clone());
        Some(AppliedRelease {
            plan: applied_plan,
            released_entries: segment.entries.len(),
            body_bytes: segment.body_bytes,
            reserved_bytes: segment.reserved_bytes,
        })
    }

    fn pinned_report(&self) -> PinnedReport {
        let mut reasons = Vec::new();
        let mut pinned_segments = 0usize;
        let mut pinned_bytes = 0usize;
        for segment in self.segments().values() {
            if segment.pins.is_empty() {
                continue;
            }
            pinned_segments += 1;
            pinned_bytes = pinned_bytes.saturating_add(segment.body_bytes);
            for reason in segment.pins.reasons() {
                if !reasons.contains(&reason) {
                    reasons.push(reason);
                }
            }
        }
        reasons.sort();
        PinnedReport {
            reasons,
            pinned_segments,
            pinned_bytes,
        }
    }
}

pub(crate) struct AppliedRelease {
    pub plan: ReleasePlan,
    pub released_entries: usize,
    pub body_bytes: usize,
    pub reserved_bytes: usize,
}
