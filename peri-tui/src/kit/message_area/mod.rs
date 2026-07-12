//! MessageArea：直接读取 VIEW_MODELS，通过 vm_to_lines 将 TuiRenderUnit
//! 转换为 Vec<Line>，按视口裁剪后渲染。
//!
//! - 滚动：由 ScrollViewState 处理键盘/鼠标事件（offset 管理）
//! - 渲染：视口裁剪——只 clone + highlight + 渲染视口内 ~60 行，避免 O(N×W) per render
//! - 智能跟随：use_effect 检测 VIEW_MODELS 变化
//! - 不再使用 RENDER_CACHE / render_bridge / ScrollView / wrap_map（已替换为 wrap_map_cache）

#![allow(clippy::needless_update)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::kit::atoms::{LANG_VERSION, VIEW_MODELS};
use crate::kit::focus_router;
use crate::kit::text_selection::TextSelection;
use crate::kit::welcome::Welcome;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    components::ScrollViewState,
    crossterm::event::{Event, KeyEventKind, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        text::{Line, Span, Text as RatText},
        widgets::{Paragraph, Wrap},
    },
};

mod footer;
mod props;
mod render;
mod selection;
pub use footer::{TodoItem, TodoStatus};
use footer::{build_footer_lines, hash_todo_items};
pub use props::MessageAreaProps;
use props::{MsgAreaTracker, ScrollbarFields, ScrollbarHook, mouse_in_area};
use render::vm_to_lines;
use selection::{
    WrappedLineInfo, build_wrap_map, copy_to_clipboard, extract_visual_range, mark_copy_message,
    viewport_logical_range, visual_to_logical,
};

// ── 滚动速度控制 ──────────────────────────────────────────────────────────

/// 鼠标滚轮每格的滚动行数倍数。
const SCROLL_LINES: u16 = 3;

/// 滚动节流窗口：≥16ms（≈60fps）才把累积 delta 推入 scroll_state。
const SCROLL_FRAME_MS: u64 = 16;

#[derive(Debug, Clone)]
struct ScrollThrottle {
    last_flush: Instant,
    pending_delta: i32, // positive = scroll_down, negative = scroll_up
}

impl Default for ScrollThrottle {
    fn default() -> Self {
        Self {
            last_flush: Instant::now(),
            pending_delta: 0,
        }
    }
}

// ── 拖拽选中节流 ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct DragThrottle {
    last_flush: Instant,
}

impl Default for DragThrottle {
    fn default() -> Self {
        Self {
            last_flush: Instant::now(),
        }
    }
}

// ── 组件 ──────────────────────────────────────────────────────────────────

#[component]
pub fn MessageArea(props: &MessageAreaProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let view_models = hooks.use_atom(&VIEW_MODELS);
    let acp_state = hooks.use_atom(&crate::kit::atoms::ACP_STATE);
    let todo_atom = hooks.use_atom(&crate::kit::atoms::TODO_ITEMS);
    hooks.use_atom(&LANG_VERSION);

    let snapshot = view_models.read();
    let todo_items = todo_atom.read().clone();
    let is_loading = acp_state.read().is_loading;

    let items_len = snapshot.items.len();
    let vm_generation = snapshot.generation;

    // ── 渲染缓存：generation 不变则复用上次的 Lines，避免每帧做 markdown 解析+syntect ──
    // [TRAP] 缓存必须用 Arc<Vec> 而非 Vec：ratatui-kit 每次 dispatch 后都触发 render，
    // 鼠标 Drag 事件 60-120Hz 会反复读取此缓存。Vec 在每次读取时深拷贝 O(N)（每行多个
    // Span + Cow<str>），直接拖满 CPU。Arc::clone 是 O(1) 引用计数。
    let lines_cache = hooks.use_state(|| (0u64, 0usize, Arc::<Vec<Line<'static>>>::default()));

    // ── total_visual_rows 缓存：仅 (generation, width, lines_len) 变化时重算 line_count ──
    // [TRAP] Paragraph::line_count 是 O(N·W)（unicode-width + wrap），每帧重算会拖垮长对话滚动。
    // cache 仅供 render body 读，不作为响应式源——用 write_no_update 写入避免 wake 自激回路。
    let total_rows_cache = hooks.use_state(|| (0u64, 0u16, 0usize, 0u16));

    // ── Footer 行预计算：必须在 empty 分支之前调用，确保所有 hook 顺序一致 ──
    let footer_lines = build_footer_lines(&mut hooks, is_loading, &todo_items);

    let empty = snapshot.items.is_empty() && !is_loading && todo_items.is_empty();
    let brewed_lines = if empty && !footer_lines.is_empty() {
        Some(footer_lines.clone())
    } else {
        None
    };

    // ── 构建 core_lines（带 Arc 缓存，footer 不参与缓存）──
    // [TRAP] 缓存 key 不能加 `lines.is_empty()` 之类的"空内容"判断——
    // Welcome 屏 items 为空时 vm_to_lines 永远返回空 Vec，写入后再读到
    // is_empty()=true，needs_rebuild 永远为 true，每帧都执行
    // `*lines_cache.write() = ...`。ratatui-kit 的 ReactiveMutRef::Drop 无条件
    // notifier.wake()（不检查值是否变化），wake 又触发 re-render → 自激回路
    // 100% CPU。空内容必须视为有效缓存，靠 generation/width 检测真实变化。
    //
    // [TRAP] core_lines_arc 用 Arc<Vec> —— Drag 60-120Hz 触发 render 时，
    // Arc::clone 是 O(1)；如果用 Vec::clone 则每帧深拷贝数千行 Line+Span，
    // 直接拖满 CPU。footer 后续单独 extend，避免 Arc 解引用后再 clone。
    let core_lines_arc: Arc<Vec<Line<'static>>> = {
        let needs_rebuild = {
            let guard = lines_cache.read();
            guard.0 != vm_generation || guard.1 != props.width
        };
        if !needs_rebuild {
            Arc::clone(&lines_cache.read().2)
        } else {
            let mut lines: Vec<Line<'static>> = Vec::new();
            for item in snapshot.items.iter() {
                lines.extend(vm_to_lines(item, props.width));
            }
            drop(snapshot);
            let arc = Arc::new(lines);
            *lines_cache.write() = (vm_generation, props.width, Arc::clone(&arc));
            arc
        }
    };

    // all_lines 仅在需要时构建（lazy）：
    // - wrap_map_cache 缓存未命中（generation/width 变化）
    // - total_visual_rows 缓存未命中
    // - 非 highlight 渲染路径（render_lines = all_lines）
    // [TRAP] Drag 期间 highlight 路径下，wrap_map / total_visual_rows 都已命中缓存，
    // 实际不需要 all_lines。每次构建需要 (*core_lines_arc).clone() O(N)——Drag 60-120Hz
    // × O(N) 直接拉满 CPU。我们改为只在真正用到时构建。
    let core_len = core_lines_arc.len();
    let footer_len = if empty { 0 } else { footer_lines.len() };
    let lines_len = core_len + footer_len;

    let scroll_state = hooks.use_state(ScrollViewState::default);
    let prev_items_len = hooks.use_state(|| 0usize);
    let _prev_is_loading = hooks.use_state(|| false);
    let scroll_throttle = hooks.use_state(ScrollThrottle::default);
    let _todo_hash = hash_todo_items(&todo_items);

    // ── 文本选区 + 折行映射缓存 ──
    let text_sel = hooks.use_state(TextSelection::default);
    let selection_down_pos = hooks.use_state(|| Option::<(u16, u16)>::None);
    let drag_throttle = hooks.use_state(DragThrottle::default);
    // [TRAP] Drag 60-120Hz 触发 render，wrap_map 必须用 Arc 避免 Vec 深拷贝。
    // highlight 不再缓存——视口裁剪后只有 ~60 行，highlight 成本可忽略。
    let wrap_map_cache = hooks.use_state(|| (0u64, 0u16, Arc::<Vec<WrappedLineInfo>>::default()));

    // ── 消息区位置追踪 ──
    let area_hook = hooks.use_hook(MsgAreaTracker::new);
    let area_rect = area_hook.rect;
    // 滚动条 fields state（hook 通过引用读取，避免 borrow 冲突）
    let scrollbar_fields = hooks.use_state(ScrollbarFields::default);
    hooks.use_hook(move || ScrollbarHook {
        fields: scrollbar_fields,
    });

    let vis_width = area_rect
        .map(|r| r.width.saturating_sub(1))
        .unwrap_or(props.width as u16)
        .max(1);
    let vis_height = area_rect.map(|r| r.height).unwrap_or(60).max(1);

    // 更新 wrap_map 缓存（仅 generation / width 变化时，write_no_update 避免自激回路）
    // [TRAP] wrap_map 只覆盖 core_lines_arc——footer 区域（spinner/todo）不需要选区，
    // 鼠标拖拽到 footer 时 visual_to_logical 返回 None，不触发 highlight。
    {
        let needs_wmap = {
            let g = wrap_map_cache.read();
            g.0 != vm_generation || g.1 != vis_width
        };
        if needs_wmap {
            if core_lines_arc.is_empty() {
                // 空内容：直接设空缓存，不调用 build_wrap_map
                let mut g = wrap_map_cache.write_no_update();
                g.0 = vm_generation;
                g.1 = vis_width;
                g.2 = Arc::default();
            } else {
                let (_, wrap_map) = build_wrap_map(&core_lines_arc, vis_width);
                let mut g = wrap_map_cache.write_no_update();
                g.0 = vm_generation;
                g.1 = vis_width;
                g.2 = Arc::new(wrap_map);
            }
        }
    }

    // ── 总视觉行数：使用 Paragraph wrap 预测（带缓存）──
    // [TRAP] 仅在 (gen, width, lines_len) 变化时构建 all_lines 重算 line_count。
    // Drag 期间 generation/width/lines_len 不变，缓存命中——跳过 O(N) 构建。
    let cached = {
        let g = total_rows_cache.read();
        (g.0, g.1, g.2, g.3)
    };
    let total_visual_rows: u16 =
        if cached.0 == vm_generation && cached.1 == vis_width && cached.2 == lines_len {
            cached.3
        } else if lines_len == 0 {
            let rows: u16 = if is_loading { 1 } else { 0 };
            let mut g = total_rows_cache.write_no_update();
            g.0 = vm_generation;
            g.1 = vis_width;
            g.2 = lines_len;
            g.3 = rows;
            rows
        } else {
            // 构建 all_lines 用于 line_count（仅在 cache 未命中时）
            let mut all_lines = (*core_lines_arc).clone();
            if !empty {
                all_lines.extend(footer_lines.iter().cloned());
            }
            let rows = Paragraph::new(RatText::from(all_lines))
                .wrap(Wrap { trim: false })
                .line_count(vis_width as u16) as u16;
            let mut g = total_rows_cache.write_no_update();
            g.0 = vm_generation;
            g.1 = vis_width;
            g.2 = lines_len;
            g.3 = rows;
            rows
        };

    // ── 鼠标事件处理（滚动 + 文本拖拽选中复制）──
    {
        hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
            if let Event::Key(key) = &event {
                let _ = focus_router::message_accepts_key(key);
            }

            if let Event::Mouse(mouse) = &event {
                // 光标移动无操作——提前返回，不触发任何 state 写入或渲染
                if matches!(mouse.kind, MouseEventKind::Moved) {
                    return EventResult::Ignored;
                }

                // 滚动节流
                let apply_scroll = |delta: i32| {
                    let mut st = scroll_throttle.write_no_update();
                    st.pending_delta += delta;
                    let now = Instant::now();
                    if now.duration_since(st.last_flush) >= Duration::from_millis(SCROLL_FRAME_MS) {
                        let pending = st.pending_delta;
                        st.pending_delta = 0;
                        st.last_flush = now;
                        drop(st);
                        if pending != 0 {
                            let mut state = scroll_state.write_no_update();
                            if pending > 0 {
                                for _ in 0..(pending as u16) {
                                    state.scroll_down();
                                }
                            } else {
                                for _ in 0..((-pending) as u16) {
                                    state.scroll_up();
                                }
                            }
                        }
                    }
                };

                if let Some(area) = area_rect {
                    let in_area = mouse_in_area(mouse.row, mouse.column, area);

                    // [DEBUG] PERI_DISABLE_DRAG_SELECT=1 完全跳过 Drag 选中——验证是否是
                    // Drag 处理逻辑引起卡死。
                    let drag_select_disabled = std::env::var("PERI_DISABLE_DRAG_SELECT")
                        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                        .unwrap_or(false);

                    // ── 文本选中处理（消息区内 Down/Drag/Up）──
                    if in_area && !drag_select_disabled {
                        match mouse.kind {
                            MouseEventKind::Down(MouseButton::Left) => {
                                // 仅记录按下的位置，不启动选区——真实拖动才开始选中
                                // [TRAP] selection_down_pos 只在事件处理器内读写，render
                                // 不依赖它——用 write_no_update 避免 wake 噪音（render 不需要
                                // 因为这个状态变化而重渲染，后续 Drag 才是真正的渲染触发点）。
                                let scroll_y = scroll_state.read().offset().y as u16;
                                let visual_row =
                                    mouse.row.saturating_sub(area.y).saturating_add(scroll_y);
                                // 视口裁剪后无边框，visual_col 直接 = mouse.column - area.x
                                let visual_col = mouse.column.saturating_sub(area.x);
                                *selection_down_pos.write_no_update() =
                                    Some((visual_row, visual_col));
                                return EventResult::Consumed;
                            }
                            MouseEventKind::Drag(MouseButton::Left) => {
                                // Drag 节流（16ms），write_no_update 避免自激回路
                                let now = Instant::now();
                                {
                                    let dt = drag_throttle.read();
                                    if dt.last_flush.elapsed()
                                        < Duration::from_millis(SCROLL_FRAME_MS)
                                    {
                                        return EventResult::Consumed;
                                    }
                                }
                                drag_throttle.write_no_update().last_flush = now;

                                let scroll_y = scroll_state.read().offset().y as u16;
                                let visual_row =
                                    mouse.row.saturating_sub(area.y).saturating_add(scroll_y);
                                let visual_col = mouse.column.saturating_sub(area.x);

                                // 单次 write guard，drop 时只 wake 一次（不是两次）
                                // start_drag + update_drag 合并到同一 guard 内
                                //
                                // [TRAP] ratatui-kit 用 parking_lot::RwLock——同一 thread 同时
                                // 持有 read + write 时 try_write 返回 Err → expect panic。
                                // 必须先把 selection_down_pos.read() 的值 copy 出来 drop guard，
                                // 再 write selection_down_pos。
                                let down_pos = *selection_down_pos.read();
                                {
                                    let mut sel_guard = text_sel.write();
                                    if let Some((dr, dc)) = down_pos {
                                        sel_guard.start_drag(dr, dc);
                                        *selection_down_pos.write_no_update() = None;
                                    }
                                    sel_guard.update_drag(visual_row, visual_col);
                                }
                                return EventResult::Consumed;
                            }
                            MouseEventKind::Up(MouseButton::Left) => {
                                *selection_down_pos.write_no_update() = None;
                                // [TRAP] 同 Drag 处理：必须 copy 出 text_sel 状态后再 write，
                                // 否则 read+write 同 thread 冲突 panic。
                                let dragging = text_sel.read().dragging;
                                if !dragging {
                                    return EventResult::Consumed;
                                }
                                // 先 copy 出 normalized_bounds（owned Option），drop read guard
                                let bounds = text_sel.read().normalized_bounds();
                                let extracted: Option<String> =
                                    if let Some(((sr, sc), (er, ec))) = bounds {
                                        let wrap_guard = wrap_map_cache.read();
                                        let lines_guard = lines_cache.read();
                                        extract_visual_range(
                                            &lines_guard.2,
                                            &wrap_guard.2,
                                            (sr, sc),
                                            (er, ec),
                                            vis_width,
                                        )
                                    } else {
                                        None
                                    };

                                // 清除选区（start/end/dragging 全清），wake 触发重渲染清除 highlight
                                {
                                    let mut sel = text_sel.write();
                                    sel.clear();
                                }

                                // 复制（独立线程，不阻塞）
                                if let Some(text) = extracted {
                                    let char_count = text.chars().count();
                                    copy_to_clipboard(text);
                                    mark_copy_message(char_count);
                                }

                                return EventResult::Consumed;
                            }
                            _ => {}
                        }
                    } else {
                        // 鼠标在消息区外
                        match mouse.kind {
                            MouseEventKind::Down(MouseButton::Left) => {
                                // 清除选区和按下记录
                                *text_sel.write() = TextSelection::new();
                                *selection_down_pos.write_no_update() = None;
                                return EventResult::Ignored;
                            }
                            _ => {
                                if matches!(mouse.kind, MouseEventKind::Drag(_)) {
                                    return EventResult::Ignored;
                                }
                            }
                        }
                    }

                    // ── 滚动处理（区域内外通用）──
                    match mouse.kind {
                        MouseEventKind::ScrollDown => apply_scroll(SCROLL_LINES as i32),
                        MouseEventKind::ScrollUp => apply_scroll(-(SCROLL_LINES as i32)),
                        _ => {}
                    }
                }

                // 所有非 Moved/Drag 鼠标事件标记为已消费（防止泄漏到下层组件）
                return EventResult::Consumed;
            }

            if let Event::Key(key) = &event {
                if key.kind == KeyEventKind::Press && focus_router::message_accepts_key(key) {
                    scroll_state.write().handle_event(&event);
                    return EventResult::Consumed;
                }
            }
            EventResult::Ignored
        });
    }

    // ── 吸底自动跟随 ──
    let last_scrolled_at = hooks.use_state(|| 0u16);
    hooks.use_effect(
        {
            let st = scroll_state;
            let pl = prev_items_len;
            let lsa = last_scrolled_at;
            let len = items_len;
            let loading = is_loading;
            move || {
                let prev = *pl.read();
                *pl.write() = len;

                if total_visual_rows == 0 || vis_height == 0 {
                    return;
                }

                if loading {
                    // [TRAP] read+write 同 state 同线程 = parking_lot 死锁——先 copy 出来
                    let prev_lsa = *lsa.read();
                    if total_visual_rows > prev_lsa {
                        let max_scroll = total_visual_rows.saturating_sub(vis_height);
                        let scroll_y = st.read().offset().y as u16;
                        if scroll_y < max_scroll {
                            st.write().scroll_to_bottom();
                        }
                        *lsa.write() = total_visual_rows;
                    }
                    return;
                }

                if len < prev {
                    st.write().scroll_to_bottom();
                    *lsa.write() = total_visual_rows;
                    return;
                }

                let max_scroll = total_visual_rows.saturating_sub(vis_height);
                let scroll_y = st.read().offset().y as u16;
                if scroll_y >= max_scroll {
                    return;
                }
                let distance = max_scroll.saturating_sub(scroll_y);
                if distance > (vis_height / 4).max(5) {
                    return;
                }
                let prev_lsa = *lsa.read();
                if total_visual_rows > prev_lsa {
                    st.write().scroll_to_bottom();
                    *lsa.write() = total_visual_rows;
                }
            }
        },
        (items_len, vm_generation, is_loading),
    );

    if empty {
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
    let scroll_y_raw = scroll_state.read().offset().y as usize;
    let scroll_y = scroll_y_raw.min(max_scroll);

    // 更新 scrollbar fields——post_component_draw 时基于此渲染滚动条
    {
        let mut g = scrollbar_fields.write_no_update();
        g.content_length = total_visual_rows as usize;
        g.position = scroll_y;
        g.viewport_length = vis_height as usize;
    }

    let vp_height = vis_height as usize;

    // core 总视觉行数
    let core_total_visual_rows: usize = {
        let g = wrap_map_cache.read();
        g.2.last().map(|e| e.visual_end).unwrap_or(0)
    };

    // 选区对应的逻辑行范围（视口外选区不参与 highlight，selection state 保留供复制）
    let sel_bounds: Option<(usize, usize)> = if !no_highlight {
        let sel = text_sel.read();
        if let Some(((sr, _), (er, _))) = sel.normalized_bounds() {
            let g = wrap_map_cache.read();
            let f = visual_to_logical(sr, &g.2).unwrap_or(0);
            let l = visual_to_logical(er, &g.2).unwrap_or(0);
            Some((f.min(l), f.max(l)))
        } else {
            None
        }
    } else {
        None
    };

    // 视口对应的 core 逻辑行范围 + 首行视觉偏移
    let (vp_core_start, vp_core_end, vp_first_offset): (usize, usize, u16) =
        if scroll_y < core_total_visual_rows && !core_lines_arc.is_empty() {
            let g = wrap_map_cache.read();
            viewport_logical_range(&g.2, scroll_y, vp_height).unwrap_or((0, 0, 0))
        } else {
            // 视口完全在 footer 内（footer 占据末尾几行）
            (0, 0, 0)
        };

    // 视口是否包含 footer（视口末尾超出 core 总视觉行数）
    let viewport_has_footer =
        !empty && !footer_lines.is_empty() && scroll_y + vp_height > core_total_visual_rows;

    // 构建 viewport_lines：clone + highlight 视口内的 core 行，必要时附加 footer
    let sel_bg = THEME_ATOM.state().read().semantic.surface.selection;
    let core_len = core_lines_arc.len();
    let mut viewport_lines: Vec<Line<'static>> = Vec::with_capacity(
        (vp_core_end.saturating_sub(vp_core_start) + 1)
            .min(vp_height + 2)
            .saturating_add(footer_lines.len()),
    );

    if scroll_y < core_total_visual_rows && vp_core_start <= vp_core_end && core_len > 0 {
        let end = vp_core_end.min(core_len - 1);
        for i in vp_core_start..=end {
            let line = &core_lines_arc[i];
            let in_sel = sel_bounds.is_some_and(|(f, l)| i >= f && i <= l);
            if in_sel {
                let spans: Vec<Span<'static>> = line
                    .spans
                    .iter()
                    .map(|s| Span::styled(s.content.clone(), s.style.bg(sel_bg)))
                    .collect();
                viewport_lines.push(Line::from(spans));
            } else {
                viewport_lines.push(line.clone());
            }
        }
    }

    if viewport_has_footer {
        viewport_lines.extend(footer_lines.iter().cloned());
    }

    // Paragraph::scroll 偏移：core 内的偏移 = vp_first_offset
    // 视口完全在 footer 内时（scroll_y >= core_total_visual_rows），按 footer 内偏移
    let scroll_offset_y: u16 = if scroll_y >= core_total_visual_rows && core_total_visual_rows > 0 {
        (scroll_y - core_total_visual_rows) as u16
    } else {
        vp_first_offset
    };

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            Text(text: Paragraph::new(RatText::from(viewport_lines))
                .wrap(Wrap { trim: false })
                .scroll((scroll_offset_y, 0)))
        }
    )
    .into_any()
}

// ── 测试 ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_with_todo_items_shows_footer_not_welcome() {
        let entries_empty = true;
        let is_loading = false;
        let todo_items_empty = false;
        let empty = entries_empty && !is_loading && todo_items_empty;

        assert!(
            !empty,
            "仅有 todo 条目且无消息时不应判定为 empty，避免 Welcome 覆盖 todo 显示"
        );
    }

    #[test]
    fn test_empty_without_todo_is_truly_empty() {
        let entries_empty = true;
        let is_loading = false;
        let todo_items_empty = true;
        let empty = entries_empty && !is_loading && todo_items_empty;

        assert!(empty);
    }

    fn proximity_check(total: u16, scroll_y: u16, vis_height: u16) -> bool {
        if total == 0 {
            return false;
        }
        let max_scroll = total.saturating_sub(vis_height);
        if scroll_y >= max_scroll {
            return false;
        }
        let distance = max_scroll.saturating_sub(scroll_y);
        let threshold = (vis_height / 2).max(5);
        distance <= threshold
    }

    #[test]
    fn test_proximity_at_bottom_should_not_trigger_scroll() {
        let total = 100;
        let vis_height = 20;
        let scroll_y = total - vis_height;
        assert!(!proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_within_half_viewport_should_follow() {
        let total = 100;
        let vis_height = 20;
        let scroll_y = total - vis_height - 10;
        assert!(proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_beyond_half_viewport_should_not_follow() {
        let total = 100;
        let vis_height = 20;
        let scroll_y = total - vis_height - 11;
        assert!(!proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_near_top_should_not_follow() {
        let total = 200;
        let vis_height = 30;
        let scroll_y = 20;
        assert!(!proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_small_viewport_minimum_threshold() {
        let total = 50;
        let vis_height = 6;
        let scroll_y = total - vis_height - 5;
        assert!(proximity_check(total, scroll_y, vis_height));
        let scroll_y = total - vis_height - 6;
        assert!(!proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_empty_content_no_follow() {
        assert!(!proximity_check(0, 0, 20));
    }

    #[test]
    fn test_proximity_content_smaller_than_viewport_at_bottom() {
        let total = 10;
        let vis_height = 30;
        assert!(!proximity_check(total, 0, vis_height));
    }
}
