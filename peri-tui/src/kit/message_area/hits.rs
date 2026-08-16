use std::sync::Arc;

use super::footer::KeepGoingLayout;
use super::grid::GridSpec;
use super::vm_cache::VmCacheSlot;
use crate::kit::atoms::{FocusedEntry, ViewModelsSnapshot};
use crate::kit::tui_render_unit::{TuiAssistantBubble, TuiRenderUnit};
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::Rect;

/// md 复制按钮的屏幕点击区域（每帧由渲染 body 构建，点击 handler 实时读取）。
/// [Why] 与 keepgoing 按钮同模式：事件在上帧渲染完成后分发，读取最近一帧的位置；
/// 存屏幕绝对坐标（含 scroll_y / area 偏移换算），handler 无需再查 wrap_map。
pub(super) struct CopyButtonHit {
    /// 按钮所在屏幕行（绝对坐标）。
    pub(super) row: u16,
    /// 按钮文本列范围（屏幕绝对坐标，[x_start, x_end)）。
    pub(super) x_start: u16,
    pub(super) x_end: u16,
    /// 所属 VM 在 VIEW_MODELS.items 中的索引——点击时读取其 text 复制。
    pub(super) slot_index: usize,
    /// 渲染时该 VM 的 content_hash——点击时校验索引仍指向同一 VM
    /// （Rewind / Reset 可能增删 items 导致索引错位）。
    pub(super) vm_hash: u64,
}

/// [Slice 4 §6.8] interaction option 的屏幕点击区域（每帧由渲染 body 构建，
/// 点击 handler 实时读取——与 CopyButtonHit 同模式；事件在上帧渲染完成后
/// 分发，读取最近一帧的位置）。单击 = 提交该选项（按钮语义）。
pub(super) struct InteractionOptionHit {
    /// 选项所在屏幕行（绝对坐标）。
    pub(super) row: u16,
    /// 选项文本列范围（屏幕绝对坐标，[x_start, x_end)；垂直排列/超宽时
    /// 整行命中）。
    pub(super) x_start: u16,
    pub(super) x_end: u16,
    /// 所属 VM 在 VIEW_MODELS.items 中的索引——点击时校验仍指向同一 VM。
    pub(super) slot_index: usize,
    /// 渲染时该 VM 的 content_hash——点击时校验（Rewind / Reset 索引错位防御）。
    pub(super) vm_hash: u64,
    /// 选项索引——提交时选择哪个 option。
    pub(super) option_index: usize,
}

/// @image 行的屏幕点击区域（每帧由渲染 body 构建，点击/hover handler 实时读取
/// ——与 CopyButtonHit 同模式；事件在上帧渲染完成后分发，读取最近一帧的位置）。
/// 存屏幕绝对坐标（含 scroll_y / area 偏移换算），handler 无需再查 wrap_map。
pub(super) struct ImageLineHit {
    /// meta 行所在屏幕行（绝对坐标）。
    pub(super) row: u16,
    /// 命中列范围（屏幕绝对坐标，[x_start, x_end)；content 区域内整行命中，
    /// 不含滚动条列——滚动条点击不被误吞）。
    pub(super) x_start: u16,
    pub(super) x_end: u16,
    /// 所属 VM 在 VIEW_MODELS.items 中的索引——点击时校验仍指向同一 VM
    /// （Rewind / Reset 可能增删 items 导致索引错位）。
    pub(super) slot_index: usize,
    /// 渲染时该 VM 的 content_hash——点击时校验（同 CopyButtonHit 防御）。
    pub(super) vm_hash: u64,
    /// 展示路径（T5 canonicalize 后；失败时为原始文本）——open 目标。
    pub(super) path: String,
    /// 受管理目录内（~/.peri/images）→ 自动预览候选。本任务（T4）仅传递
    /// 给 T7 预览资格判定，暂无读取点。
    #[allow(dead_code)]
    pub(super) managed: bool,
    /// 重建期算好的大小文案（B/KB/MB 或 missing）——hover 渲染复用，
    /// hover 时不再 stat（§4.4 stat 时机取舍）。
    pub(super) size_text: String,
    /// slot 内逻辑行索引（wrap_map 中该 meta 行的 visual_start）——hover 渲染定位。
    pub(super) logical_idx: usize,
}

/// @image 行 hover 状态（§4.4）：Moved 事件命中变化时由 handler 写入
/// [`IMAGE_HOVER`]，渲染 body 读取决定该 meta 行是否显示绝对路径 + accent
/// 高亮（移出/遮挡 → None 恢复默认渲染）。字段与 [`ImageLineHit`] 对齐
/// （渲染定位 + 陈旧校验）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ImageHoverState {
    /// 命中时鼠标所在屏幕行（绝对坐标）。
    pub(crate) row: u16,
    /// 所属 VM 在 VIEW_MODELS.items 中的索引。
    pub(crate) slot_index: usize,
    /// slot 内逻辑行索引——渲染定位（滚动后 hover 高亮跟随行）。
    pub(crate) logical_idx: usize,
    /// 渲染时该 VM 的 content_hash（陈旧校验）。
    pub(crate) vm_hash: u64,
    /// 展示路径（T5 canonicalize 后；失败时为原始文本）——hover 行显示。
    pub(crate) path: String,
    /// 重建期算好的大小文案（B/KB/MB 或 missing）。
    pub(crate) size_text: String,
}

// ── md 复制按钮屏幕位置（每帧更新）──
// 按钮行在 slot 内是唯一视觉行（copy_button_line 已保证不折行），
// 屏幕行 = area.y + slot 视觉偏移 + 行内视觉偏移 - scroll_y。
// 视口外的按钮不进入映射（点不到），避免列表随会话增长。
pub(super) fn update_copy_button_hits(
    copy_buttons: State<Arc<Vec<CopyButtonHit>>>,
    vm_caches: &State<Vec<VmCacheSlot>>,
    view_models: &AtomState<ViewModelsSnapshot>,
    area_rect: Option<Rect>,
    vis_height: u16,
    scroll_y: usize,
    slot_visual_starts: &[usize],
) {
    let mut hits: Vec<CopyButtonHit> = Vec::new();
    if let Some(area) = area_rect {
        let vp_end = area.y.saturating_add(vis_height);
        let caches_read = vm_caches.read();
        // [LOW-5] 点击校验的身份 hash：运行中 bubble 的 content_hash 每秒
        // 随 duration 漂移（G1 按秒刷新时长文本），跨秒点击会偶发拒绝——
        // 命中映射保存稳定身份 hash（assistant 排除时变 duration），
        // 事件时点按同口径比对。快照索引可能落后于 vm_caches（TOCTOU
        // 防御同 rebuild 阶段：越界回退 slot.content_hash）。
        let vms_guard = view_models.read();
        for (slot_index, slot) in caches_read.iter().enumerate() {
            let Some(btn) = &slot.copy_button else {
                continue;
            };
            let Some(entry) = slot.wrap_map.get(btn.logical_idx) else {
                continue;
            };
            let vis_row = slot_visual_starts
                .get(slot_index)
                .copied()
                .unwrap_or(0)
                .saturating_add(entry.visual_start);
            let row = area.y as i64 + vis_row as i64 - scroll_y as i64;
            if row < area.y as i64 || row >= vp_end as i64 {
                continue;
            }
            let hit_hash = match vms_guard.items.get(slot_index) {
                Some(TuiRenderUnit::TuiAssistantBubble(b)) => {
                    TuiAssistantBubble::stable_identity_hash(&b.text, b.reasoning.as_ref())
                }
                _ => slot.content_hash,
            };
            hits.push(CopyButtonHit {
                row: row as u16,
                x_start: area.x.saturating_add(btn.x_start),
                x_end: area.x.saturating_add(btn.x_end),
                slot_index,
                vm_hash: hit_hash,
            });
        }
    }
    *copy_buttons.write_no_update() = Arc::new(hits);
}

// ── [T4 §4] @image 行屏幕位置（每帧更新）──
// 与 md 复制按钮同模式：meta 行在 slot 内是完整渲染的逻辑行（部分截断的
// meta 行不进入映射——渲染位置与点击区域错位）。屏幕行 = area.y + slot
// 视觉偏移 + 行内视觉偏移 - scroll_y；视口外的行不进入映射（点不到）。
// 命中列 = content 区域内整行（前缀列到滚动条列前），wrap 续行同列区间
// 天然覆盖；滚动条列（最右 1 列）不进入——滚动条点击不被误吞。
pub(super) fn update_image_line_hits(
    image_rects: State<Arc<Vec<ImageLineHit>>>,
    vm_caches: &State<Vec<VmCacheSlot>>,
    area_rect: Option<Rect>,
    vis_height: u16,
    scroll_y: usize,
    slot_visual_starts: &[usize],
    grid: GridSpec,
) {
    let mut hits: Vec<ImageLineHit> = Vec::new();
    if let Some(area) = area_rect {
        let vp_end = area.y.saturating_add(vis_height);
        let caches_read = vm_caches.read();
        let content_x = area.x.saturating_add(grid.cont_prefix_width() as u16);
        for (slot_index, slot) in caches_read.iter().enumerate() {
            for info in &slot.image_lines {
                let Some(entry) = slot.wrap_map.get(info.logical_idx) else {
                    continue;
                };
                let vis_row = slot_visual_starts
                    .get(slot_index)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(entry.visual_start);
                let row = area.y as i64 + vis_row as i64 - scroll_y as i64;
                if row < area.y as i64 || row >= vp_end as i64 {
                    continue;
                }
                hits.push(ImageLineHit {
                    row: row as u16,
                    x_start: content_x,
                    x_end: content_x.saturating_add(grid.content),
                    slot_index,
                    vm_hash: slot.content_hash,
                    path: info.path.clone(),
                    managed: info.managed,
                    size_text: info.size_text.clone(),
                    logical_idx: info.logical_idx,
                });
            }
        }
    }
    *image_rects.write_no_update() = Arc::new(hits);
}

// ── [Slice 4 §6.8] interaction option 屏幕位置 + 当前项高亮信息（每帧更新）──
// 与 md 复制按钮同模式：视口外的选项不进映射（点不到）；高亮只作用于
// 视口行（G3 视口级，不动渲染缓存）。返回值供视口循环对「焦点 slot 的
// 当前 option 行」应用 selection bg + bold（§9）。
#[allow(clippy::too_many_arguments)]
pub(super) fn update_interaction_hits(
    interaction_rects: State<Arc<Vec<InteractionOptionHit>>>,
    vm_caches: &State<Vec<VmCacheSlot>>,
    area_rect: Option<Rect>,
    vis_height: u16,
    scroll_y: usize,
    slot_visual_starts: &[usize],
    focused_entry_atom: &AtomState<Option<FocusedEntry>>,
    interaction_option: &State<usize>,
) -> Option<(usize, usize)> {
    let mut hits: Vec<InteractionOptionHit> = Vec::new();
    let mut focused_interaction_row: Option<(usize, usize)> = None;
    // [S2 单一事实源] 渲染读点——与仲裁同读 FOCUSED_ENTRY（临时 guard
    // 语句末 drop；interaction_option 仍是局部派生，不参与跨组件仲裁）。
    let focus_slot = focused_entry_atom.read().as_ref().map(|f| f.slot);
    let cur_option = *interaction_option.read();
    if let Some(area) = area_rect {
        let vp_end = area.y.saturating_add(vis_height);
        let caches_read = vm_caches.read();
        for (slot_index, slot) in caches_read.iter().enumerate() {
            let Some(il) = &slot.interaction else {
                continue;
            };
            for (opt_i, row_local) in il.option_rows.iter().enumerate() {
                let Some(entry) = slot.wrap_map.get(*row_local) else {
                    continue;
                };
                let vis_row = slot_visual_starts
                    .get(slot_index)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(entry.visual_start);
                let row = area.y as i64 + vis_row as i64 - scroll_y as i64;
                if row < area.y as i64 || row >= vp_end as i64 {
                    continue;
                }
                // 列区间：横向布局按 option 文本区间；垂直/超宽整行命中
                let (x_start, x_end) = match il.option_cols.get(opt_i).copied().flatten() {
                    Some((s, e)) => (area.x.saturating_add(s), area.x.saturating_add(e)),
                    None => (
                        area.x,
                        area.x
                            .saturating_add(area.width)
                            .max(area.x.saturating_add(1)),
                    ),
                };
                hits.push(InteractionOptionHit {
                    row: row as u16,
                    x_start,
                    x_end,
                    slot_index,
                    vm_hash: slot.content_hash,
                    option_index: opt_i,
                });
                if focus_slot == Some(slot_index) && opt_i == cur_option {
                    focused_interaction_row = Some((slot_index, *row_local));
                }
            }
        }
    }
    *interaction_rects.write_no_update() = Arc::new(hits);
    focused_interaction_row
}

/// 计算 keepgoing 按钮的屏幕点击区域 `(y, x_start, width)`。
///
/// 渲染布局：core 行（`core_total_visual_rows` 行）→ footer_lines（`line_index` 行）
/// → padding 行。footer_lines[line_index] 的屏幕 y = area.y + core_total_visual_rows
/// + line_index - scroll_y（i64 数学避免 u16 下溢）。
///
/// 返回 None 的情形：
/// - empty：Welcome 布局（footer 渲染在 Welcome 之下，行位置模型不同）——按钮可见但不可点击
/// - 按钮被滚出视口（y 不在 [area.y, area.y + vis_height) 内）
pub(super) fn compute_keepgoing_rect(
    empty: bool,
    area_rect: Option<ratatui_kit::ratatui::layout::Rect>,
    layout: Option<KeepGoingLayout>,
    core_total_visual_rows: usize,
    scroll_y: usize,
    vis_height: u16,
) -> Option<(u16, u16, u16)> {
    let area = area_rect?;
    let layout = layout?;
    if empty {
        return None;
    }
    let row =
        area.y as i64 + core_total_visual_rows as i64 + layout.line_index as i64 - scroll_y as i64;
    let vp_end = area.y as i64 + vis_height as i64;
    if row < area.y as i64 || row >= vp_end {
        return None;
    }
    Some((
        row as u16,
        area.x.saturating_add(layout.start_col),
        layout.width,
    ))
}
