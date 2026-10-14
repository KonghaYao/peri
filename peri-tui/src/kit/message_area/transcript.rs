use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::grid::{GridSpec, ZoneTransition};
use super::selection::{SlotIndex, SlotLines};
use super::vm_cache::VmCacheSlot;
use crate::kit::atoms::{TranscriptPublication, ViewModelsSnapshot};
use crate::kit::entry_render_cache::{EntryInvalidation, EntryRenderContext};
use crate::kit::tui_render_unit::TuiRenderUnit;

const HEAVY_CACHE_BUDGET: usize = 16 * 1024 * 1024;

/// entry 在时间轴上的起止列区间（§7）：文本区 = 基准列，过程区 = 右移 `indent`。
///
/// 相邻判定只看「本 entry 起点 zone」与「前一个产出行 entry 的终点 zone」——
/// 文本区进入过程区画进入台阶、过程区回到文本区画退出台阶，同区接续不画符。
#[derive(Clone, Copy, PartialEq, Eq)]
enum EntryZone {
    Text,
    Process,
}

/// entry 起点 zone（§7）：AssistantBubble 有 reasoning 时 reasoning 先行 → 过程区，
/// 否则与正文同为文本区；UserBubble 与 turn 边界 Divider 属文本区；其余变体
/// （工具卡片 / 子 Agent 组 / 折叠组 / 系统 / 交互 / Todo）整体属过程区。
fn start_zone(vm: &TuiRenderUnit) -> EntryZone {
    match vm {
        TuiRenderUnit::TuiAssistantBubble(data) if data.reasoning.is_some() => EntryZone::Process,
        TuiRenderUnit::TuiAssistantBubble(_)
        | TuiRenderUnit::TuiUserBubble(_)
        | TuiRenderUnit::TuiDivider(_) => EntryZone::Text,
        _ => EntryZone::Process,
    }
}

/// entry 终点 zone（§7）：AssistantBubble 正文非空 → 文本区，否则等于起点
/// （纯 reasoning 块整块留在过程区）；Divider 属文本区。
fn end_zone(vm: &TuiRenderUnit) -> EntryZone {
    match vm {
        TuiRenderUnit::TuiAssistantBubble(data) if !data.text.is_empty() => EntryZone::Text,
        TuiRenderUnit::TuiAssistantBubble(_) => start_zone(vm),
        TuiRenderUnit::TuiUserBubble(_) | TuiRenderUnit::TuiDivider(_) => EntryZone::Text,
        _ => EntryZone::Process,
    }
}

/// entry 是否产出渲染行——0 行 entry（空 bubble / 空 user prompt）不参与相邻判定（§7）。
/// 判定条件与 `render::vm_to_lines_cached_with_layout` 的 0 行早返回同源，两处须同步。
fn produces_lines(vm: &TuiRenderUnit) -> bool {
    match vm {
        TuiRenderUnit::TuiAssistantBubble(data) => {
            !(data.text.is_empty() && data.reasoning.is_none())
        }
        TuiRenderUnit::TuiUserBubble(data) => {
            data.reminder.is_some() || !data.text.is_empty() || !data.attachments.is_empty()
        }
        _ => true,
    }
}

/// 计算 slot 的过程区转场形态（§6）：由「本 entry 起点 zone」与「前一个产出行
/// entry 终点 zone」的差异决定——文本区 → 过程区 = [`ZoneTransition::EnterProcess`]
/// （入口画 ` ╰─╮`），过程区 → 文本区 = [`ZoneTransition::ExitProcess`]（入口画
/// ` ╭─╯`），同区接续或时间轴首个 entry = [`ZoneTransition::None`]。
///
/// [Why] 转场由「转场后的首个 entry」承担：判定只依赖前驱（后向依赖），追加新
/// entry 只需渲染自身，不回溯翻转任何已渲染 slot；`AssistantBubble` 内部
/// reasoning → 正文的转场由该 entry 渲染时自画（entry 内部事务，无跨 slot 依赖）。
/// 区间首 slot 也读 `items[slot - 1]`——相邻关系不因重建区间而变。
///
/// transcript_index 的按需物化同用此判定，保证按需重渲染与常驻渲染同列。
pub(super) fn zone_transition(items: &im::Vector<TuiRenderUnit>, slot: usize) -> ZoneTransition {
    let Some(vm) = items.get(slot) else {
        return ZoneTransition::None;
    };
    let start = start_zone(vm);
    for index in (0..slot).rev() {
        let previous = &items[index];
        if !produces_lines(previous) {
            continue;
        }
        return match (end_zone(previous), start) {
            (EntryZone::Text, EntryZone::Process) => ZoneTransition::EnterProcess,
            (EntryZone::Process, EntryZone::Text) => ZoneTransition::ExitProcess,
            _ => ZoneTransition::None,
        };
    }
    ZoneTransition::None
}

#[derive(Default)]
pub(super) struct Transcript {
    index: Arc<SlotIndex>,
    generation: Option<u64>,
    reset: u64,
    grid: Option<GridSpec>,
    width: u16,
    context: Option<EntryRenderContext>,
    animations: BTreeMap<usize, u64>,
    anchors: BTreeSet<usize>,
    recency: BTreeSet<(u64, usize)>,
    stamps: Vec<u64>,
    bytes: Vec<usize>,
    retained_bytes: usize,
    clock: u64,
    frame: Option<u64>,
    pub(super) scanned_slots: usize,
    pub(super) updated_slots: usize,
    pub(super) evictions: usize,
    pub(super) invalidation: &'static str,
}

impl Transcript {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn prepare(
        &mut self,
        snapshot: &ViewModelsSnapshot,
        publication: &TranscriptPublication,
        caches: &mut Vec<VmCacheSlot>,
        grid: GridSpec,
        width: u16,
        context: EntryRenderContext,
        frame: u64,
        reset: u64,
        scroll_y: usize,
        height: usize,
    ) -> Arc<SlotIndex> {
        self.scanned_slots = 0;
        self.updated_slots = 0;
        self.evictions = 0;
        let global = self.reset != reset
            || self.grid != Some(grid)
            || self.width != width
            || self.context.as_ref().is_none_or(|previous| {
                previous.language != context.language
                    || !Arc::ptr_eq(&previous.theme, &context.theme)
            });
        let changed = self.generation != Some(snapshot.generation) || self.reset != reset;
        self.invalidation = if self.reset != reset {
            "reset"
        } else if self.generation.is_none() {
            "initial"
        } else if self.grid != Some(grid) {
            "grid"
        } else if self.width != width {
            "width"
        } else if global {
            "theme-or-language"
        } else if changed
            && publication.generation == snapshot.generation
            && self.generation == Some(publication.previous_generation)
        {
            "adjacent-publication"
        } else if changed {
            "generation-gap-or-unmatched-publication"
        } else {
            "none"
        };
        let changed_from = if global || self.generation.is_none() {
            0
        } else if changed
            && publication.generation == snapshot.generation
            && self.generation == Some(publication.previous_generation)
        {
            publication.changed_from.min(snapshot.items.len())
        } else if changed {
            0
        } else {
            snapshot.items.len()
        };
        if global || changed {
            let length = snapshot.items.len();
            let previous_length = caches.len();
            for slot in length..caches.len() {
                self.remove_accounting(slot);
            }
            caches.resize_with(length, VmCacheSlot::default);
            self.stamps.resize(length, 0);
            self.bytes.resize(length, 0);
            self.animations.split_off(&changed_from);
            self.anchors.split_off(&changed_from);
            Arc::make_mut(&mut self.index).truncate(length);
            Arc::make_mut(&mut self.index).set_source(
                snapshot.items.clone(),
                grid,
                context.clone(),
                frame,
            );
            let pinned = self.index.visible_slots(
                scroll_y.min(self.index.total_visual().saturating_sub(height)),
                height,
            );
            for slot in changed_from..length {
                self.scanned_slots += 1;
                let vm = &snapshot.items[slot];
                let period = vm.animation_period_frames();
                if period != 0 {
                    self.animations.insert(slot, period);
                }
                if matches!(vm, TuiRenderUnit::TuiAskUserBlock(block) if block.pending) {
                    self.anchors.insert(slot);
                }
                // 转场翻转（相邻 entry 的 zone 变化）必须重渲染该 slot（不影响
                // 其他 slot）；已淘汰的 slot 没有行可改，等回到视口时按当时的
                // 转场形态渲染。
                let transition = zone_transition(&snapshot.items, slot);
                let transition_flipped = caches[slot].lines.is_some()
                    && caches[slot].entry.transition() != Some(transition);
                if global
                    || slot >= previous_length
                    || !caches[slot].matches_content(vm)
                    || transition_flipped
                {
                    self.ensure_slot(slot, vm, caches, grid, width, &context, frame, transition);
                    self.enforce_budget(caches, &pinned);
                }
            }
        }
        let scroll_y = scroll_y.min(self.index.total_visual().saturating_sub(height));
        let viewport = self.index.visible_slots(scroll_y, height);
        let animated: Vec<_> = self
            .animations
            .range(viewport.clone())
            .filter(|(_, period)| {
                self.frame
                    .is_none_or(|previous| previous / **period != frame / **period)
            })
            .map(|(&slot, _)| slot)
            .collect();
        for slot in animated {
            let transition = zone_transition(&snapshot.items, slot);
            self.ensure_slot(
                slot,
                &snapshot.items[slot],
                caches,
                grid,
                width,
                &context,
                frame,
                transition,
            );
        }
        for slot in viewport.clone() {
            if caches[slot].lines.is_none() {
                let transition = zone_transition(&snapshot.items, slot);
                self.ensure_slot(
                    slot,
                    &snapshot.items[slot],
                    caches,
                    grid,
                    width,
                    &context,
                    frame,
                    transition,
                );
            }
            self.touch(slot);
        }
        self.enforce_budget(caches, &viewport);
        self.generation = Some(snapshot.generation);
        self.reset = reset;
        self.grid = Some(grid);
        self.width = width;
        self.context = Some(context);
        self.frame = Some(frame);
        Arc::clone(&self.index)
    }

    #[allow(clippy::too_many_arguments)]
    fn ensure_slot(
        &mut self,
        slot: usize,
        vm: &TuiRenderUnit,
        caches: &mut [VmCacheSlot],
        grid: GridSpec,
        width: u16,
        context: &EntryRenderContext,
        frame: u64,
        transition: ZoneTransition,
    ) {
        let cache = &mut caches[slot];
        drop(cache.lines.take());
        let invalidation = cache
            .entry
            .ensure(vm, &grid, context.clone(), frame, transition);
        let layout_changed = matches!(
            invalidation,
            EntryInvalidation::Content | EntryInvalidation::Layout
        ) || cache.wrap_map.is_empty()
            || cache.wrap_width != width;
        if layout_changed {
            let (height, map) = cache.entry.build_wrap_map(width);
            cache.visual_rows = height;
            cache.wrap_map = Arc::new(map);
            cache.wrap_width = width;
        }
        cache.content_hash = vm.content_hash();
        cache.variant = Some(std::mem::discriminant(vm));
        cache.copy_button = cache.entry.copy_button().cloned();
        cache.interaction = cache.entry.interaction().cloned();
        cache.image_lines = cache.entry.image_lines().to_vec();
        let lines = Arc::new(match cache.entry.markdown_lines().stable_overlay() {
            Some((start, stable)) => {
                SlotLines::composite(Arc::clone(cache.entry.lines()), start, stable)
            }
            None => SlotLines::single(Arc::clone(cache.entry.lines())),
        });
        if !layout_changed {
            Arc::make_mut(&mut self.index).replace_lines(slot, Arc::clone(&lines));
        } else {
            Arc::make_mut(&mut self.index).set_slot(
                slot,
                Arc::clone(&lines),
                &cache.wrap_map,
                false,
            );
        }
        cache.lines = Some(lines);
        self.updated_slots += 1;
        self.retained_bytes = self.retained_bytes.saturating_sub(self.bytes[slot]);
        self.bytes[slot] = cache.retained_bytes();
        self.retained_bytes += self.bytes[slot];
        self.touch(slot);
    }

    fn touch(&mut self, slot: usize) {
        self.recency.remove(&(self.stamps[slot], slot));
        self.clock = self.clock.wrapping_add(1);
        self.stamps[slot] = self.clock;
        if self.bytes[slot] != 0 {
            self.recency.insert((self.clock, slot));
        }
    }

    pub(super) fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    fn remove_accounting(&mut self, slot: usize) {
        self.recency.remove(&(self.stamps[slot], slot));
        self.retained_bytes = self.retained_bytes.saturating_sub(self.bytes[slot]);
        self.bytes[slot] = 0;
    }

    fn enforce_budget(&mut self, caches: &mut [VmCacheSlot], pinned: &std::ops::Range<usize>) {
        self.evict_to_budget(caches, pinned, HEAVY_CACHE_BUDGET);
    }

    fn evict_to_budget(
        &mut self,
        caches: &mut [VmCacheSlot],
        pinned: &std::ops::Range<usize>,
        budget: usize,
    ) {
        let mut visible = Vec::new();
        while self.retained_bytes > budget {
            let Some((stamp, slot)) = self.recency.pop_first() else {
                break;
            };
            if pinned.contains(&slot) {
                visible.push((stamp, slot));
                continue;
            }
            caches[slot].evict();
            self.retained_bytes = self.retained_bytes.saturating_sub(self.bytes[slot]);
            self.bytes[slot] = 0;
            self.evictions += 1;
        }
        self.recency.extend(visible);
    }

    pub(super) fn anchor(&self) -> Option<usize> {
        self.anchors.first().copied()
    }

    pub(super) fn warm_visible(
        &mut self,
        snapshot: &ViewModelsSnapshot,
        caches: &mut [VmCacheSlot],
        scroll_y: usize,
        height: usize,
    ) -> Arc<SlotIndex> {
        let viewport = self.index.visible_slots(scroll_y, height);
        let context = self.context.clone().unwrap();
        for slot in viewport.clone() {
            if caches[slot].lines.is_none() {
                let transition = zone_transition(&snapshot.items, slot);
                self.ensure_slot(
                    slot,
                    &snapshot.items[slot],
                    caches,
                    self.grid.unwrap(),
                    self.width,
                    &context,
                    self.frame.unwrap(),
                    transition,
                );
            }
            self.touch(slot);
        }
        self.enforce_budget(caches, &viewport);
        Arc::clone(&self.index)
    }
}

#[cfg(test)]
#[path = "transcript_test.rs"]
mod tests;
