use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use super::grid::{GridSpec, TextLineSource, ZoneTransition};
use super::selection::{SlotIndex, SlotLines};
use super::viewport_anchor::{PrependRelocation, ReadingAnchor, ViewportAnchor};
use super::vm_cache::VmCacheSlot;

use crate::kit::entry_render_cache::{EntryInvalidation, EntryRenderContext};
use crate::kit::history_window::WindowUnitToken;
use crate::kit::tui_render_unit::{HistoryOverlap, TuiRenderUnit, history_overlap};

const HEAVY_CACHE_BUDGET: usize = 8 * 1024 * 1024;
const HEAVY_CACHE_TARGET: usize = 6 * 1024 * 1024;

/// entry 在时间轴上的起止列区间（§7）：文本区 = 基准列，过程区 = 右移 `indent`。
///
/// 相邻判定只看「本 entry 起点 zone」与「前一个产出行 entry 的终点 zone」——
/// 文本区进入过程区画进入台阶、过程区回到文本区画退出台阶，同区接续不画符。
#[derive(Clone, Copy, PartialEq, Eq)]
enum EntryZone {
    Text,
    Process,
}

/// entry 起点 zone（§7）：AssistantBubble 有 reasoning 时 reasoning 先行 → 过程区；
/// 短回复（`text` 字符数 < `BRIEF_REPLY_MAX_CHARS`，见 `TuiAssistantBubble::is_brief`）
/// 整体并入过程区（缩进形态）；其余 AssistantBubble 与 UserBubble、turn 边界
/// Divider 属文本区；其余变体（工具卡片 / 子 Agent 组 / 系统 / 交互 / Todo）
/// 整体属过程区。human（user 气泡）不参与短回复缩进。
fn start_zone(vm: &TuiRenderUnit) -> EntryZone {
    match vm {
        TuiRenderUnit::TuiAssistantBubble(data) if data.reasoning.is_some() || data.is_brief() => {
            EntryZone::Process
        }
        TuiRenderUnit::TuiAssistantBubble(_)
        | TuiRenderUnit::TuiUserBubble(_)
        | TuiRenderUnit::TuiDivider(_) => EntryZone::Text,
        _ => EntryZone::Process,
    }
}

/// entry 终点 zone（§7）：AssistantBubble 正文非空且非短回复 → 文本区；否则等于
/// 起点（纯 reasoning 块与短回复整块留在过程区）；Divider 属文本区。
fn end_zone(vm: &TuiRenderUnit) -> EntryZone {
    match vm {
        TuiRenderUnit::TuiAssistantBubble(data) if !data.text.is_empty() && !data.is_brief() => {
            EntryZone::Text
        }
        TuiRenderUnit::TuiAssistantBubble(_) => start_zone(vm),
        TuiRenderUnit::TuiUserBubble(_) | TuiRenderUnit::TuiDivider(_) => EntryZone::Text,
        _ => EntryZone::Process,
    }
}

/// entry 是否产出渲染行——0 行 entry（空 bubble / 空 user prompt）不参与相邻判定（§7）。
/// 判定条件与 `render::vm_to_lines_with_options` 的 0 行早返回同源，两处须同步。
#[must_use]
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

/// 台阶线色来源：按 entry 类型映射文本区左侧线条色——user 气泡用
/// `text.secondary`、assistant 正文用 `accent`、Divider 用 `text.secondary`（与
/// `── label ──` 同色）；主题值在渲染层解析（`render/helpers.rs::text_line_color`）。
#[must_use]
fn text_line_source(vm: &TuiRenderUnit) -> TextLineSource {
    match vm {
        TuiRenderUnit::TuiUserBubble(_) => TextLineSource::User,
        TuiRenderUnit::TuiDivider(_) => TextLineSource::Divider,
        // 文本区 entry 只剩 AssistantBubble（含正文）→ 正文竖线色。
        _ => TextLineSource::Assistant,
    }
}

/// 计算 slot 的过程区转场形态（§6）：由「本 entry 起点 zone」与「前一个产出行
/// entry 终点 zone」的差异决定——文本区 → 过程区 = [`ZoneTransition::EnterProcess`]
/// （入口画 ` ╰─╮`），过程区 → 文本区 = [`ZoneTransition::ExitProcess`]（入口画
/// ` ╭─╯`），同区接续或时间轴首个 entry = [`ZoneTransition::None`]。台阶线色
/// 随所接文本的左侧线条（[`TextLineSource`]）：进入台阶接上方文本 entry、
/// 退出台阶接下方（承担台阶的当前）文本 entry，转场处颜色不断链。
///
/// [Why] 转场由「转场后的首个 entry」承担：判定只依赖前驱（后向依赖），追加新
/// entry 只需渲染自身，不回溯翻转任何已渲染 slot；`AssistantBubble` 内部
/// reasoning → 正文的转场由该 entry 渲染时自画（entry 内部事务，无跨 slot 依赖）。
/// 区间首 slot 也读 `items[slot - 1]`——相邻关系不因重建区间而变。
///
/// transcript_index 的按需物化同用此判定，保证按需重渲染与常驻渲染同列。
pub(crate) fn zone_transition(items: &im::Vector<TuiRenderUnit>, slot: usize) -> ZoneTransition {
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
            (EntryZone::Text, EntryZone::Process) => {
                ZoneTransition::EnterProcess(text_line_source(previous))
            }
            (EntryZone::Process, EntryZone::Text) => {
                ZoneTransition::ExitProcess(text_line_source(vm))
            }
            _ => ZoneTransition::None,
        };
    }
    ZoneTransition::None
}

/// 围栏逻辑行范围 → slot 内视觉行范围；空范围或越界返回 None。
fn fence_visual_range(
    wrap_map: &[crate::kit::message_area::selection::WrappedLineInfo],
    logical: &std::ops::Range<usize>,
) -> Option<std::ops::Range<usize>> {
    let last = logical.end.checked_sub(1)?;
    let first = wrap_map.get(logical.start)?;
    let last = wrap_map.get(last)?;
    (last.visual_end > first.visual_start).then_some(first.visual_start..last.visual_end)
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
    anchor_counts: HashMap<WindowUnitToken, usize>,
    recency: BTreeSet<(u64, usize)>,
    stamps: Vec<u64>,
    bytes: Vec<usize>,
    retained_bytes: usize,
    /// 当前缓存消息引用的图几何字节（同源码共享只计一次）。
    mermaid_bytes: usize,
    clock: u64,
    frame: Option<u64>,
    genui_revision: u64,
    /// [REQ-C01-3 / A20] 最近一次发布的重定位结果（仅当该次发布与一次更早历史并入配对）。
    /// 渲染 body 取走（一次性）后交给 `run_auto_follow` 的并入分支消费；
    /// 语义与身份口径见 [`super::viewport_anchor`]。
    prepend: Option<PrependRelocation>,
    /// 同条消息内高度变化（图完成/缩放/异步结果）时的阅读位置重定位。
    reflow: Option<PrependRelocation>,
    /// 最近一次已显示快照携带的桥接并入代数，点击折叠沿用上一桥接发布的代数。
    merge_epoch_seen: u64,
    structural_epoch_seen: u64,
    pub(super) scanned_slots: usize,
    pub(super) updated_slots: usize,
    pub(super) evictions: usize,
    #[cfg(test)]
    pub(super) genui_budget_checks: usize,
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
        let merge_epoch = publication.prepend_epoch;
        self.scanned_slots = 0;
        self.updated_slots = 0;
        self.evictions = 0;
        #[cfg(test)]
        {
            self.genui_budget_checks = 0;
        }
        let published = self
            .generation
            .is_some_and(|generation| generation != snapshot.generation);
        let structural_epoch = publication.structural_epoch;
        let structural_changed = structural_epoch != self.structural_epoch_seen;
        let merging = published && self.reset == reset && merge_epoch != self.merge_epoch_seen;
        let displayed_epoch = if published || self.generation.is_none() {
            merge_epoch
        } else {
            self.merge_epoch_seen
        };
        let mut measurements = super::history_layout::begin_frame(
            snapshot.generation,
            displayed_epoch,
            grid,
            width,
            &context,
        );
        // Capture before a segment release drops the previous index.
        let prepend_anchor = merging
            .then(|| ViewportAnchor::capture(&self.index, scroll_y))
            .flatten();
        let layout_changed = self.reset != reset
            || self.grid != Some(grid)
            || self.width != width
            || self.context.as_ref().is_none_or(|previous| {
                previous.language != context.language
                    || previous.occurrence != context.occurrence
                    || previous.surface != context.surface
                    || !Arc::ptr_eq(&previous.theme, &context.theme)
            });
        let run = if merging && !layout_changed {
            self.index
                .canonical_items()
                .map(|old| history_overlap(old, &snapshot.items))
                .filter(|run| run.len > 0)
        } else {
            None
        };
        let release_all = structural_changed && run.is_none();
        if release_all {
            for cache in caches.iter_mut() {
                cache.evict();
                cache.anchor_token = None;
            }
            self.anchor_counts.clear();
            self.bytes = vec![0; caches.len()];
            self.stamps = vec![0; caches.len()];
            self.recency.clear();
            self.retained_bytes = 0;
            self.index = Arc::new(SlotIndex::default());
            self.generation = None;
        }
        self.structural_epoch_seen = structural_epoch;
        let global = release_all || layout_changed;
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
        } else if run.is_some() {
            "history-page"
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
        let reading_anchor = if published {
            None
        } else {
            ReadingAnchor::capture(&self.index, scroll_y, caches)
        };
        if global || changed {
            let length = snapshot.items.len();
            let previous_length = caches.len();
            let old_visible = self.index.visible_slots(
                scroll_y.min(self.index.total_visual().saturating_sub(height)),
                height,
            );
            let (prefix, suffix, pinned) = if let Some(run) = run {
                self.move_history(caches, run, length);
                // The first emitting entry depends on its predecessor, possibly across empty
                // bubbles. Recheck that seam before skipping the rest of the measured run.
                let end = run.new_start + run.len;
                let seam = snapshot
                    .items
                    .iter()
                    .skip(run.new_start)
                    .take(run.len)
                    .position(produces_lines)
                    .map_or(end, |n| run.new_start + n + 1);
                let moved =
                    |old: usize| run.new_start + old.saturating_sub(run.old_start).min(run.len);
                (
                    0..seam,
                    end..length,
                    moved(old_visible.start)..moved(old_visible.end),
                )
            } else {
                for slot in length..caches.len() {
                    self.forget_token(caches[slot].anchor_token);
                    self.remove_accounting(slot);
                }
                caches.resize_with(length, VmCacheSlot::default);
                self.stamps.resize(length, 0);
                self.bytes.resize(length, 0);
                self.animations.split_off(&changed_from);
                self.anchors.split_off(&changed_from);
                Arc::make_mut(&mut self.index).truncate(length);
                (changed_from..length, length..length, old_visible)
            };
            Arc::make_mut(&mut self.index).set_source(
                snapshot.items.clone(),
                grid,
                context.clone(),
                frame,
            );
            for slot in prefix.chain(suffix) {
                self.scanned_slots += 1;
                let vm = &snapshot.items[slot];
                let token = super::viewport_anchor::window_identity_token(vm);
                if caches[slot].anchor_token != Some(token) {
                    self.forget_token(caches[slot].anchor_token.replace(token));
                    *self.anchor_counts.entry(token).or_default() += 1;
                }
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
                if run.is_some()
                    && caches[slot].variant.is_none()
                    && let Some(layout) = measurements
                        .as_mut()
                        .and_then(|page| page.take(slot, vm, transition))
                {
                    let cache = &mut caches[slot];
                    cache.content_hash = vm.content_hash();
                    cache.variant = Some(std::mem::discriminant(vm));
                    cache.layout_transition = Some(transition);
                    cache.wrap_width = width;
                    cache.visual_rows = layout.visual_rows();
                    Arc::make_mut(&mut self.index).set_cold(slot, layout);
                    continue;
                }
                let transition_flipped = caches[slot].layout_transition != Some(transition);
                if global
                    || (run.is_none() && slot >= previous_length)
                    || !caches[slot].matches_content(vm)
                    || transition_flipped
                {
                    self.ensure_slot(slot, vm, caches, grid, width, &context, frame, transition);
                    self.enforce_budget(caches, &pinned);
                }
            }
        }
        let genui_revision = crate::kit::genui::view_state::revision();
        if self.genui_revision != genui_revision {
            for slot in 0..caches.len() {
                if caches[slot].entry.genui_state_changed() {
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
                    let visible = self.index.visible_slots(
                        scroll_y.min(self.index.total_visual().saturating_sub(height)),
                        height,
                    );
                    self.enforce_budget(caches, &visible);
                    #[cfg(test)]
                    {
                        self.genui_budget_checks += 1;
                    }
                }
            }
            self.genui_revision = genui_revision;
        }
        // ── [REQ-C01-3 / A20] 发布**后**的重定位：同一身份在新索引里重查视觉起点 ──
        // 只为携带新并入代数的快照重定位。中间的本地折叠可能改变 generation，
        // 其桥接元数据仍属于旧页面。被替换或不唯一的身份不补偿。
        // `from_offset` 取**未夹取**的当帧 scroll_y：它就是旧布局上视口顶行的位置。
        self.prepend = None;
        if published {
            if merge_epoch != self.merge_epoch_seen {
                self.prepend = prepend_anchor.and_then(|anchor| {
                    let target = match run {
                        Some(run) => {
                            anchor.relocate_page(&self.index, run, &self.anchor_counts, caches)
                        }
                        None => anchor.relocate(&self.index),
                    };
                    target.map(|target_offset| PrependRelocation {
                        from_offset: scroll_y,
                        target_offset,
                    })
                });
            }
            self.merge_epoch_seen = merge_epoch;
        }
        let capture_scroll = scroll_y;
        let scroll_y = self
            .prepend
            .map_or(scroll_y, |moved| moved.target_offset)
            .min(self.index.total_visual().saturating_sub(height));
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
            let preview_stale = caches[slot].lines.is_some()
                && caches[slot].entry.has_mermaid() == Some(true)
                && caches[slot].entry.preview_revision()
                    != crate::kit::mermaid::message_revision_for_vm(&snapshot.items[slot]);
            if caches[slot].lines.is_none() || preview_stale {
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
        self.refresh_mermaid_bytes(caches);
        self.enforce_budget(caches, &viewport);
        crate::kit::atoms::RENDER_ACHIEVED_EPOCH.set(
            crate::kit::atoms::RENDER_ACHIEVED_EPOCH
                .get()
                .max(structural_epoch),
        );
        crate::kit::atoms::TUI_RENDER_CACHE_BYTES.set(self.retained_bytes());
        if crate::kit::history_page_consumer::window_path_enabled() {
            let handle = crate::kit::atoms::WINDOW_VIEWPORT.state();
            let follow = handle
                .read()
                .as_ref()
                .is_some_and(|viewport| viewport.follow_bottom);
            if let Some(anchor) = ViewportAnchor::capture(&self.index, scroll_y) {
                crate::kit::history_window::publish_viewport_from_token(
                    anchor.window_token(),
                    anchor.offset_within_unit(),
                    follow,
                );
            } else {
                crate::kit::history_window::publish_viewport_unanchored(follow);
            }
        }
        self.reflow = if self.updated_slots > 0 {
            reading_anchor.and_then(|anchor| {
                anchor
                    .relocate(&self.index, caches)
                    .map(|target_offset| PrependRelocation {
                        from_offset: capture_scroll,
                        target_offset,
                    })
            })
        } else {
            None
        };
        if self.generation.is_none() {
            self.merge_epoch_seen = merge_epoch;
        }
        self.generation = Some(snapshot.generation);
        self.reset = reset;
        self.grid = Some(grid);
        self.width = width;
        self.context = Some(context);
        self.frame = Some(frame);
        Arc::clone(&self.index)
    }

    fn forget_token(&mut self, token: Option<WindowUnitToken>) {
        if let Some(token) = token
            && let std::collections::hash_map::Entry::Occupied(mut count) =
                self.anchor_counts.entry(token)
        {
            *count.get_mut() -= 1;
            if *count.get() == 0 {
                count.remove();
            }
        }
    }

    fn move_history(&mut self, caches: &mut Vec<VmCacheSlot>, run: HistoryOverlap, len: usize) {
        // Move the cached values themselves. Cold slots carry no lines but still have measured
        // leaves in the layout tree. Dropping the two excluded ranges releases their payloads.
        fn move_values<T: Default>(values: &mut Vec<T>, run: HistoryOverlap, len: usize) {
            values.truncate(run.old_start + run.len);
            values.splice(
                ..run.old_start,
                std::iter::repeat_with(T::default).take(run.new_start),
            );
            values.resize_with(len, T::default);
        }
        let old_end = run.old_start + run.len;
        for cache in caches[..run.old_start].iter().chain(&caches[old_end..]) {
            self.forget_token(cache.anchor_token);
        }
        let removed: usize = self.bytes[..run.old_start]
            .iter()
            .chain(&self.bytes[old_end..])
            .sum();
        self.retained_bytes = self.retained_bytes.saturating_sub(removed);
        let moved = |slot: usize| {
            (run.old_start..old_end)
                .contains(&slot)
                .then(|| run.new_start + (slot - run.old_start))
        };
        self.recency = std::mem::take(&mut self.recency)
            .into_iter()
            .filter_map(|(stamp, slot)| moved(slot).map(|slot| (stamp, slot)))
            .collect();
        self.animations = std::mem::take(&mut self.animations)
            .into_iter()
            .filter_map(|(slot, period)| moved(slot).map(|slot| (slot, period)))
            .collect();
        self.anchors = std::mem::take(&mut self.anchors)
            .into_iter()
            .filter_map(moved)
            .collect();
        move_values(caches, run, len);
        move_values(&mut self.stamps, run, len);
        move_values(&mut self.bytes, run, len);
        Arc::make_mut(&mut self.index).move_history(run);
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
        cache.layout_transition = cache.entry.transition();
        cache.variant = Some(std::mem::discriminant(vm));
        cache.copy_button = cache.entry.copy_button().cloned();
        cache.interaction = cache.entry.interaction().cloned();
        cache.image_lines = cache.entry.image_lines().to_vec();
        cache.genui_buttons = cache.entry.genui_buttons().to_vec();
        cache.mermaid_fences = cache.entry.mermaid_fences().to_vec();
        let lines = Arc::new(SlotLines::from_entry(&cache.entry));
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

    #[must_use]
    pub(super) fn retained_bytes(&self) -> usize {
        self.retained_bytes.saturating_add(self.mermaid_bytes)
    }

    fn refresh_mermaid_bytes(&mut self, caches: &[VmCacheSlot]) {
        // 相同源码/列宽的几何只计价一次（与 store 的共享口径一致）。
        let mut seen = std::collections::HashSet::new();
        let mut total = 0usize;
        for cache in caches {
            for span in &cache.mermaid_fences {
                if seen.insert((span.fence.source.clone(), span.width)) {
                    total = total.saturating_add(span.layout_bytes);
                }
            }
        }
        self.mermaid_bytes = total;
    }

    /// 可见消息 → 可见行与围栏范围相交 → 只调度命中的图。预取关闭。
    fn schedule_visible_fences(&self, caches: &[VmCacheSlot], scroll_y: usize, height: usize) {
        if height == 0 {
            return;
        }
        let visible_end = scroll_y.saturating_add(height);
        let mut visible = std::collections::HashSet::new();
        let mut known = std::collections::HashSet::new();
        for cache in caches {
            known.extend(cache.mermaid_fences.iter().map(|span| span.key.clone()));
        }
        for slot in self.index.visible_slots(scroll_y, height) {
            let Some(slot_start) = self.index.slot_visual_start(slot) else {
                continue;
            };
            let Some(cache) = caches.get(slot) else {
                continue;
            };
            if cache.wrap_map.is_empty() {
                continue;
            }
            let local_start = scroll_y.saturating_sub(slot_start);
            let local_end = visible_end.saturating_sub(slot_start);
            for span in &cache.mermaid_fences {
                let Some(range) = fence_visual_range(&cache.wrap_map, &span.logical) else {
                    continue;
                };
                if range.start < local_end && range.end > local_start {
                    visible.insert(span.key.clone());
                    crate::kit::mermaid::observe_span(span);
                }
            }
        }
        crate::kit::mermaid::retract_unseen(&visible, &known);
    }

    fn remove_accounting(&mut self, slot: usize) {
        self.recency.remove(&(self.stamps[slot], slot));
        self.retained_bytes = self.retained_bytes.saturating_sub(self.bytes[slot]);
        self.bytes[slot] = 0;
    }

    fn enforce_budget(&mut self, caches: &mut [VmCacheSlot], pinned: &std::ops::Range<usize>) {
        if self.retained_bytes.saturating_add(self.mermaid_bytes) > HEAVY_CACHE_BUDGET {
            crate::kit::mermaid::evict_geometry_to(HEAVY_CACHE_TARGET / 2);
            self.refresh_mermaid_bytes(caches);
            let vm_target = HEAVY_CACHE_TARGET.saturating_sub(self.mermaid_bytes);
            if self.retained_bytes > vm_target {
                self.evict_to_budget(caches, pinned, vm_target);
            }
        }
    }

    #[cfg(test)]
    pub(super) fn render_cache_budgets() -> (usize, usize) {
        (HEAVY_CACHE_BUDGET, HEAVY_CACHE_TARGET)
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
            self.bytes[slot] = caches[slot].retained_bytes();
            self.retained_bytes = self.retained_bytes.saturating_add(self.bytes[slot]);
            self.evictions += 1;
        }
        self.recency.extend(visible);
    }

    pub(super) fn anchor(&self) -> Option<usize> {
        self.anchors.first().copied()
    }

    /// [REQ-C01-3 / A20] 取走本次发布的重定位结果（**一次性**）。
    ///
    /// 渲染 body 在 `prepare` 之后调用一次并交给 `run_auto_follow` 的并入分支；
    /// 取走后不会在后续帧被重复应用（否则非并入帧会用陈旧目标覆盖用户的滚动）。
    pub(super) fn take_prepend_relocation(&mut self) -> Option<PrependRelocation> {
        self.prepend.take()
    }

    /// 同条消息内高度变化的重定位结果（一次性）。
    pub(super) fn take_reflow_relocation(&mut self) -> Option<PrependRelocation> {
        self.reflow.take()
    }

    pub(super) fn warm_visible(
        &mut self,
        snapshot: &ViewModelsSnapshot,
        caches: &mut [VmCacheSlot],
        scroll_y: usize,
        height: usize,
    ) -> Arc<SlotIndex> {
        let viewport = self.index.visible_slots(scroll_y, height);
        let context = self
            .context
            .clone()
            .expect("transcript preparation installs context before warming visible slots");
        for slot in viewport.clone() {
            if caches[slot].lines.is_none() {
                let transition = zone_transition(&snapshot.items, slot);
                self.ensure_slot(
                    slot,
                    &snapshot.items[slot],
                    caches,
                    self.grid.expect(
                        "transcript preparation installs its grid before warming visible slots",
                    ),
                    self.width,
                    &context,
                    self.frame.expect(
                        "transcript preparation starts a frame before warming visible slots",
                    ),
                    transition,
                );
            }
            self.touch(slot);
        }
        self.schedule_visible_fences(caches, scroll_y, height);
        self.refresh_mermaid_bytes(caches);
        self.enforce_budget(caches, &viewport);
        Arc::clone(&self.index)
    }
}

#[cfg(test)]
#[path = "genui_test.rs"]
mod genui_tests;

#[cfg(test)]
#[path = "transcript_test.rs"]
mod tests;

use crate::kit::atoms::TranscriptPublication;

use crate::kit::atoms::ViewModelsSnapshot;
