//! MessageArea：直接读取 VIEW_MODELS，通过 vm_to_lines 将 TuiRenderUnit
//! 转换为 Vec<Line>，按视口裁剪后渲染。
//!
//! - 滚动：由自持 ScrollPos（usize 偏移）处理键盘/鼠标事件（offset 管理）
//! - 渲染：视口裁剪——只 clone + highlight + 渲染视口内 ~60 行，避免 O(N×W) per render
//! - 智能跟随：use_effect 检测 VIEW_MODELS 变化
//! - 不再使用 RENDER_CACHE / render_bridge / ScrollView / wrap_map（已替换为 wrap_map_cache）

#![allow(clippy::needless_update)]

use std::sync::Arc;
use std::time::Instant;

#[cfg(test)]
use crate::kit::atoms::FocusedEntry;
use crate::kit::atoms::{
    BRIDGE_RESET_COUNTER, FOCUSED_ENTRY, IMAGE_HOVER, LANG_VERSION, LOADING_EPOCH, VIEW_MODELS,
};
use crate::kit::text_selection::TextSelection;
use crate::kit::tui_render_unit::TuiRenderUnit;
#[cfg(test)]
use crate::kit::tui_render_unit::{FoldKey, FoldState};
use crate::kit::welcome::Welcome;
use peri_theme::atoms::{PALETTE_ATOM, THEME_ATOM};
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::{
    layout::{Constraint, Direction},
    style::{Modifier, Style},
    text::{Line, Span, Text as RatText},
    widgets::{Block, Padding, Paragraph, Wrap},
};

mod entry_nav;
mod footer;
pub(crate) mod grid;
mod handlers;
mod hits;
mod image_action;
mod no_color;
mod props;
pub(crate) mod render;
pub(crate) mod scroll;
mod selection;
mod vm_cache;

#[cfg(test)]
use self::entry_nav::{
    apply_fold_override, apply_fold_toggle, cycle_interaction_option, entry_click_decision,
    fold_key_of, move_entry_focus, pending_interaction_of, set_entry_focus,
};
pub(crate) use self::hits::ImageHoverState;
use self::hits::{CopyButtonHit, ImageLineHit, InteractionOptionHit, compute_keepgoing_rect};
#[cfg(test)]
use self::image_action::hover_target_for;
// macOS-only 符号：对应 mod_test 中 `#[cfg(target_os = "macos")]` 测试，
// 非 macOS 平台下剔除避免 unused-import（CI ubuntu/windows 全量 clippy）。
#[cfg(all(test, target_os = "macos"))]
use self::image_action::{
    OpenImageError, build_open_command, build_open_command_with, try_open_image,
};
use self::no_color::strip_line_colors;
use self::vm_cache::{
    VmCacheSlot, palette_markdown_key, render_timing_enabled, total_visual_rows, trace_phase,
};
#[cfg(test)]
use footer::KeepGoingLayout;
use footer::build_footer_lines;
pub(crate) use footer::hash_todo_items;
pub use footer::{TodoItem, TodoStatus};
pub use props::MessageAreaProps;
use props::{MsgAreaTracker, ScrollbarFields, ScrollbarHook};
use render::vm_to_lines_cached;
#[cfg(test)]
use scroll::GesturePending;
use scroll::{DragThrottle, ScrollThrottle, ScrollbarDragState};
use selection::{
    WrappedLineInfo, build_wrap_map, concat_wrap_maps, highlight_line_in_selection,
    viewport_logical_range, visual_to_logical,
};

// ── 组件 ──────────────────────────────────────────────────────────────────

#[component]
pub fn MessageArea(props: &MessageAreaProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    tracing::trace!(target: "frozen_diag", "MessageArea: update/body called");
    let view_models = hooks.use_atom(&VIEW_MODELS);
    let acp_state = hooks.use_atom(&crate::kit::atoms::ACP_STATE);
    let todo_atom = hooks.use_atom(&crate::kit::atoms::TODO_ITEMS);
    hooks.use_atom(&LANG_VERSION);
    // 订阅 PALETTE_ATOM / THEME_ATOM：主题切换时触发 MessageArea 重渲染，
    // palette_key 同时包含 Markdown palette 与 code block 的 surface.sunken，
    // 确保所有 Markdown 色值随主题更新。
    let _palette = hooks.use_atom(&PALETTE_ATOM);
    let theme = hooks.use_atom(&peri_theme::atoms::THEME_ATOM);
    let current_palette_key =
        palette_markdown_key(&_palette.read(), theme.read().semantic.surface.sunken);
    // 订阅 TERMINAL_CAPS：NO_COLOR 时对可见行做颜色剥离（§12，G3 视口级 pass）。
    // 启动时探测一次后不再变化；订阅仅为语义完整（切换不重渲染也无副作用）。
    let caps = hooks.use_atom(&crate::kit::atoms::TERMINAL_CAPS);
    let strip_color = !caps.read().color;
    // 订阅 IMAGE_HOVER：Moved handler 写入时触发消息区重渲染（hover 行
    // 绝对路径显示/清除）。读取仍在渲染 body 视口循环（读副本，G3 视口级）。
    hooks.use_atom(&IMAGE_HOVER);

    let snapshot = view_models.read();
    let todo_items = todo_atom.read().clone();
    let is_loading = acp_state.read().is_loading;

    // [Slice 3] Transcript 统一水平网格（§3.1）——content 列宽来自 SessionColumn。
    let grid = props.grid;

    // [PERI_RENDER_TIMING] 帧计时起点
    let frame_t0 = if render_timing_enabled() {
        Some(Instant::now())
    } else {
        None
    };

    let vm_generation = snapshot.generation;

    // ── 按 VM 分片的渲染缓存 ──────────────────────────────────────────────────
    // vm_caches 与 snapshot.items 长度对齐，每个 slot 缓存一个 VM 的 lines + wrap_map。
    // [TRAP] write_no_update 必须用——ratatui-kit ReactiveMutRef::Drop 无条件 wake()，
    // render body 内 write 会自激回路 100% CPU。
    let vm_caches: State<Vec<VmCacheSlot>> = hooks.use_state(Vec::new);

    // ── Footer 行预计算：必须在 empty 分支之前调用，确保所有 hook 顺序一致 ──
    // keepgoing 防抖：KEEPGOING_BLOCKED_UNTIL 内的时间未过期 → 按钮禁用样式渲染
    let keepgoing_blocked = crate::kit::atoms::KEEPGOING_BLOCKED_UNTIL
        .state()
        .read()
        .is_some_and(|until| Instant::now() < until);
    // 消息区位置追踪 + 视宽：build_footer_lines 需要 vis_width 判断按钮是否
    // 超宽换行（m4：换行后点击区域与实际渲染位置错位，超宽时跳过按钮渲染）。
    let area_hook = hooks.use_hook(MsgAreaTracker::new);
    let area_rect = area_hook.rect;
    let vis_width = area_rect
        .map(|r| r.width.saturating_sub(1))
        .unwrap_or(grid.total_width() as u16)
        .max(1);
    let (footer_lines, keepgoing_layout, footer_has_content) = build_footer_lines(
        &mut hooks,
        is_loading,
        &todo_items,
        keepgoing_blocked,
        vis_width,
    );
    // keepgoing 按钮屏幕点击区域 (y, x_start, width)，每帧更新，点击 handler 实时读取
    // （handler 闭包捕获的是 State 句柄而非帧快照，滚动后坐标仍正确）
    let keepgoing_rect = hooks.use_state(|| Option::<(u16, u16, u16)>::None);
    // md 复制按钮屏幕点击区域（每帧更新，点击 handler 实时读取——同上）
    let copy_buttons = hooks.use_state(Arc::<Vec<CopyButtonHit>>::default);
    // [T4 §4] @image 行屏幕点击/hover 热区（每帧由渲染 body 更新，点击与
    // Moved handler 实时读取——同 CopyButtonHit 模式）。
    let image_rects = hooks.use_state(Arc::<Vec<ImageLineHit>>::default);

    let empty = snapshot.items.is_empty() && !is_loading && todo_items.is_empty();
    // [Why has_content 而非 !footer_lines.is_empty()] footer 常驻渲染后恒非空
    // （idle 态含静止 spinner 占位行），空态若据此判定会让 Welcome 页面被
    // footer 占位行污染；仅当有实质内容（summary/todo）时才在 Welcome 下展示。
    let brewed_lines = if empty && footer_has_content {
        Some(footer_lines.clone())
    } else {
        None
    };

    let scroll_state = hooks.use_state(scroll::ScrollPos::default);
    let prev_items_len = hooks.use_state(|| 0usize);
    let _prev_is_loading = hooks.use_state(|| false);
    let scroll_throttle = hooks.use_state(ScrollThrottle::default);
    let _todo_hash = hash_todo_items(&todo_items);

    // ── 文本选区 + 左键手势状态机 ──
    let text_sel = hooks.use_state(TextSelection::default);
    // 手势中间状态（Pending）：Down 冻结 screen/visual/entry_hit；Drag 超
    // 容差升级后置 None，Armed 由 text_sel.dragging 表达。
    let gesture = hooks.use_state(|| Option::<scroll::GesturePending>::None);
    let drag_throttle = hooks.use_state(DragThrottle::default);

    // 滚动条 fields state（hook 通过引用读取，避免 borrow 冲突）
    let scrollbar_fields = hooks.use_state(ScrollbarFields::default);
    hooks.use_hook(move || ScrollbarHook {
        fields: scrollbar_fields,
    });
    // 滚动条 thumb 拖拽状态（点击/拖拽事件处理器读写）
    let scrollbar_drag = hooks.use_state(ScrollbarDragState::default);
    // [Slice 2] §8.1 `↓ New output` 指示器屏幕点击区域 (y, x_start, x_end)，
    // 每帧由渲染 body 更新（write_no_update），点击 handler 实时读取——与
    // keepgoing / md 复制按钮同模式（事件在上帧渲染完成后分发）。
    let new_output_rect = hooks.use_state(|| Option::<(u16, u16, u16)>::None);
    // [Slice 4 §6.8] interaction option 点击热区（每帧由渲染 body 更新，
    // 点击 handler 实时读取——同 CopyButtonHit 模式）。
    let interaction_rects = hooks.use_state(Arc::<Vec<InteractionOptionHit>>::default);
    // [Slice 4 §6.8] interaction option 键盘焦点（entry 焦点内部态，不新增
    // FocusLayer——option 是 entry 焦点内部状态，仲裁仍走 message_nav_accepts）。
    // 焦点移动到其他 entry 时重置为 0（下方 handler）。
    let interaction_option = hooks.use_state(|| 0usize);
    // [Slice 4 §6.8]「等待时锚定此 block」：pending interaction block 的 slot
    // index（每帧派生：扫描快照，pending 完成 → 自然清除）。
    let anchor_slot_state = hooks.use_state(|| Option::<usize>::None);

    let vis_height = area_rect.map(|r| r.height).unwrap_or(60).max(1);

    // ── 遍历每个 VM，按 (content_hash, vis_width) 命中判断 ──
    // [Why] 流式期间只有最后一个 AssistantBubble 的 content_hash 变化（text 累积），
    // 其余 committed VM hash 完全稳定——直接复用 Arc<Vec<Line>> 和 Arc<Vec<WrappedLineInfo>>。
    // 单次成本从 O(N×W) 降至 O(W)。
    //
    // [Opt B] hash-only clone：只收集 content_hash (u64)，避免每帧全量 clone TuiRenderUnit。
    // rebuild 阶段按需通过 snapshot 重读 VM 数据（流式期间只追加，索引有效）。
    //
    // [TRAP] 必须先提取 hashes 再 drop(snapshot)，否则 vm_caches.write_no_update 与
    // view_models read guard 冲突。
    //
    // [Fix §15] [Slice 4 §6.8] pending interaction block 的 slot index 与
    // item_hashes 同一次扫描派生（原独立 O(N) 循环合并——每帧至多一次全量
    // 遍历；同一时刻至多一个 pending block（模态互斥），break 语义保留）。
    let mut anchor_slot: Option<usize> = None;
    // §8.2 动画帧：100ms 粒度壁钟 tick——running 类 VM 每帧重建以推进
    // braille 动画（与 render.rs anim_tick 同公式同源）。
    let anim_frame = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64 / 100)
        .unwrap_or(0);
    // running 类 VM 标记（tool/subagent/reasoning running）——与 hash 同一次
    // 扫描派生，rebuild 判定按帧强制重建这些 slot。
    let mut running_flags: Vec<bool> = Vec::with_capacity(snapshot.items.len());
    let item_hashes: Vec<u64> = snapshot
        .items
        .iter()
        .enumerate()
        .map(|(i, vm)| {
            if anchor_slot.is_none() && matches!(vm, TuiRenderUnit::TuiAskUserBlock(a) if a.pending)
            {
                anchor_slot = Some(i);
            }
            running_flags.push(vm.is_animating());
            vm.content_hash()
        })
        .collect();
    let items_len = item_hashes.len();
    // [Slice 4 §6.8] pending 完成（结果回写 pending=false）后扫描不到 →
    // anchor 自然清除。write_no_update 不触发自激重渲染；block 完成时
    // push_view_models 的 generation 变化会触发 auto_follow effect。
    if *anchor_slot_state.read() != anchor_slot {
        *anchor_slot_state.write_no_update() = anchor_slot;
    }
    drop(snapshot);

    // 同步 vm_caches 长度对齐 item_hashes
    if vm_caches.read().len() != items_len {
        vm_caches
            .write_no_update()
            .resize(items_len, VmCacheSlot::default());
    }

    // 第一阶段：检测哪些 slot 需要 rebuild（content_hash 或 vis_width 变化）
    // [Opt B] 用 item_hashes (Vec<u64>) 替代 items_vec 迭代，避免持有 TuiRenderUnit clone。
    let rebuild_indices: Vec<usize> = {
        let caches_read = vm_caches.read();
        item_hashes
            .iter()
            .enumerate()
            .filter_map(|(i, hash)| {
                let slot = &caches_read[i];
                if slot.content_hash != *hash
                    || slot.width != vis_width
                    || slot.palette_key != current_palette_key
                    || slot.lang_key != LANG_VERSION.get()
                    // §8.2 running 类 VM 按动画帧强制重建（hash 可能跨秒才变）
                    || (running_flags[i] && slot.anim_frame != anim_frame)
                {
                    Some(i)
                } else {
                    None
                }
            })
            .collect()
    };
    // [PERI_RENDER_TIMING] hash 比对耗时
    let t_hash = Instant::now();
    trace_phase(
        "hash+detect",
        frame_t0.unwrap_or(t_hash),
        Some(&format!(
            "items={items_len}, rebuilds={}",
            rebuild_indices.len()
        )),
    );

    // 第二阶段：rebuild 需要更新的 slot——只有这些 VM 调用 vm_to_lines + build_wrap_map
    // [Phase 2] markdown_cache 在 slot 内部跨 rebuild 复用：即使 VM hash 变化（text 追加 token），
    // cache 仍能命中上次 stable_text 前缀，仅处理新增 block。
    // [Borrow] 单次 write_no_update 持锁整个 rebuild 循环——ratatui-kit State 仅在
    // render body 单线程访问，无锁竞争。直接可变借用 slot.markdown_cache，避免 clone
    // ConvertState（含 Vec<Line>，clone 成本高）。
    //
    // [Opt B] 仅 rebuild_indices 非空时才重新 read snapshot 获取 VM 引用。
    // safe_len 防御 TOCTOU 索引越界（Rewind/Reset 可能缩短 items）。
    if !rebuild_indices.is_empty() {
        let snapshot2 = view_models.read();
        let safe_len = snapshot2.items.len().min(items_len);
        let mut caches = vm_caches.write_no_update();
        for i in &rebuild_indices {
            if *i >= safe_len {
                continue; // TOCTOU 防御：跳过不可用索引，下帧重新同步
            }
            let vm = &snapshot2.items[*i];
            let vm_hash = vm.content_hash();
            let slot = &mut caches[*i];
            let (lines, copy_button, interaction, image_lines) =
                vm_to_lines_cached(vm, &grid, &mut slot.markdown_cache, true);
            let lines = Arc::new(lines);
            let (_, wm) = build_wrap_map(&lines, vis_width);
            let visual_rows = wm.last().map(|e| e.visual_end).unwrap_or(0);
            slot.content_hash = vm_hash;
            slot.width = vis_width;
            slot.palette_key = current_palette_key;
            slot.lang_key = LANG_VERSION.get();
            slot.anim_frame = anim_frame;
            slot.lines = lines;
            slot.wrap_map = Arc::new(wm);
            slot.visual_rows = visual_rows;
            slot.copy_button = copy_button;
            slot.interaction = interaction;
            slot.image_lines = image_lines;
        }
    }
    // [PERI_RENDER_TIMING] rebuild 耗时（仅 rebuild_indices 非空时有意义）
    let t_rebuild = Instant::now();
    if !rebuild_indices.is_empty() {
        trace_phase(
            "rebuild",
            t_hash,
            Some(&format!("{} slots", rebuild_indices.len())),
        );
    }

    // 第三阶段：拼接 concat_wrap_map（累加 visual_offset 和 logical_idx）。
    // [Scheme D] 不再构建全量 core_lines——仅收集每个 slot 的 Arc<Vec<Line>> 引用
    // 和 slot_offsets（累积偏移）。视口循环按需从 slot 中提取行。
    // [Why] per-frame clone 全量 core_lines 是 Phase 1 之后的首要瓶颈。
    let mut core_total_visual_rows: usize = 0;
    let mut total_logical_lines: usize = 0;
    let mut slot_arcs: Vec<Arc<Vec<Line<'static>>>> = Vec::new();
    let mut slot_offsets: Vec<usize> = Vec::new();
    // 每个 slot 在拼接后的全量视觉行中的起始偏移（供 md 复制按钮换算屏幕坐标）。
    let mut slot_visual_starts: Vec<usize> = Vec::new();
    // 每个 slot 的视觉行数（与 slot_visual_starts 同长度）——anchor 视觉范围
    // 用 `start + rows` 直接算出，避免每帧对 concat_wrap_map 做第二次全量扫描。
    let mut slot_visual_rows: Vec<usize> = Vec::new();
    let concat_wrap_map: Vec<WrappedLineInfo> = {
        let caches_read = vm_caches.read();
        slot_arcs.reserve(caches_read.len());
        slot_offsets.reserve(caches_read.len());
        slot_visual_starts.reserve(caches_read.len());
        slot_visual_rows.reserve(caches_read.len());
        let mut slots: Vec<(&[WrappedLineInfo], usize, usize)> =
            Vec::with_capacity(caches_read.len());
        for (slot_index, slot) in caches_read.iter().enumerate() {
            let lines_start = total_logical_lines;
            slot_offsets.push(lines_start);
            total_logical_lines += slot.lines.len();
            slot_arcs.push(Arc::clone(&slot.lines));
            slots.push((&slot.wrap_map, lines_start, slot_index));
            slot_visual_starts.push(core_total_visual_rows);
            slot_visual_rows.push(slot.visual_rows);
            core_total_visual_rows += slot.visual_rows;
        }
        concat_wrap_maps(&slots)
    };
    let slot_arcs_arc: Arc<Vec<Arc<Vec<Line<'static>>>>> = Arc::new(slot_arcs);
    let slot_offsets_arc: Arc<Vec<usize>> = Arc::new(slot_offsets);
    let concat_wrap_map_arc: Arc<Vec<WrappedLineInfo>> = Arc::new(concat_wrap_map);
    // [PERI_RENDER_TIMING] concat 阶段耗时
    let num_slots = slot_arcs_arc.len();

    // [Slice 4 §6.8] anchor 的视觉行范围（core 行）：slot 起始偏移 + slot 内
    // 视觉行数。供 auto_follow 的 anchor 分支对齐视口到 block 底部。
    // 每帧由 anchor_slot 派生（resize 后按新快照/wrap_map 自动重算）。
    // [Fix §15] O(1) 查表：旧实现每帧对 concat_wrap_map 全量 filter 求最大
    // visual_end（O(total wrapped lines) 第二次全量遍历）——slot.visual_rows
    // 即 wrap_map 末项 visual_end（rebuild 时同源写入），与旧语义完全一致。
    let anchor_visual_range: Option<(usize, usize)> = anchor_slot_state.read().and_then(|slot| {
        let start = *slot_visual_starts.get(slot)?;
        let rows = *slot_visual_rows.get(slot)?;
        if rows == 0 {
            return None; // 空 slot（无 wrap 条目）→ 无锚定（与旧 max() 语义一致）
        }
        Some((start, start + rows))
    });
    let t_concat = Instant::now();
    trace_phase(
        "concat",
        t_rebuild,
        Some(&format!(
            "slots={num_slots}, logic_lines={total_logical_lines}, vis_rows={core_total_visual_rows}"
        )),
    );

    // ── 总视觉行数 = core + footer ──
    // [Why] 分片缓存命中后 core_total_visual_rows 已是 sum(slot.visual_rows)，无需 line_count。
    // footer 通常几行，单独 build_wrap_map 成本可忽略。
    let footer_visual_rows: usize = if !empty && !footer_lines.is_empty() {
        let (_, footer_map) = build_wrap_map(&footer_lines, vis_width);
        footer_map.last().map(|e| e.visual_end).unwrap_or(0)
    } else {
        0
    };
    // [Padding] 追加 SCROLL_PADDING 行空白滚动空间，减少流式输出期间因新行到达
    // 导致的视口抖动。这 2 行不计入实际渲染内容，仅影响 max_scroll / content_length；
    // 吸底跟随恢复判定会扣除它们（scroll::should_follow_after_user_scroll）。
    let total_visual_rows =
        total_visual_rows(core_total_visual_rows, footer_visual_rows, is_loading);

    handlers::register_keepgoing_click(&mut hooks, keepgoing_rect);

    handlers::register_md_copy_click(&mut hooks, copy_buttons, view_models);

    // ── 鼠标事件处理（滚动 + 文本拖拽选中复制）──
    // [TRAP] event_handler 闭包必须是 'static → 必须 move。但 concat_wrap_map_arc /
    // slot_arcs_arc / slot_offsets_arc 后续视口裁剪也要用。Arc::clone 是 O(1) 引用计数，
    // ── 吸底自动跟随 ──
    let last_scrolled_at = hooks.use_state(|| 0usize);
    // 粘性吸底开关：默认跟随；用户向上滚动即退出（浏览模式），滚回底部才恢复。
    let follow_bottom = hooks.use_state(|| true);
    handlers::register_new_output_indicator_click(
        &mut hooks,
        new_output_rect,
        follow_bottom,
        scroll_state,
    );

    handlers::register_interaction_option_click(&mut hooks, interaction_rects, view_models);

    handlers::register_image_click(&mut hooks, image_rects, view_models);

    handlers::register_image_hover(&mut hooks, image_rects);

    // [S2 单一事实源] FOCUSED_ENTRY 订阅（hook 声明必须在 handler 之前，hook
    // 顺序每次渲染一致）：仲裁/渲染/外部清除共读同一事实源，无收敛窗口期。
    let focused_entry_atom = hooks.use_atom(&FOCUSED_ENTRY);
    handlers::register_entry_click(&mut hooks, area_rect, gesture, interaction_option, text_sel);

    // 闭包持 clone，原值继续在 render body 内用。
    // [Why 位置] 必须声明在 follow_bottom 之后（闭包捕获），且所有 hook 每次渲染
    // 以相同相对顺序调用——use_event_handler 只是占顺序槽，位置调整无状态错位。
    handlers::register_scroll_events(
        &mut hooks,
        area_rect,
        vis_width,
        scroll_state,
        scroll_throttle,
        text_sel,
        gesture,
        drag_throttle,
        scrollbar_fields,
        scrollbar_drag,
        follow_bottom,
        view_models,
        grid,
        Arc::clone(&concat_wrap_map_arc),
        Arc::clone(&slot_arcs_arc),
        Arc::clone(&slot_offsets_arc),
    );

    handlers::register_keyboard_nav(&mut hooks, interaction_option);

    // [S2 单一事实源] 焦点同步已删除：外部清除（输入区点击 / session 复位）
    // 在事件边界直接写 FOCUSED_ENTRY = None，仲裁与渲染同源读取——不再需要
    // effect 收敛局部 entry_focus（旧双轨：仲裁读局部、清除只写共享 → 窗口期）。

    let prev_total_visual_rows = hooks.use_state(|| 0usize);
    // [Fix] resize 高度变化哨兵：vis_height 加入 effect 依赖，终端高度变化时触发
    // auto_follow 的 resize 跟随逻辑（否则 resize 缩小视口后底部 footer/spinner 消失）。
    let prev_vis_height = hooks.use_state(|| 0u16);
    // [Fix] Submit 强制滚底 / History 切换强制滚底：订阅 atom 重新渲染
    let _loading_epoch_atom = hooks.use_atom(&LOADING_EPOCH);
    let _reset_counter_atom = hooks.use_atom(&BRIDGE_RESET_COUNTER);
    let loading_epoch = LOADING_EPOCH.get();
    let prev_loading_epoch = hooks.use_state(|| loading_epoch);
    let bridge_reset_counter = BRIDGE_RESET_COUNTER.get();
    let prev_reset_counter = hooks.use_state(|| bridge_reset_counter);
    hooks.use_effect(
        {
            move || {
                scroll::run_auto_follow(&scroll::AutoFollowCtx {
                    total_visual_rows,
                    vis_height,
                    scroll_state,
                    prev_items_len,
                    last_scrolled_at,
                    items_len,
                    is_loading,
                    follow_bottom,
                    prev_total_visual_rows,
                    prev_vis_height,
                    loading_epoch,
                    prev_loading_epoch,
                    bridge_reset_counter,
                    prev_reset_counter,
                    // [Slice 4 §6.8] pending interaction block 锚定范围
                    // （每帧派生值；effect 依赖不含它——block 完成时 generation
                    // 变化触发 effect，用当帧最新值）。
                    anchor_visual_range,
                })
            }
        },
        (
            items_len,
            vm_generation,
            is_loading,
            total_visual_rows,
            vis_height,
        ),
    );

    if empty {
        // 重置滚动条字段，避免 Welcome 页面残留旧会话的滚动条
        *scrollbar_fields.write_no_update() = ScrollbarFields::default();
        // 清空 md 复制按钮映射——Welcome 页面无消息，防止点击残留按钮复制已消失内容
        *copy_buttons.write_no_update() = Arc::new(Vec::new());
        // 清空 @image 行热区映射（同 copy_buttons——Welcome 页面无消息可点）
        *image_rects.write_no_update() = Arc::new(Vec::new());
        if let Some(lines) = brewed_lines {
            return element!(
                View(
                    flex_direction: Direction::Vertical,
                    width: Constraint::Fill(1),
                    height: Constraint::Fill(1),
                ) {
                    View(height: Constraint::Fill(1)) {
                        Welcome(width: grid.content_width())
                    }
                    Text(text: Paragraph::new(RatText::from(lines)).wrap(Wrap { trim: false }))
                }
            )
            .into_any();
        }
        return element!(
            View(width: Constraint::Fill(1), height: Constraint::Fill(1)) {
                Welcome(width: grid.content_width())
            }
        )
        .into_any();
    }

    // ── 视口裁剪渲染（移除 ScrollView 的全量大 buffer）──
    // [TRAP] 原 ScrollView + Paragraph-with-Wrap 组合是 100% CPU 的真凶：ScrollView 创建
    // (width × total_visual_rows) 大 buffer，Paragraph 渲染所有 N 行到这个 buffer 是
    // O(N×W) per render。Drag 60-120Hz × O(N×W) 直接拉满 CPU。
    //
    // 视口裁剪：只 clone + highlight + 渲染视口内 ~60 行（vis_height）。
    //   1. 通过 wrap_map_cache 二分查找视口对应的逻辑行 [vp_start, vp_end]
    //   2. vp_first_offset = scroll_y - wrap_map[vp_start].visual_start（首行视觉偏移）
    //   3. viewport_lines = clone(core[vp_start..=vp_end]) + 必要时附加 footer_lines
    //   4. Paragraph::scroll((vp_first_offset, 0)) 精确偏移首行
    //
    // highlight：视口内选区行用 sel_bg 背景。不再缓存 highlight 结果——视口裁剪后
    // 只有 ~60 行，highlight 成本可忽略。Drag 期间频繁跨逻辑行变化也不会卡。
    //
    // [DEBUG] PERI_NO_HIGHLIGHT=1 紧急回退——完全不进入 highlight 路径。
    let no_highlight = std::env::var("PERI_NO_HIGHLIGHT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    // clamp scroll_y 不超过 max_scroll（替代 ScrollView 渲染时的 clamp）
    let max_scroll = total_visual_rows.saturating_sub(vis_height as usize);
    let scroll_y_raw = scroll_state.read().offset();
    let scroll_y = scroll_y_raw.min(max_scroll);

    // [Fix] 每帧钳制 scroll_state.offset 到 [0, max_scroll]。
    // apply_scroll 的 scroll_down() 无限递增 offset，没有上限感知——用户可以一直
    // 往下滚直到 offset 远超 max_scroll。虽然 scroll_y = raw.min(max_scroll) 让
    // 渲染正确，但 scroll_state 内部 offset 不被重置，往上滚时需要把多余 offset
    // 消耗完（如 offset=100, max_scroll=40 → 需滚 60 次才恢复）。
    // write_no_update 不触发 re-render，避免自激回路。
    if scroll_y_raw > max_scroll {
        scroll_state.write_no_update().set_offset(max_scroll);
    }

    // 更新 scrollbar fields——post_component_draw 时基于此渲染滚动条
    //
    // [Fix] ratatui Scrollbar 的 position 模型是 "item index"，max_position =
    // content_length - 1。但我们的 scroll_y 是 scroll offset，max = content_length -
    // vis_height。直接传 scroll_y 会导致 thumb 永远到不了底部（因为 scroll_y <
    // content_length - 1）。需要把 [0, max_scroll] 线性映射到 [0, content_length-1]。
    {
        let mut g = scrollbar_fields.write_no_update();
        g.content_length = total_visual_rows;
        g.position = scroll_y
            .saturating_mul(total_visual_rows.saturating_sub(1))
            .checked_div(max_scroll)
            .unwrap_or(0);
        g.viewport_length = vis_height as usize;
    }

    // [Fix 滚动尾随] 渲染帧兜底 flush：ratatui-kit 无 tick 定时器，停手后事件不再
    // 到达时，残留 pending_delta 在任意后续渲染帧（鼠标移动/内容更新/其他事件）
    // 落地，消除「滚动停止不到位」；反向抵消已由 apply_scroll 先行处理。
    // write_no_update 不触发 wake，无自激重渲染。
    scroll::flush_scroll_if_due(
        &scroll_throttle,
        &scroll_state,
        &scrollbar_fields,
        &follow_bottom,
    );

    let vp_height = vis_height as usize;

    // ── keepgoing 按钮屏幕位置（每帧更新）──
    // 必须在 scroll_y 计算之后、handler 使用之前；write_no_update 避免自激重渲染。
    {
        let mut k_rect = keepgoing_rect.write_no_update();
        *k_rect = compute_keepgoing_rect(
            empty,
            area_rect,
            keepgoing_layout,
            core_total_visual_rows,
            scroll_y,
            vis_height,
        );
    }

    hits::update_copy_button_hits(
        copy_buttons,
        &vm_caches,
        &view_models,
        area_rect,
        vis_height,
        scroll_y,
        &slot_visual_starts,
    );

    hits::update_image_line_hits(
        image_rects,
        &vm_caches,
        area_rect,
        vis_height,
        scroll_y,
        &slot_visual_starts,
        grid,
    );

    let interaction_highlight: Option<(usize, usize)> = hits::update_interaction_hits(
        interaction_rects,
        &vm_caches,
        area_rect,
        vis_height,
        scroll_y,
        &slot_visual_starts,
        &focused_entry_atom,
        &interaction_option,
    );

    // core_total_visual_rows 在前面拼接 wrap_map 时算出，直接复用。

    // 选区范围（字符级）：(first_logical, last_logical, sr, sc, er, ec)
    // 视口外选区不参与 highlight，selection state 保留供复制
    // [Why] 字符级高亮——旧版只存 (first_logical, last_logical) 整逻辑行范围，
    // 导致整行背景色覆盖；与字符级复制提取不一致。现在保留完整 (sr, sc, er, ec)，
    // highlight_line_in_selection 用 wrap_byte_starts 算 byte 范围，拆分 spans。
    // sr/er 为视觉行（usize，内容可超 65535 视觉行），sc/ec 为视觉列（u16）。
    let sel_bounds: Option<(usize, usize, usize, u16, usize, u16)> = if !no_highlight {
        let sel = text_sel.read();
        if let Some(((sr, sc), (er, ec))) = sel.normalized_bounds() {
            // Clamp sr/er 到 wrap_map 视觉范围内（footer 区域无 wrap_map）
            let max_visual = concat_wrap_map_arc
                .last()
                .map(|e| e.visual_end.saturating_sub(1))
                .unwrap_or(0);
            let sr_c = sr.min(max_visual);
            let er_c = er.min(max_visual);
            match (
                visual_to_logical(sr_c, &concat_wrap_map_arc),
                visual_to_logical(er_c, &concat_wrap_map_arc),
            ) {
                (Some(f), Some(l)) => Some((f.min(l), f.max(l), sr_c, sc, er_c, ec)),
                _ => None,
            }
        } else {
            None
        }
    } else {
        None
    };

    // 视口对应的 core 逻辑行范围 + 首行视觉偏移
    let (vp_core_start, vp_core_end, vp_first_offset): (usize, usize, u16) =
        if scroll_y < core_total_visual_rows && total_logical_lines > 0 {
            viewport_logical_range(&concat_wrap_map_arc, scroll_y, vp_height).unwrap_or((0, 0, 0))
        } else {
            // 视口完全在 footer 内（footer 占据末尾几行）
            (0, 0, 0)
        };

    // 视口是否包含 footer（视口末尾超出 core 总视觉行数）
    let viewport_has_footer =
        !empty && !footer_lines.is_empty() && scroll_y + vp_height > core_total_visual_rows;

    // 构建 viewport_lines：clone + highlight 视口内的 core 行，必要时附加 footer
    let sel_bg = THEME_ATOM.state().read().semantic.surface.selection;
    // [T4 §4] hover 状态读副本（每帧一次）：视口 post-pass 据此把命中的
    // @image meta 行替换为绝对路径 + accent 高亮（G3 视口级，不写缓存）。
    // 陈旧校验：hover 的 vm_hash 与当帧 slot 的 content_hash 不一致
    // （Rewind/Reset 后索引错位 / 内容变更）→ 不应用（防御误高亮）。
    let image_hover = {
        let caches = vm_caches.read();
        IMAGE_HOVER.state().read().as_ref().and_then(|h| {
            caches
                .get(h.slot_index)
                .filter(|s| s.content_hash == h.vm_hash)
                .map(|_| h.clone())
        })
    };
    // [Slice 3] §9 focus 视觉（G3：只作用于视口行，不写缓存/业务状态）：
    // focused entry 左缘 selection border（outer 列，空格 + selection 背景
    // 反色——整格填充色块，视觉为连续竖线；NO_COLOR 时退化为 `|` 字符，
    // §12 状态不依赖颜色）。
    // [S2 单一事实源] 渲染读点——与仲裁同读 FOCUSED_ENTRY。
    let focus_slot = focused_entry_atom.read().as_ref().map(|f| f.slot);
    let focus_caps = crate::kit::atoms::TERMINAL_CAPS.state();
    let focus_border_glyph = if focus_caps.read().color { " " } else { "|" };
    let focus_border_style = Style::default().bg(sel_bg);
    let core_len = total_logical_lines;
    let mut viewport_lines: Vec<Line<'static>> = Vec::with_capacity(
        (vp_core_end.saturating_sub(vp_core_start) + 1)
            .min(vp_height + 2)
            .saturating_add(footer_lines.len()),
    );

    if scroll_y < core_total_visual_rows && vp_core_start <= vp_core_end && core_len > 0 {
        let end = vp_core_end.min(core_len - 1);
        // concat_wrap_map_arc 已是 Arc，clone 引用计数 O(1)
        let wrap_map_arc = Arc::clone(&concat_wrap_map_arc);
        // [Scheme D] 通过 slot_index + slot_offsets 按需从 slot 中提取行
        let slots_arc = Arc::clone(&slot_arcs_arc);
        let offsets_arc = Arc::clone(&slot_offsets_arc);
        for i in vp_core_start..=end {
            let in_sel = sel_bounds.is_some_and(|(f, l, _, _, _, _)| i >= f && i <= l);
            if let Some(entry) = wrap_map_arc.get(i) {
                let local_idx = i - offsets_arc[entry.slot_index];
                let line = &slots_arc[entry.slot_index][local_idx];
                let mut out = if in_sel {
                    let (_, _, sr, sc, er, ec) = sel_bounds.unwrap();
                    highlight_line_in_selection(line, entry, sr, er, sc, ec, vis_width, sel_bg)
                } else {
                    line.clone()
                };
                // [T4 §4] @image hover 视觉 post-pass：命中行替换为绝对路径 +
                // accent 高亮。按 (slot_index, logical_idx) 定位——滚动后 hover
                // 高亮跟随行移动，不依赖鼠标位置。
                if let Some(h) = image_hover.as_ref()
                    && entry.slot_index == h.slot_index
                    && local_idx == h.logical_idx
                {
                    let hover_sem = THEME_ATOM.state().read().semantic;
                    out = render::render_image_hover_line(h, &grid, &hover_sem);
                }
                // [Slice 3] focus 视觉 post-pass——替换首列 outer 空 cell
                // （渲染层约定：非空行首 span 恒为裸空格 outer cell）。
                if focus_slot == Some(entry.slot_index)
                    && let Some(first) = out.spans.first_mut()
                    && first.content.as_ref() == " "
                    && first.style == Style::default()
                {
                    *first = Span::styled(focus_border_glyph, focus_border_style);
                }
                // [Slice 4 §6.8] interaction「当前项」高亮：焦点 slot 的当前
                // option 行（§9：selection bg + border + bold——border 是选项
                // 静态 `[ ]` 外框，bg + bold 在此应用）。
                if interaction_highlight == Some((entry.slot_index, local_idx))
                    && !out.spans.is_empty()
                {
                    out.style = out.style.bg(sel_bg).add_modifier(Modifier::BOLD);
                }
                viewport_lines.push(out);
            }
        }
    }

    // [Slice 2] §8.1 `↓ New output` 指示器——浏览态（用户滚离底部）且视口未到
    // 真实内容底时，在视口末尾（有 footer 时插在 footer 之前）插入指示行。
    // 视口附加行：不进 VmCacheSlot / wrap_map / total_visual_rows（G3 视口级）；
    // NO_COLOR 剥离 pass（下方）天然覆盖（文本保留、颜色剥离）。
    // [Why 内容底口径] total_visual_rows 含 SCROLL_PADDING 缓冲（不可见行），
    // 判定以「真实内容底」= core + footer 视觉行数为准——滚到视觉底部即消失，
    // 与粘性 follow 恢复（should_follow_after_user_scroll 扣缓冲）口径对齐。
    let new_output_active = scroll::new_output_indicator_active(
        *follow_bottom.read(),
        scroll_y,
        vp_height,
        core_total_visual_rows + footer_visual_rows,
    );
    {
        let mut rect = new_output_rect.write_no_update();
        if new_output_active && let Some(area) = area_rect {
            let arrow = if caps.read().unicode { "\u{2193}" } else { "v" };
            let sem = THEME_ATOM.state().read().semantic;
            let mut spans = vec![Span::raw(" ".repeat(grid.first_prefix_width()))];
            spans.push(Span::styled(
                format!("{arrow} {}", crate::i18n::tr("msg-new-output")),
                Style::default()
                    .fg(sem.status.running)
                    .add_modifier(Modifier::BOLD),
            ));
            viewport_lines.push(Line::from(spans));
            // 屏幕行 = area.y + 指示行在 viewport_lines 中的索引 - vp_first_offset
            // （Paragraph::scroll 仅跳过首行的视觉偏移，viewport_lines 完整传入；
            // 指示行是 push 后的末元素）。
            let screen_row =
                area.y as i64 + viewport_lines.len() as i64 - 1 - vp_first_offset as i64;
            let vp_end = area.y as i64 + i64::from(vis_height);
            *rect = if screen_row >= area.y as i64 && screen_row < vp_end {
                let x_end = area
                    .x
                    .saturating_add(area.width)
                    .max(area.x.saturating_add(1));
                Some((screen_row as u16, area.x, x_end))
            } else {
                None
            };
        } else {
            *rect = None;
        }
    }

    if viewport_has_footer {
        viewport_lines.extend(footer_lines.iter().cloned());
    }
    // [NO_COLOR 剥离 pass]（§12）：只作用于视口内可见行，不写缓存/业务状态（G3）。
    // 剥离颜色但保留 modifier（bold/italic/dim）与符号、文本——状态不依赖颜色。
    if strip_color {
        for line in viewport_lines.iter_mut() {
            *line = strip_line_colors(line);
        }
    }
    // [PERI_RENDER_TIMING] 视口裁剪耗时
    let t_viewport = Instant::now();
    trace_phase(
        "viewport",
        t_concat,
        Some(&format!("vp_lines={}", viewport_lines.len())),
    );

    // Paragraph::scroll 偏移：core 内的偏移 = vp_first_offset
    // 视口完全在 footer 内时（scroll_y >= core_total_visual_rows），按 footer 内偏移
    let scroll_offset_y: u16 = if scroll_y >= core_total_visual_rows && core_total_visual_rows > 0 {
        (scroll_y - core_total_visual_rows) as u16
    } else {
        vp_first_offset
    };

    // [Why] View 必须保持 `Fill(1)`——ScrollbarHook 在 MessageArea 的 drawer.area
    // 最右 1 列渲染滚动条 thumb。若 View 改为 `Max(vis_width)`，View 自身 area 缩到
    // vis_width，ScrollbarHook 的 drawer.area 也跟着缩，导致 thumb 渲染在 area.width-2
    // 处（向左偏 1 列）。
    //
    // 让 Paragraph 实际 wrap 宽度 = vis_width 的正确做法：给 Paragraph 套
    // `Block::default().padding(Padding::new(0, 1, 0, 0))`（右 padding 1）。Block 占满
    // View 的 area.width，内部 wrap 宽度 = area.width - 1 = vis_width，与
    // `total_visual_rows` / `wrap_map_cache` / `line_count(vis_width)` 的估算一致；
    // 右 padding 1 列留给 scrollbar thumb（post_component_draw 时绘制覆盖 padding 空白）。
    // [PERI_RENDER_TIMING] 帧总耗时
    trace_phase(
        "frame-total",
        frame_t0.unwrap_or(t_viewport),
        Some(&format!("gen={vm_generation}")),
    );
    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            Text(text: Paragraph::new(RatText::from(viewport_lines))
                .wrap(Wrap { trim: false })
                .block(Block::default().padding(Padding::new(0, 1, 0, 0)))
                .scroll((scroll_offset_y, 0)))
        }
    )
    .into_any()
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
