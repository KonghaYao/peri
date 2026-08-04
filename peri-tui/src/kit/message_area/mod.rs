//! MessageArea：直接读取 VIEW_MODELS，通过 vm_to_lines 将 TuiRenderUnit
//! 转换为 Vec<Line>，按视口裁剪后渲染。
//!
//! - 滚动：由自持 ScrollPos（usize 偏移）处理键盘/鼠标事件（offset 管理）
//! - 渲染：视口裁剪——只 clone + highlight + 渲染视口内 ~60 行，避免 O(N×W) per render
//! - 智能跟随：use_effect 检测 VIEW_MODELS 变化
//! - 不再使用 RENDER_CACHE / render_bridge / ScrollView / wrap_map（已替换为 wrap_map_cache）

#![allow(clippy::needless_update)]

use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::kit::atoms::{
    BRIDGE_RESET_COUNTER, KEEPGOING_BLOCKED_UNTIL, LANG_VERSION, LOADING_EPOCH, RENDER_HEARTBEAT,
    SUBMIT_TX, VIEW_MODELS,
};
use crate::kit::mouse_router;
use crate::kit::submit_request::SubmitRequest;
use crate::kit::text_selection::TextSelection;
use crate::kit::tui_render_unit::TuiRenderUnit;
use crate::kit::welcome::Welcome;
use peri_theme::atoms::{PALETTE_ATOM, THEME_ATOM};
use ratatui_kit::{
    crossterm::event::{Event, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        text::{Line, Text as RatText},
        widgets::{Block, Padding, Paragraph, Wrap},
    },
};

mod footer;
mod props;
mod render;
pub(crate) mod scroll;
mod selection;
use footer::{KeepGoingLayout, build_footer_lines, hash_todo_items};
pub use footer::{TodoItem, TodoStatus};
pub use props::MessageAreaProps;
use props::{MsgAreaTracker, ScrollbarFields, ScrollbarHook};
use render::vm_to_lines_cached;
use scroll::{DragThrottle, ScrollThrottle, ScrollbarDragState};
use selection::{
    WrappedLineInfo, build_wrap_map, concat_wrap_maps, copy_to_clipboard,
    highlight_line_in_selection, mark_copy_message, viewport_logical_range, visual_to_logical,
};

/// keepgoing 按钮点击防抖时长（连续点击冷却）。
const KEEPGOING_DEBOUNCE: Duration = Duration::from_millis(1500);

/// 计算 palette 中影响 markdown 渲染的关键字段哈希。
/// 当主题切换时，hash 变化 → 触发 vm_caches 重建 → markdown 色值更新。
fn palette_markdown_key(p: &ratatui_kit::prelude::Palette) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.fg.hash(&mut h);
    p.bg.hash(&mut h);
    p.fg_dim.hash(&mut h);
    p.accent.hash(&mut h);
    p.surface.hash(&mut h);
    p.border.hash(&mut h);
    p.success.hash(&mut h);
    p.warning.hash(&mut h);
    p.error.hash(&mut h);
    p.info.hash(&mut h);
    h.finish()
}

// ── 渲染性能诊断（PERI_RENDER_TIMING=1 启用）──────────────────────────────

fn render_timing_enabled() -> bool {
    thread_local! {
        static ENABLED: bool = std::env::var("PERI_RENDER_TIMING")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
    }
    ENABLED.with(|&e| e)
}

/// 如果启用诊断，打印阶段耗时。
#[track_caller]
fn trace_phase(phase: &str, start: Instant, detail: Option<&str>) {
    if render_timing_enabled() {
        let elapsed_us = start.elapsed().as_micros();
        let extra = detail.map(|d| format!(" | {d}")).unwrap_or_default();
        tracing::info!(target: "perf.render", "[{phase}] {elapsed_us}μs{extra}");
    }
}

// ── 按 VM 分片的渲染缓存 ──────────────────────────────────────────────────
//
// [Why] 旧版 lines_cache / wrap_map_cache / total_rows_cache 以 (vm_generation, width)
// 为 key，但 push_view_models 每个 token 都 generation += 1，流式期间缓存永远不命中，
// 每个 token 都触发 O(N×W) 的全量 markdown 解析 + wrap_map 重建 + line_count → CPU 拉满。
//
// 现在按 VM 的 content_hash 分片：只有正在流式（hash 变化）的那个 VM 重新解析 markdown
// + 重建 wrap_map，其余 VM 直接 Arc::clone 复用。流式单次成本从 O(N×W) 降至 O(W)。
//
// content_hash 由 build_view_models / TuiAssistantBubble::recompute_hash 维护，
// 已覆盖 text / reasoning.text / reasoning.collapsed / tool duration(secs) 等可变字段。
#[derive(Clone, Default)]
struct VmCacheSlot {
    /// 上次渲染时 VM 的 content_hash。变化时（流式追加 text、折叠/展开 reasoning、
    /// tool duration 跨秒）触发 markdown 重新解析 + wrap_map 重建。
    content_hash: u64,
    /// 上次渲染时的视宽。width 变化（窗口 resize）时 wrap 规则改变，必须重建。
    width: u16,
    /// 上次渲染时的 palette 关键字段哈希。主题切换时 hash 变化 → 强制重建所有 VM 的 markdown 渲染。
    palette_key: u64,
    /// 上次渲染时的 LANG_VERSION。语言切换时递增 → 强制重建（md 复制按钮文本依赖 i18n）。
    lang_key: u64,
    /// 该 VM 解析后的所有 Line（markdown + reasoning + tool card 渲染结果）。
    lines: Arc<Vec<Line<'static>>>,
    /// 该 VM 内部 wrap_map（visual_row 从 0 起）。拼接时累加 visual_offset 和 logical_idx 偏移。
    wrap_map: Arc<Vec<WrappedLineInfo>>,
    /// 该 VM 占据的视觉行数（= wrap_map 末项 visual_end）。
    visual_rows: u16,
    /// [Phase 2] markdown 增量渲染缓存——按文本前缀复用 stable_state，仅处理新增 block。
    /// 仅 AssistantBubble / UserBubble 实际使用；其他 VM 类型保留默认值不消耗资源。
    markdown_cache: crate::kit::markdown::MarkdownRenderCache,
    /// md 复制按钮布局（slot 内逻辑索引 + 列范围）。None = 该 VM 无按钮
    /// （非 AssistantBubble / 空文本 / 宽度不足）。rebuild 时随 lines 重建。
    copy_button: Option<render::CopyButtonInfo>,
}

/// md 复制按钮的屏幕点击区域（每帧由渲染 body 构建，点击 handler 实时读取）。
/// [Why] 与 keepgoing 按钮同模式：事件在上帧渲染完成后分发，读取最近一帧的位置；
/// 存屏幕绝对坐标（含 scroll_y / area 偏移换算），handler 无需再查 wrap_map。
struct CopyButtonHit {
    /// 按钮所在屏幕行（绝对坐标）。
    row: u16,
    /// 按钮文本列范围（屏幕绝对坐标，[x_start, x_end)）。
    x_start: u16,
    x_end: u16,
    /// 所属 VM 在 VIEW_MODELS.items 中的索引——点击时读取其 text 复制。
    slot_index: usize,
    /// 渲染时该 VM 的 content_hash——点击时校验索引仍指向同一 VM
    /// （Rewind / Reset 可能增删 items 导致索引错位）。
    vm_hash: u64,
}

// ── 组件 ──────────────────────────────────────────────────────────────────

#[component]
pub fn MessageArea(props: &MessageAreaProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let view_models = hooks.use_atom(&VIEW_MODELS);
    let acp_state = hooks.use_atom(&crate::kit::atoms::ACP_STATE);
    let todo_atom = hooks.use_atom(&crate::kit::atoms::TODO_ITEMS);
    hooks.use_atom(&LANG_VERSION);
    // 订阅 PALETTE_ATOM：主题切换时触发 MessageArea 重渲染，
    // 配合 palette_key 使 vm_caches 失效，确保 markdown 色值随主题更新。
    let _palette = hooks.use_atom(&PALETTE_ATOM);
    let current_palette_key = palette_markdown_key(&_palette.read());

    let snapshot = view_models.read();
    let todo_items = todo_atom.read().clone();
    let is_loading = acp_state.read().is_loading;

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
        .unwrap_or(props.width as u16)
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

    // ── 文本选区 ──
    let text_sel = hooks.use_state(TextSelection::default);
    let selection_down_pos = hooks.use_state(|| Option::<(usize, u16)>::None);
    let drag_throttle = hooks.use_state(DragThrottle::default);

    // 滚动条 fields state（hook 通过引用读取，避免 borrow 冲突）
    let scrollbar_fields = hooks.use_state(ScrollbarFields::default);
    hooks.use_hook(move || ScrollbarHook {
        fields: scrollbar_fields,
    });
    // 滚动条 thumb 拖拽状态（点击/拖拽事件处理器读写）
    let scrollbar_drag = hooks.use_state(ScrollbarDragState::default);

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
    let item_hashes: Vec<u64> = snapshot.items.iter().map(|vm| vm.content_hash()).collect();
    let items_len = item_hashes.len();
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
            let (lines, copy_button) =
                vm_to_lines_cached(vm, vis_width as usize, &mut slot.markdown_cache, true);
            let lines = Arc::new(lines);
            let (_, wm) = build_wrap_map(&lines, vis_width);
            let visual_rows = wm.last().map(|e| e.visual_end).unwrap_or(0) as u16;
            slot.content_hash = vm_hash;
            slot.width = vis_width;
            slot.palette_key = current_palette_key;
            slot.lang_key = LANG_VERSION.get();
            slot.lines = lines;
            slot.wrap_map = Arc::new(wm);
            slot.visual_rows = visual_rows;
            slot.copy_button = copy_button;
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
    let concat_wrap_map: Vec<WrappedLineInfo> = {
        let caches_read = vm_caches.read();
        slot_arcs.reserve(caches_read.len());
        slot_offsets.reserve(caches_read.len());
        slot_visual_starts.reserve(caches_read.len());
        let mut slots: Vec<(&[WrappedLineInfo], usize, usize)> =
            Vec::with_capacity(caches_read.len());
        for (slot_index, slot) in caches_read.iter().enumerate() {
            let lines_start = total_logical_lines;
            slot_offsets.push(lines_start);
            total_logical_lines += slot.lines.len();
            slot_arcs.push(Arc::clone(&slot.lines));
            slots.push((&slot.wrap_map, lines_start, slot_index));
            slot_visual_starts.push(core_total_visual_rows);
            core_total_visual_rows += slot.visual_rows as usize;
        }
        concat_wrap_maps(&slots)
    };
    let slot_arcs_arc: Arc<Vec<Arc<Vec<Line<'static>>>>> = Arc::new(slot_arcs);
    let slot_offsets_arc: Arc<Vec<usize>> = Arc::new(slot_offsets);
    let concat_wrap_map_arc: Arc<Vec<WrappedLineInfo>> = Arc::new(concat_wrap_map);
    // [PERI_RENDER_TIMING] concat 阶段耗时
    let num_slots = slot_arcs_arc.len();
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
    let total_visual_rows: u16 = if core_total_visual_rows == 0 && footer_visual_rows == 0 {
        if is_loading { 1 } else { 0 }
    } else {
        (core_total_visual_rows + footer_visual_rows)
            .saturating_add(scroll::SCROLL_PADDING)
            .min(u16::MAX as usize) as u16
    };

    // ── keepgoing 按钮点击（footer summary 行右侧）──
    // [Why] 必须注册在 scroll handler 之前：两者同 Global+High，同优先级按注册序分发，
    // scroll::handle_event 对消息区内 Down(Left) 一律 Consumed（文本选中起点）——
    // 若在其后注册，按钮点击会被 scroll handler 截断、永远收不到。
    // 命中 → Consumed（scroll handler 不处理该点击，不会误设选区起点）；
    // 未命中 → Ignored（滚动/选区逻辑照常）。
    // [TRAP] 闭包捕获 State 句柄（keepgoing_rect）而非帧快照——每帧 write_no_update
    // 更新 rect，滚动/布局变化后坐标仍准确；事件在上帧渲染完成后分发，读取的
    // 是最近一帧的按钮位置。
    {
        hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
            let Event::Mouse(mouse) = event else {
                return EventResult::Ignored;
            };
            if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
                return EventResult::Ignored;
            }
            // 弹窗/面板遮挡时不响应（与 status_bar / scroll handler 一致）
            if mouse_router::is_occluded() {
                return EventResult::Ignored;
            }
            // 命中检测：点击坐标落在按钮屏幕区域内 (y, x_start, width)
            // [Why 先于防抖] 防抖期内点击禁用按钮也应 Consumed——否则事件落到
            // scroll handler 的文本选区逻辑（消息区内 Down(Left) 设置选区锚点）。
            let Some((by, bx, bw)) = *keepgoing_rect.read() else {
                return EventResult::Ignored;
            };
            let (x, y) = (mouse.column, mouse.row);
            if y != by || x < bx || x >= bx.saturating_add(bw) {
                return EventResult::Ignored;
            }
            // 防抖：防抖期内按钮渲染为禁用样式，点击被吞掉但不触发提交
            let now = Instant::now();
            let blocked = KEEPGOING_BLOCKED_UNTIL
                .state()
                .read()
                .is_some_and(|until| now < until);
            if blocked {
                return EventResult::Consumed;
            }
            // 触发 keepgoing 提交：发送空白 user prompt（服务端不插入 user 消息，仅继续 loop）
            if let Some(tx) = SUBMIT_TX.get() {
                let _ = tx.send(SubmitRequest::KeepGoing);
            }
            // 防抖：冷却期内按钮禁用（渲染为 muted 样式，见 build_footer_lines）
            *KEEPGOING_BLOCKED_UNTIL.state().write() = Some(now + KEEPGOING_DEBOUNCE);
            // 防抖到期后清除阻塞并 bump 心跳触发重渲染，恢复可点击样式
            tokio::spawn(async move {
                tokio::time::sleep(KEEPGOING_DEBOUNCE).await;
                *KEEPGOING_BLOCKED_UNTIL.state().write() = None;
                RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
            });
            EventResult::Consumed
        });
    }

    // ── md 复制按钮点击（复制整条 AI 回复的原始 markdown）──
    // [Why] 注册顺序与 keepgoing 相同：必须在 scroll handler 之前——scroll::handle_event
    // 对消息区内 Down(Left) 一律 Consumed（文本选中起点），在其后注册收不到点击。
    // 命中 → Consumed（滚动/选区逻辑不处理该点击，不会误设选区起点）；
    // 未命中 → Ignored（滚动/选区逻辑照常）。
    // [Why 每次渲染重建] ratatui-kit 的 use_event_handler 闭包每帧重新注册（当帧值），
    // copy_buttons State 由渲染 body 后部 write_no_update 更新——事件分发时读到的
    // 是最近一帧的按钮位置（与 keepgoing 一致）。
    {
        hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
            let Event::Mouse(mouse) = event else {
                return EventResult::Ignored;
            };
            if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
                return EventResult::Ignored;
            }
            // 弹窗/面板遮挡时不响应（与 status_bar / scroll handler 一致）
            if mouse_router::is_occluded() {
                return EventResult::Ignored;
            }
            let (x, y) = (mouse.column, mouse.row);
            let hits = copy_buttons.read();
            let Some(hit) = hits
                .iter()
                .find(|h| y == h.row && x >= h.x_start && x < h.x_end)
            else {
                return EventResult::Ignored;
            };
            // 读取最新 VM 文本：校验 content_hash 防 Rewind/Reset 后索引错位
            let snapshot = view_models.read();
            let matched = snapshot
                .items
                .get(hit.slot_index)
                .is_some_and(|vm| vm.content_hash() == hit.vm_hash);
            let text = if matched {
                match &snapshot.items[hit.slot_index] {
                    TuiRenderUnit::TuiAssistantBubble(d) => Some(d.text.clone()),
                    _ => None,
                }
            } else {
                None
            };
            drop(snapshot);
            if let Some(text) = text {
                copy_to_clipboard(text.clone());
                mark_copy_message(text.chars().count());
            }
            // 命中按钮（即使 VM 不匹配）也 Consumed——防止点击落到文本选区逻辑
            EventResult::Consumed
        });
    }

    // ── 鼠标事件处理（滚动 + 文本拖拽选中复制）──
    // [TRAP] event_handler 闭包必须是 'static → 必须 move。但 concat_wrap_map_arc /
    // slot_arcs_arc / slot_offsets_arc 后续视口裁剪也要用。Arc::clone 是 O(1) 引用计数，
    // ── 吸底自动跟随 ──
    let last_scrolled_at = hooks.use_state(|| 0u16);
    // 粘性吸底开关：默认跟随；用户向上滚动即退出（浏览模式），滚回底部才恢复。
    let follow_bottom = hooks.use_state(|| true);

    // 闭包持 clone，原值继续在 render body 内用。
    // [Why 位置] 必须声明在 follow_bottom 之后（闭包捕获），且所有 hook 每次渲染
    // 以相同相对顺序调用——use_event_handler 只是占顺序槽，位置调整无状态错位。
    {
        let wrap_map_for_closure = Arc::clone(&concat_wrap_map_arc);
        let slot_arcs_for_closure = Arc::clone(&slot_arcs_arc);
        let slot_offsets_for_closure = Arc::clone(&slot_offsets_arc);
        hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
            scroll::handle_event(
                &event,
                area_rect,
                vis_width,
                &scroll_state,
                &scroll_throttle,
                &text_sel,
                &selection_down_pos,
                &drag_throttle,
                &wrap_map_for_closure,
                &slot_arcs_for_closure,
                &slot_offsets_for_closure,
                &scrollbar_fields,
                &scrollbar_drag,
                &follow_bottom,
            )
        });
    }

    let prev_total_visual_rows = hooks.use_state(|| 0u16);
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
        if let Some(lines) = brewed_lines {
            return element!(
                View(
                    flex_direction: Direction::Vertical,
                    width: Constraint::Fill(1),
                    height: Constraint::Fill(1),
                ) {
                    View(height: Constraint::Fill(1)) {
                        Welcome(width: props.width)
                    }
                    Text(text: Paragraph::new(RatText::from(lines)).wrap(Wrap { trim: false }))
                }
            )
            .into_any();
        }
        return element!(
            View(width: Constraint::Fill(1), height: Constraint::Fill(1)) {
                Welcome(width: props.width)
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
    let max_scroll = (total_visual_rows as usize).saturating_sub(vis_height as usize);
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
        g.content_length = total_visual_rows as usize;
        g.position = (scroll_y * (total_visual_rows as usize - 1))
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

    // ── md 复制按钮屏幕位置（每帧更新）──
    // 按钮行在 slot 内是唯一视觉行（copy_button_line 已保证不折行），
    // 屏幕行 = area.y + slot 视觉偏移 + 行内视觉偏移 - scroll_y。
    // 视口外的按钮不进入映射（点不到），避免列表随会话增长。
    {
        let mut hits: Vec<CopyButtonHit> = Vec::new();
        if let Some(area) = area_rect {
            let vp_end = area.y.saturating_add(vis_height);
            let caches_read = vm_caches.read();
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
                hits.push(CopyButtonHit {
                    row: row as u16,
                    x_start: area.x.saturating_add(btn.x_start),
                    x_end: area.x.saturating_add(btn.x_end),
                    slot_index,
                    vm_hash: slot.content_hash,
                });
            }
        }
        *copy_buttons.write_no_update() = Arc::new(hits);
    }

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
                if in_sel {
                    let (_, _, sr, sc, er, ec) = sel_bounds.unwrap();
                    let highlighted =
                        highlight_line_in_selection(line, entry, sr, er, sc, ec, vis_width, sel_bg);
                    viewport_lines.push(highlighted);
                } else {
                    viewport_lines.push(line.clone());
                }
            }
        }
    }

    if viewport_has_footer {
        viewport_lines.extend(footer_lines.iter().cloned());
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

/// 计算 keepgoing 按钮的屏幕点击区域 `(y, x_start, width)`。
///
/// 渲染布局：core 行（`core_total_visual_rows` 行）→ footer_lines（`line_index` 行）
/// → padding 行。footer_lines[line_index] 的屏幕 y = area.y + core_total_visual_rows
/// + line_index - scroll_y（i64 数学避免 u16 下溢）。
///
/// 返回 None 的情形：
/// - empty：Welcome 布局（footer 渲染在 Welcome 之下，行位置模型不同）——按钮可见但不可点击
/// - 按钮被滚出视口（y 不在 [area.y, area.y + vis_height) 内）
fn compute_keepgoing_rect(
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

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
