//! MessageArea：直接读取 VIEW_MODELS，通过 vm_to_lines 将 TuiRenderUnit
//! 转换为 Vec<Line>，按视口裁剪后渲染。
//!
//! - 滚动：由 ScrollViewState 处理键盘/鼠标事件（offset 管理）
//! - 渲染：视口裁剪——只 clone + highlight + 渲染视口内 ~60 行，避免 O(N×W) per render
//! - 智能跟随：use_effect 检测 VIEW_MODELS 变化
//! - 不再使用 RENDER_CACHE / render_bridge / ScrollView / wrap_map（已替换为 wrap_map_cache）

#![allow(clippy::needless_update)]

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::kit::atoms::{LANG_VERSION, VIEW_MODELS};
use crate::kit::text_selection::TextSelection;
use crate::kit::welcome::Welcome;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    components::ScrollViewState,
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
mod scroll;
mod selection;
pub use footer::{TodoItem, TodoStatus};
use footer::{build_footer_lines, hash_todo_items};
pub use props::MessageAreaProps;
use props::{MsgAreaTracker, ScrollbarFields, ScrollbarHook};
use render::vm_to_lines;
use scroll::{DragThrottle, ScrollThrottle, ScrollbarDragState};
use selection::{
    WrappedLineInfo, build_wrap_map, highlight_line_in_selection, viewport_logical_range,
    visual_to_logical,
};

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

    // ── total_visual_rows 缓存：仅 (generation, width, lines_len, footer_hash) 变化时重算 line_count ──
    // [TRAP] Paragraph::line_count 是 O(N·W)（unicode-width + wrap），每帧重算会拖垮长对话滚动。
    // cache 仅供 render body 读，不作为响应式源——用 write_no_update 写入避免 wake 自激回路。
    // [Fix] 新增 footer_hash key：footer 内容变化但行数相同时（如 spinner 文本变长），
    // lines_len 不变但仍需 invalidate 缓存，否则 total_visual_rows 返回旧值导致末尾几行无法到达。
    let total_rows_cache = hooks.use_state(|| (0u64, 0u16, 0usize, 0u64, 0u16));

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
    // 滚动条 thumb 拖拽状态（点击/拖拽事件处理器读写）
    let scrollbar_drag = hooks.use_state(ScrollbarDragState::default);

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
    // [TRAP] 仅在 (gen, width, lines_len, footer_hash) 变化时构建 all_lines 重算 line_count。
    // Drag 期间 generation/width/lines_len/footer_hash 不变，缓存命中——跳过 O(N) 构建。
    //
    // footer_hash：对 footer 文本内容做 hash，捕获 spinner 文本变化（线数不变时也会 vary）。
    let footer_hash = {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for line in &footer_lines {
            for span in &line.spans {
                span.content.hash(&mut hasher);
            }
        }
        hasher.finish()
    };
    let cached = {
        let g = total_rows_cache.read();
        (g.0, g.1, g.2, g.3, g.4)
    };
    let total_visual_rows: u16 = if cached.0 == vm_generation
        && cached.1 == vis_width
        && cached.2 == lines_len
        && cached.3 == footer_hash
    {
        cached.4
    } else if lines_len == 0 {
        let rows: u16 = if is_loading { 1 } else { 0 };
        let mut g = total_rows_cache.write_no_update();
        g.0 = vm_generation;
        g.1 = vis_width;
        g.2 = lines_len;
        g.3 = footer_hash;
        g.4 = rows;
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
        g.3 = footer_hash;
        g.4 = rows;
        rows
    };

    // ── 鼠标事件处理（滚动 + 文本拖拽选中复制）──
    {
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
                &wrap_map_cache,
                &lines_cache,
                &scrollbar_fields,
                &scrollbar_drag,
            )
        });
    }

    // ── 吸底自动跟随 ──
    let last_scrolled_at = hooks.use_state(|| 0u16);
    let prev_total_visual_rows = hooks.use_state(|| 0u16);
    hooks.use_effect(
        {
            move || {
                scroll::run_auto_follow(&scroll::AutoFollowCtx {
                    total_visual_rows,
                    vis_height,
                    scroll_state: scroll_state.clone(),
                    prev_items_len: prev_items_len.clone(),
                    last_scrolled_at: last_scrolled_at.clone(),
                    items_len,
                    is_loading,
                    prev_total_visual_rows: prev_total_visual_rows.clone(),
                })
            }
        },
        (items_len, vm_generation, is_loading, total_visual_rows),
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

    // [Fix] 每帧钳制 scroll_state.offset.y 到 [0, max_scroll]。
    // apply_scroll 的 scroll_down() 无限递增 offset，没有上限感知——用户可以一直
    // 往下滚直到 offset 远超 max_scroll。虽然 scroll_y = raw.min(max_scroll) 让
    // 渲染正确，但 scroll_state 内部 offset 不被重置，往上滚时需要把多余 offset
    // 消耗完（如 offset=100, max_scroll=40 → 需滚 60 次才恢复）。
    // write_no_update 不触发 re-render，避免自激回路。
    if scroll_y_raw > max_scroll {
        scroll_state
            .write_no_update()
            .set_offset(ratatui_kit::ratatui::layout::Position::new(
                0,
                max_scroll as u16,
            ));
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
        g.position = if max_scroll > 0 {
            (scroll_y * (total_visual_rows as usize - 1)) / max_scroll
        } else {
            0
        };
        g.viewport_length = vis_height as usize;
    }

    let vp_height = vis_height as usize;

    // core 总视觉行数
    let core_total_visual_rows: usize = {
        let g = wrap_map_cache.read();
        g.2.last().map(|e| e.visual_end).unwrap_or(0)
    };

    // 选区范围（字符级）：(first_logical, last_logical, sr, sc, er, ec)
    // 视口外选区不参与 highlight，selection state 保留供复制
    // [Why] 字符级高亮——旧版只存 (first_logical, last_logical) 整逻辑行范围，
    // 导致整行背景色覆盖；与字符级复制提取不一致。现在保留完整 (sr, sc, er, ec)，
    // highlight_line_in_selection 用 wrap_byte_starts 算 byte 范围，拆分 spans。
    let sel_bounds: Option<(usize, usize, u16, u16, u16, u16)> = if !no_highlight {
        let sel = text_sel.read();
        if let Some(((sr, sc), (er, ec))) = sel.normalized_bounds() {
            let g = wrap_map_cache.read();
            // Clamp sr/er 到 wrap_map 视觉范围内（footer 区域无 wrap_map）
            let max_visual =
                g.2.last()
                    .map(|e| (e.visual_end.saturating_sub(1)) as u16)
                    .unwrap_or(0);
            let sr_c = sr.min(max_visual);
            let er_c = er.min(max_visual);
            match (visual_to_logical(sr_c, &g.2), visual_to_logical(er_c, &g.2)) {
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
        // 取 wrap_map arc clone 避免循环中重复 read guard
        let wrap_map_arc = wrap_map_cache.read().2.clone();
        for i in vp_core_start..=end {
            let line = &core_lines_arc[i];
            let in_sel = sel_bounds.is_some_and(|(f, l, _, _, _, _)| i >= f && i <= l);
            if in_sel {
                let (_, _, sr, sc, er, ec) = sel_bounds.unwrap();
                if let Some(entry) = wrap_map_arc.get(i) {
                    let highlighted =
                        highlight_line_in_selection(line, entry, sr, er, sc, ec, vis_width, sel_bg);
                    viewport_lines.push(highlighted);
                } else {
                    viewport_lines.push(line.clone());
                }
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
}
