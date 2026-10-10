//! ratatui-kit SubAgentDetailPanel component.
//!
//! §6.7 subagent 详情 pane：Enter 打开 nested transcript 或详情 pane，不把
//! 完整嵌套消息铺入主时间轴。本面板从 VIEW_MODELS 扫描 `TuiSubAgentGroup`
//! （按 `SELECTED_SUBAGENT_ID` 匹配，由消息区焦点分派在 Enter 时写入），
//! 用真实面板宽度构建 `GridSpec`，共享 EntryRenderCache 嵌套渲染子消息。
//! 缓存保留完整内容高度，只为可见范围访问行，不渲染 md 复制按钮。
//!
//! canonical 内容只读；面板内可选择、展开和滚动（滚轮复用面板仲裁）；Esc 单层关闭
//! （栈顶弹栈，不触及其他面板/焦点仲裁）。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{
    BG_DISPLAY, BG_LIVE_DETAIL, BgDisplayEntry, LANG_VERSION, SELECTED_SUBAGENT_ID, VIEW_MODELS,
};
use crate::kit::message_area::grid::GridSpec;
use crate::kit::tui_render_unit::{
    EntryStatus, FoldTarget, TuiRenderUnit, TuiSubAgentGroup, fold_for_status,
};
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Rect},
        style::{Style, Stylize},
        text::{Line, Span},
    },
};

#[path = "subagent_detail_cache.rs"]
mod render_cache;

#[component]
pub fn SubAgentDetailPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let _lang_ver = hooks.use_atom(&LANG_VERSION);
    // 外部滚动状态——面板滚轮仲裁（panel_scroll.rs）驱动，统一 3 行/格 + 节流
    let sv = hooks.use_state(|| 0usize);
    let scrollbar_grab = hooks.use_state(|| None::<usize>);
    let render_cache = hooks.use_state(render_cache::DetailRenderCache::default);
    hooks.use_hook(move || DetailViewportHook {
        cache: render_cache,
        scroll: sv,
        outer: None,
        last_height: 0,
        last_viewport_height: 0,
    });
    let area = hooks.use_previous_size();

    // 选中 subagent：SELECTED_SUBAGENT_ID（消息区焦点分派写入）→ 候选源解析
    let selected_id = SELECTED_SUBAGENT_ID.state().read().clone();
    let vm_store = hooks.use_atom(&VIEW_MODELS);
    let display_store = hooks.use_atom(&BG_DISPLAY);
    let live_store = hooks.use_atom(&BG_LIVE_DETAIL);
    let group = resolve_selected_subagent(
        &vm_store.read(),
        &live_store.read(),
        &display_store.read(),
        selected_id.as_deref(),
    );
    let _ = vm_store;
    let _ = display_store;
    let _ = live_store;

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
        let key = match event {
            Event::Mouse(mouse) => {
                if crate::kit::atoms::POPUP_KIND.state().read().is_some() {
                    return EventResult::Ignored;
                }
                let viewport = detail_viewport(area);
                let scrollbar_col = area.x.saturating_add(area.width).saturating_sub(1);
                let in_rows = mouse.row >= viewport.y
                    && mouse.row < viewport.y.saturating_add(viewport.height);
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left)
                        if in_rows && mouse.column == scrollbar_col =>
                    {
                        let cache = render_cache.read();
                        let current = *sv.read();
                        let geometry = ScrollbarGeometry::new(
                            cache.height(),
                            viewport.height as usize,
                            current,
                        );
                        let track_row =
                            mouse.row.saturating_sub(viewport.y).saturating_sub(1) as usize;
                        let grab = geometry.grab_offset(track_row);
                        *scrollbar_grab.write() = Some(grab);
                        if !geometry.contains_thumb(track_row) {
                            *sv.write() = geometry.offset_for(track_row, grab);
                        }
                        return EventResult::Consumed;
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        if let Some(grab) = *scrollbar_grab.read() {
                            let cache = render_cache.read();
                            let geometry = ScrollbarGeometry::new(
                                cache.height(),
                                viewport.height as usize,
                                *sv.read(),
                            );
                            let track_row =
                                mouse.row.saturating_sub(viewport.y.saturating_add(1)) as usize;
                            *sv.write() = geometry.offset_for(track_row, grab);
                            return EventResult::Consumed;
                        }
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        let was_dragging = scrollbar_grab.read().is_some();
                        if was_dragging {
                            *scrollbar_grab.write() = None;
                            return EventResult::Consumed;
                        }
                    }
                    MouseEventKind::Down(MouseButton::Left)
                        if in_rows
                            && mouse.column >= viewport.x
                            && mouse.column < scrollbar_col =>
                    {
                        let row = (*sv.read())
                            .saturating_add(mouse.row.saturating_sub(viewport.y) as usize);
                        render_cache.write().click_fold_header(row);
                        return EventResult::Consumed;
                    }
                    _ => {}
                }
                return EventResult::Ignored;
            }
            Event::Key(key) => key,
            _ => return EventResult::Ignored,
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }
        // Esc 单层关闭（面板打开时 active_layer=Panel，消息区 Esc 分支放行，
        // 这里栈顶弹栈收尾；嵌套消息不参与消息区焦点仲裁）。
        match key.code {
            KeyCode::Esc => close_panel(),
            KeyCode::Tab => {
                if let Some(row) = render_cache.write().focus_next(false) {
                    focus_detail_row(sv, row, area.height.saturating_sub(2) as usize);
                }
            }
            KeyCode::BackTab => {
                if let Some(row) = render_cache.write().focus_next(true) {
                    focus_detail_row(sv, row, area.height.saturating_sub(2) as usize);
                }
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
                if let Some(row) = render_cache.write().focus_next(false) {
                    focus_detail_row(sv, row, area.height.saturating_sub(2) as usize);
                }
            }
            KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => {
                if let Some(row) = render_cache.write().focus_next(true) {
                    focus_detail_row(sv, row, area.height.saturating_sub(2) as usize);
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let mut cache = render_cache.write();
                if cache.focused().is_none()
                    && let Some(row) = cache.focus_next(false)
                {
                    focus_detail_row(sv, row, area.height.saturating_sub(2) as usize);
                }
                cache.toggle_focused(key.code == KeyCode::Char(' '));
            }
            KeyCode::Up => {
                let next = sv.read().saturating_sub(1);
                *sv.write() = next;
            }
            KeyCode::Down => {
                let next = sv.read().saturating_add(1);
                *sv.write() = next;
            }
            KeyCode::PageUp => {
                let next = sv.read().saturating_sub(area.height as usize);
                *sv.write() = next;
            }
            KeyCode::PageDown => {
                let next = sv.read().saturating_add(area.height as usize);
                *sv.write() = next;
            }
            KeyCode::Home => *sv.write() = 0,
            KeyCode::End => *sv.write() = render_cache.read().height(),
            _ => {}
        }
        EventResult::Consumed
    });

    // 面板绘制区域（上一帧）——嵌套渲染 wrap 宽度 + 滚轮仲裁
    let grid = GridSpec::grid_for(area.width.max(2));

    // 嵌套渲染：组内 view_models → vm_to_lines_cached（复用统一水平网格与
    // markdown 增量缓存；md 复制按钮关闭——嵌套内容不提供复制按钮）。
    let mut lines: Vec<Line<'static>> = Vec::new();
    match &group {
        Some(g) => {
            lines.push(Line::from(vec![Span::styled(
                crate::truncate::truncate_by_width(
                    &g.agent_name,
                    area.width.saturating_sub(1) as usize,
                ),
                Style::new()
                    .fg(theme_def.read().component.panel.title)
                    .bold(),
            )]));
            lines.push(Line::from(vec![Span::styled(
                crate::truncate::truncate_by_width(
                    &format!("  [{}]", g.agent_id),
                    area.width.saturating_sub(1) as usize,
                ),
                Style::new().fg(theme_def.read().semantic.text.dim),
            )]));
            lines.push(Line::from(""));
            let theme = theme_def.read().clone();
            let mut cache = render_cache.write_no_update();
            if cache.prepare(g, &grid, theme, LANG_VERSION.get(), lines) {
                *sv.write_no_update() = initial_detail_offset(
                    g.is_running,
                    cache.height(),
                    area.height.saturating_sub(2) as usize,
                );
            }
        }
        None => {
            lines.push(Line::from(vec![Span::styled(
                i18n::tr("subagent-detail-not-found"),
                Style::new().fg(theme_def.read().semantic.text.muted),
            )]));
            render_cache.write_no_update().clear(lines);
        }
    }

    crate::kit::panel_scroll::register_virtual_panel_scroll(
        PanelKind::SubAgentDetail,
        area,
        sv,
        render_cache.read().height(),
        area.height.saturating_sub(2) as usize,
    );

    panel_shell!(PanelKind::SubAgentDetail, {
        View(
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        )
    })
}

struct DetailViewportHook {
    cache: State<render_cache::DetailRenderCache>,
    scroll: State<usize>,
    outer: Option<Rect>,
    last_height: usize,
    last_viewport_height: usize,
}

impl Hook for DetailViewportHook {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        // Border changes drawer.area while drawing its child. Capture the panel
        // bounds before that happens so the viewport uses one border inset.
        self.outer = Some(drawer.area);
    }

    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        let Some(outer) = self.outer else {
            return;
        };
        let viewport = detail_viewport(outer);
        if viewport.is_empty() {
            return;
        }
        let mut cache = self.cache.write_no_update();
        let height = cache.height();
        let mut scroll = self.scroll.write_no_update();
        let top = follow_detail_offset(
            *scroll,
            self.last_height,
            self.last_viewport_height,
            height,
            viewport.height as usize,
            cache.source_changed(),
        );
        *scroll = top;
        self.last_height = height;
        self.last_viewport_height = viewport.height as usize;
        cache.draw(drawer, viewport, top);
        crate::kit::scrollbar_chrome::draw_scrollbar(
            drawer,
            Rect {
                width: outer.width,
                ..viewport
            },
            height,
            top,
            viewport.height as usize,
        );
    }
}

fn clamp_detail_offset(top: usize, height: usize, viewport_height: usize) -> usize {
    top.min(height.saturating_sub(viewport_height))
}

fn focus_detail_row(scroll: State<usize>, row: usize, viewport: usize) {
    let current = *scroll.read();
    let next = if row < current {
        row
    } else if row >= current.saturating_add(viewport) {
        row.saturating_add(1).saturating_sub(viewport)
    } else {
        current
    };
    if next != current {
        *scroll.write() = next;
    }
}

fn initial_detail_offset(running: bool, height: usize, viewport: usize) -> usize {
    if running {
        height.saturating_sub(viewport)
    } else {
        0
    }
}

#[derive(Debug, Clone, Copy)]
struct ScrollbarGeometry {
    track: usize,
    thumb: usize,
    thumb_start: usize,
    max_scroll: usize,
}

impl ScrollbarGeometry {
    fn new(content: usize, viewport: usize, offset: usize) -> Self {
        let track = viewport.saturating_sub(2);
        let max_scroll = content.saturating_sub(viewport);
        if track == 0 || max_scroll == 0 {
            return Self {
                track,
                thumb: track,
                thumb_start: 0,
                max_scroll,
            };
        }
        // Keep the thumb hit area in step with ratatui's Scrollbar::part_lengths.
        let denom = content.saturating_sub(1).saturating_add(viewport);
        let thumb = round_ratio(viewport, track, denom).clamp(1, track);
        let thumb_start = round_ratio(offset.min(content.saturating_sub(1)), track, denom)
            .min(track.saturating_sub(thumb));
        Self {
            track,
            thumb,
            thumb_start,
            max_scroll,
        }
    }

    fn grab_offset(self, row: usize) -> usize {
        if self.contains_thumb(row) {
            row - self.thumb_start
        } else {
            self.thumb / 2
        }
    }

    fn contains_thumb(self, row: usize) -> bool {
        row >= self.thumb_start && row < self.thumb_start.saturating_add(self.thumb)
    }

    fn offset_for(self, row: usize, grab: usize) -> usize {
        let travel = self.track.saturating_sub(self.thumb);
        if travel == 0 {
            return 0;
        }
        let start = row
            .min(self.track.saturating_sub(1))
            .saturating_sub(grab)
            .min(travel);
        round_ratio(start, self.max_scroll, travel).min(self.max_scroll)
    }
}

fn round_ratio(a: usize, b: usize, divisor: usize) -> usize {
    if divisor == 0 {
        return 0;
    }
    (((a as u128).saturating_mul(b as u128) + (divisor as u128 / 2)) / divisor as u128)
        .min(usize::MAX as u128) as usize
}

fn follow_detail_offset(
    top: usize,
    previous_height: usize,
    previous_viewport: usize,
    height: usize,
    viewport: usize,
    source_changed: bool,
) -> usize {
    if source_changed
        && previous_height > 0
        && height > previous_height
        && top >= previous_height.saturating_sub(previous_viewport)
    {
        height.saturating_sub(viewport)
    } else {
        clamp_detail_offset(top, height, viewport)
    }
}

fn detail_viewport(outer: Rect) -> Rect {
    Rect {
        y: outer.y.saturating_add(1),
        height: outer.height.saturating_sub(2),
        width: outer.width.saturating_sub(1),
        ..outer
    }
}

#[cfg(test)]
fn scroll_content_height(line_count: usize) -> u16 {
    line_count.clamp(1, u16::MAX as usize) as u16
}

/// 解析选中 subagent 的渲染源。
///
/// 两个候选源：VIEW_MODELS 扫描（组的权威投影）与 `BG_LIVE_DETAIL`（某次 bg
/// 运行的权威投影——bg 的工具事件只进这里，且组在 turn 边界被归档冻结）。选中项
/// 能对上某次 bg 运行时以 live 为准，冻结的组只作回退。
fn resolve_selected_subagent(
    vm_snapshot: &crate::kit::atoms::ViewModelsSnapshot,
    live: &std::collections::HashMap<String, crate::kit::atoms::BgLiveDetail>,
    display: &[BgDisplayEntry],
    selected_id: Option<&str>,
) -> Option<TuiSubAgentGroup> {
    find_live_detail_subagent(live, display, selected_id)
        .or_else(|| find_selected_subagent(vm_snapshot, selected_id))
}

/// 从 VIEW_MODELS 快照扫描 `TuiSubAgentGroup`，按 instance_id / agent_id 匹配选中项。
/// 扫描口径与 agent.rs `collect_subagents` 一致（含折叠组内嵌套递归）。
fn find_selected_subagent(
    snap: &crate::kit::atoms::ViewModelsSnapshot,
    selected_id: Option<&str>,
) -> Option<TuiSubAgentGroup> {
    let selected_id = selected_id?;
    for vm in snap.items.iter() {
        if let Some(g) = scan_vm_for_subagent(vm, selected_id) {
            return Some(g);
        }
    }
    None
}

/// 递归扫描单个 VM（含 TuiCollapsedGroup 内层与嵌套 SubAgent 内层）。
fn scan_vm_for_subagent(vm: &TuiRenderUnit, selected_id: &str) -> Option<TuiSubAgentGroup> {
    match vm {
        TuiRenderUnit::TuiSubAgentGroup(g) => {
            if g.instance_id == selected_id || g.agent_id == selected_id {
                Some(g.clone())
            } else {
                // 嵌套 SubAgent 内层也扫描（agent.rs 同口径）
                g.view_models
                    .iter()
                    .find_map(|inner| scan_vm_for_subagent(inner, selected_id))
            }
        }
        TuiRenderUnit::TuiCollapsedGroup(g) => g
            .view_models
            .iter()
            .find_map(|inner| scan_vm_for_subagent(inner, selected_id)),
        _ => None,
    }
}

fn find_live_detail_subagent(
    live: &std::collections::HashMap<String, crate::kit::atoms::BgLiveDetail>,
    display: &[BgDisplayEntry],
    selected_id: Option<&str>,
) -> Option<TuiSubAgentGroup> {
    let selected_id = selected_id?;
    let task_id = live_task_id_for(live, display, selected_id)?;
    let detail = live.get(&task_id)?;
    let status = if detail.subagent_is_error {
        EntryStatus::Error
    } else if detail.status == crate::kit::atoms::BgLiveStatus::Running {
        EntryStatus::Running
    } else {
        EntryStatus::Completed
    };
    let group = TuiSubAgentGroup {
        instance_id: task_id.to_string(),
        agent_id: detail
            .agent_id
            .clone()
            .unwrap_or_else(|| selected_id.to_string()),
        agent_name: detail
            .agent_name
            .clone()
            .unwrap_or_else(|| detail.summary.clone()),
        view_models: detail.nested_units.clone(),
        collapsed: false,
        is_running: detail.status == crate::kit::atoms::BgLiveStatus::Running,
        is_error: detail.subagent_is_error,
        error_reason: detail.subagent_result.clone(),
        fold: fold_for_status(FoldTarget::SubAgent, status),
        user_modified: false,
        content_hash: 0,
    };
    Some(group)
}

/// 解析选中 id 对应的 `BG_LIVE_DETAIL` task_id。
///
/// 三种来源：底栏行点击的 task_id；旧入口传入的 bg agent_id；消息区 Enter
/// 写入的组 instance_id。后者只认 live 明细记录的同一次运行：resume 复用
/// child_thread_id，按 agent_id 回查会把旧的那次运行当成当前运行。
fn live_task_id_for(
    live: &std::collections::HashMap<String, crate::kit::atoms::BgLiveDetail>,
    display: &[BgDisplayEntry],
    selected_id: &str,
) -> Option<String> {
    if live.contains_key(selected_id) {
        return Some(selected_id.to_string());
    }
    if let Some(entry) = display
        .iter()
        .rev()
        .find(|entry| entry.linked_agent_id.as_deref() == Some(selected_id))
    {
        return Some(entry.id.clone());
    }
    live.iter()
        .find(|(_, detail)| detail.subagent_instance_id.as_deref() == Some(selected_id))
        .map(|(task_id, _)| task_id.clone())
}

fn close_panel() {
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}

#[cfg(test)]
#[path = "subagent_detail_test.rs"]
mod tests;
