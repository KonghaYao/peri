use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::grid::{GridSpec, TextLineSource, ZoneTransition};
use super::selection::{SlotIndex, SlotLines};
use super::viewport_anchor::{PrependRelocation, ViewportAnchor};
use super::vm_cache::VmCacheSlot;
use crate::kit::atoms::{TranscriptPublication, ViewModelsSnapshot};
use crate::kit::entry_render_cache::{EntryInvalidation, EntryRenderContext};
use crate::kit::tui_render_unit::TuiRenderUnit;

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
/// Divider 属文本区；其余变体（工具卡片 / 子 Agent 组 / 折叠组 / 系统 / 交互 /
/// Todo）整体属过程区。human（user 气泡）不参与短回复缩进。
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

/// 台阶线色来源：按 entry 类型映射文本区左侧线条色——user 气泡用
/// `text.secondary`、assistant 正文用 `accent`、Divider 用 `text.secondary`（与
/// `── label ──` 同色）；主题值在渲染层解析（`render/helpers.rs::text_line_color`）。
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
    /// [REQ-C01-3 / A20] 最近一次发布的重定位结果（仅当该次发布与一次更早历史并入配对）。
    /// 渲染 body 取走（一次性）后交给 `run_auto_follow` 的并入分支消费；
    /// 语义与身份口径见 [`super::viewport_anchor`]。
    prepend: Option<PrependRelocation>,
    /// 已观测并配对的并入代数（`HISTORY_PREPEND_EPOCH`）——只在观测到**发布变化**
    /// 时推进，因此「epoch 先于发布被读到」不会漏配（见 `viewport_anchor` 模块文档 4.）。
    merge_epoch_seen: u64,
    structural_epoch_seen: u64,
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
        merge_epoch: u64,
    ) -> Arc<SlotIndex> {
        self.scanned_slots = 0;
        self.updated_slots = 0;
        self.evictions = 0;
        let structural_epoch = crate::kit::atoms::STRUCTURAL_EPOCH.get();
        let structural_changed = structural_epoch != self.structural_epoch_seen;
        if structural_changed {
            for cache in caches.iter_mut() {
                cache.evict();
            }
            self.bytes = vec![0; caches.len()];
            self.stamps = vec![0; caches.len()];
            self.recency.clear();
            self.retained_bytes = 0;
            self.index = Arc::new(SlotIndex::default());
            self.generation = None;
            self.structural_epoch_seen = structural_epoch;
        }
        let global = structural_changed
            || self.reset != reset
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
        // ── [REQ-C01-3 / A20] 发布**前**的锚点：旧布局此刻仍在位 ──
        // 身份重定位的两半都必须在这里做：`self.index` 还是上一次发布的索引
        // （旧槽位 + 旧 items），下一个分支就会把它改写成新快照的布局。`capture`
        // 只认渲染侧事实（索引 + scroll_y），不接收任何桥接槽位号。
        let published = self
            .generation
            .is_some_and(|generation| generation != snapshot.generation);
        let prepend_anchor = if published {
            ViewportAnchor::capture(&self.index, scroll_y)
        } else {
            None
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
        // ── [REQ-C01-3 / A20] 发布**后**的重定位：同一身份在新索引里重查视觉起点 ──
        // 只在「本次发布」与「一次更早历史并入」配对时产出：`HISTORY_PREPEND_EPOCH`
        // 仍是唯一的跨层事实（并合发生过），`merge_epoch_seen` 只在观测到发布变化时
        // 推进——epoch 早于/晚于发布被读到都不会漏配。身份认不出（被替换 / 被并入
        // 折叠组而消失 / 新窗口里出现多次）→ `relocate` 返回 `None` → 不补偿。
        // `from_offset` 取**未夹取**的当帧 scroll_y：它就是旧布局上视口顶行的位置。
        self.prepend = None;
        if published {
            if merge_epoch != self.merge_epoch_seen {
                self.prepend = prepend_anchor.and_then(|anchor| {
                    anchor
                        .relocate(&self.index)
                        .map(|target_offset| PrependRelocation {
                            from_offset: scroll_y,
                            target_offset,
                        })
                });
            }
            self.merge_epoch_seen = merge_epoch;
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
        crate::kit::atoms::RENDER_ACHIEVED_EPOCH.set(
            crate::kit::atoms::RENDER_ACHIEVED_EPOCH
                .get()
                .max(structural_epoch),
        );
        crate::kit::atoms::TUI_RENDER_CACHE_BYTES.set(self.retained_bytes);
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
        if self.retained_bytes > HEAVY_CACHE_BUDGET {
            self.evict_to_budget(caches, pinned, HEAVY_CACHE_TARGET);
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
            self.bytes[slot] = 0;
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
