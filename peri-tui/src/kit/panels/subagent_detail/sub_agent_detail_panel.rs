//! ratatui-kit SubAgentDetailPanel component.
//!
//! §6.7 subagent 详情 pane：Enter 打开 nested transcript 或详情 pane，不把
//! 完整嵌套消息铺入主时间轴。本面板从 VIEW_MODELS 扫描 `TuiSubAgentGroup`
//! （按 `SELECTED_SUBAGENT_ID` 匹配，由消息区焦点分派在 Enter 时写入），
//! 用 draw 期记录的面板内容区宽度构建 `GridSpec`，共享 EntryRenderCache 嵌套
//! 渲染子消息。缓存保留完整内容高度，只为可见范围访问行，不渲染 md 复制按钮。
//!
//! canonical 内容只读；面板内可选择、展开和滚动（滚轮复用面板仲裁）；Esc 单层关闭
//! （栈顶弹栈，不触及其他面板/焦点仲裁）。

use super::{child_history, render_cache};

use std::sync::Arc;

use crate::app::panel_types::PanelKind;
use crate::i18n;

use crate::kit::message_area::grid::GridSpec;
use crate::kit::message_area::render::{
    status_symbol_and_color, subagent_error_reason, subagent_error_reason_lines,
};
use crate::kit::message_area::viewport_policy;
use crate::kit::tui_render_unit::EntryStatus;
use crate::kit::tui_render_unit::TuiRenderUnit;
use crate::kit::tui_render_unit::TuiSubAgentGroup;
use peri_theme::atoms::THEME_ATOM;
use peri_theme::theme::ThemeDefinition;
use peri_tui_components::scrollbar::{VerticalBar, VerticalPress};
use ratatui_kit::crossterm::event::Event;
use ratatui_kit::crossterm::event::KeyCode;
use ratatui_kit::crossterm::event::KeyEventKind;
use ratatui_kit::crossterm::event::KeyModifiers;
use ratatui_kit::crossterm::event::MouseButton;
use ratatui_kit::crossterm::event::MouseEventKind;
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::Constraint;
use ratatui_kit::ratatui::layout::Rect;
use ratatui_kit::ratatui::style::Style;
use ratatui_kit::ratatui::style::Stylize;
use ratatui_kit::ratatui::text::Line;
use ratatui_kit::ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

/// 身份头（面板自有 chrome）：状态符号 + agent 名称 + agent id；failed 时追加
/// 失败原因行。
///
/// 状态符号复用主时间线组头映射 [`status_symbol_and_color`]，失败原因复用
/// [`subagent_error_reason`] / [`subagent_error_reason_lines`]——面板不维护第二套
/// 映射。`width` 是 Q9 记录的内容区让出滚动条列后的视口宽，`grid` 由同一份内容
/// 区派生（`indent = 0`）；原因行沿用主区组头的缩进/换行预算。
#[must_use]
pub(super) fn detail_header_lines(
    group: &TuiSubAgentGroup,
    grid: &GridSpec,
    width: usize,
    theme: &ThemeDefinition,
) -> Vec<Line<'static>> {
    let (symbol, color) =
        status_symbol_and_color(group.is_running, group.is_error, &theme.semantic);
    let symbol_text = format!("{symbol} ");
    let name = crate::truncate::truncate_to_width(
        &group.agent_name,
        width.saturating_sub(symbol_text.width()).max(1),
    );
    let mut lines = vec![
        Line::from(vec![
            Span::styled(symbol_text, Style::default().fg(color)),
            Span::styled(name, Style::new().fg(theme.component.panel.title).bold()),
        ]),
        Line::from(vec![Span::styled(
            crate::truncate::truncate_to_width(&format!("  [{}]", group.agent_id), width),
            Style::new().fg(theme.semantic.text.dim),
        )]),
    ];
    // 只有真实 failed 且有原因（canonical error_reason 或子工具错误兜底）才
    // 追加；completed/running 不把 result 文本当失败原因，也不留空行。
    if group.is_error
        && let Some(reason) = subagent_error_reason(group)
    {
        lines.extend(subagent_error_reason_lines(grid, reason));
    }
    lines
}

#[component]
pub fn SubAgentDetailPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let _lang_ver = hooks.use_atom(&LANG_VERSION);
    // 外部滚动状态——面板滚轮仲裁（panel_scroll.rs）驱动，统一 3 行/格
    let sv = hooks.use_state(|| 0usize);
    let scrollbar_grab = hooks.use_state(|| None::<u16>);
    let render_cache = hooks.use_state(render_cache::DetailRenderCache::default);
    // 副产物命中（GenUI 按钮 / Mermaid 查看图）——draw 期按内容区几何重建，
    // 事件处理器读取最近一帧（与消息区 hits 同模式）。
    let artifacts = hooks.use_state(|| Arc::new(render_cache::DetailArtifacts::default()));
    let history = hooks.use_state(child_history::ChildHistoryView::default);
    let _ = hooks.use_atom(&ACTIVE_SESSION_ID);
    let _ = hooks.use_atom(&BRIDGE_RESET_COUNTER);
    // GenUI 动作只 bump heartbeat（不直接改 VIEW_MODELS）；面板订阅它才能把
    // Set/Reset 后的按钮外观与高度刷新出来。
    let _ = hooks.use_atom(&RENDER_HEARTBEAT);
    let _ = hooks.use_atom(&SELECTED_SUBAGENT_ID);
    // 面板内容区几何——draw 期记录（`DetailGeometry`），prepare、鼠标命中与滚轮
    // 仲裁读取同一份。首帧 update 尚未测量（`content == None`），此时不 prepare；
    // 首次 draw 回填后由 `DetailGeometry::poll_change` 请求一帧校正渲染，届时才按
    // 实测宽度准备（不再用 0/2 列兜底宽度先准备一遍）。
    let draw_hook = hooks.use_hook(move || DetailViewportHook {
        cache: render_cache,
        scroll: sv,
        geometry: DetailGeometry::default(),
        last_height: 0,
        last_viewport_height: 0,
        artifacts,
    });
    let content = draw_hook.geometry.content();
    let prepare_layout = draw_hook.geometry.prepare_layout();
    let viewport = content.map(detail_viewport);
    let viewport_height = content.map_or(0, |content| content.height as usize);

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

    let history_scope = history.read().scope_for(
        selected_id.as_deref().unwrap_or_default(),
        group.as_ref().map(|group| group.agent_id.as_str()),
    );
    hooks.use_effect(
        {
            let scope = history_scope.clone();
            move || {
                history.write().reset(scope);
                render_cache.write().clear(Vec::new());
                *artifacts.write() = Arc::default();
                *sv.write() = 0;
            }
        },
        history_scope.clone(),
    );
    let event_scope = history_scope.clone();

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
        let key = match event {
            Event::Mouse(mouse) => {
                if crate::kit::atoms::POPUP_KIND.state().read().is_some() {
                    return EventResult::Ignored;
                }
                // 命中几何与 draw/prepare 同源（draw 期记录的内容区）；首帧未测量时
                // 不产生命中——不能按兜底矩形反推行号。
                let Some(viewport) = viewport else {
                    return EventResult::Ignored;
                };
                let scrollbar_col = viewport.x.saturating_add(viewport.width);
                let in_rows = mouse.row >= viewport.y
                    && mouse.row < viewport.y.saturating_add(viewport.height);
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left)
                        if in_rows && mouse.column == scrollbar_col =>
                    {
                        let cache = render_cache.read();
                        let current = *sv.read();
                        let bar = VerticalBar::new(
                            viewport.height,
                            viewport.height as usize,
                            cache.height(),
                            current,
                        );
                        let Some(bar) = bar else {
                            return EventResult::Consumed;
                        };
                        let bar_area = Rect::new(scrollbar_col, viewport.y, 1, viewport.height);
                        match bar.press(bar_area, mouse.row) {
                            VerticalPress::Top => {
                                *sv.write() = 0;
                                *scrollbar_grab.write() = None;
                            }
                            VerticalPress::Bottom => {
                                *sv.write() = bar.max_offset();
                                *scrollbar_grab.write() = None;
                            }
                            VerticalPress::Thumb { grab } => {
                                *scrollbar_grab.write() = Some(grab);
                            }
                            VerticalPress::Track { offset } => {
                                *sv.write() = offset;
                                *scrollbar_grab.write() = Some((bar.thumb_len / 2) as u16);
                            }
                        }
                        return EventResult::Consumed;
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        if let Some(grab) = *scrollbar_grab.read() {
                            let cache = render_cache.read();
                            let current = *sv.read();
                            let Some(bar) = VerticalBar::new(
                                viewport.height,
                                viewport.height as usize,
                                cache.height(),
                                current,
                            ) else {
                                return EventResult::Consumed;
                            };
                            let bar_area = Rect::new(scrollbar_col, viewport.y, 1, viewport.height);
                            *sv.write() = bar.drag_offset(bar_area, mouse.row, grab);
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
                        // 缓存读锁内只做命中解析；动作（GenUI 分发 / 打开图面板）
                        // 在锁外执行，避免动作触发重渲染时与缓存写入互相等待。
                        let click = render_cache::resolve_artifact_click(
                            &render_cache.read(),
                            &artifacts.read(),
                            mouse.row,
                            mouse.column,
                        );
                        if render_cache::apply_artifact_click(click) {
                            return EventResult::Consumed;
                        }
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
        if key.code == KeyCode::Char('h') {
            let enabled = history.read().enabled;
            if enabled {
                history.write().close();
            } else {
                history.write().reset(event_scope.clone());
                child_history::request(history, child_history::Action::Open);
            }
            render_cache.write().clear(Vec::new());
            *sv.write() = 0;
            return EventResult::Consumed;
        }
        if history.read().enabled {
            use child_history::Action;
            let action = match key.code {
                KeyCode::Char('[') => Some(Action::Older),
                KeyCode::Char(']') => Some(Action::Newer),
                KeyCode::Char('r') => Some(Action::Open),
                KeyCode::Enter => Some(Action::Body),
                KeyCode::Char('n') => Some(Action::NextChunk),
                KeyCode::Char('p') => Some(Action::RestartBody),
                _ => None,
            };
            if let Some(action) = action {
                child_history::request(history, action);
                *sv.write() = 0;
                return EventResult::Consumed;
            }
            match key.code {
                KeyCode::Esc => {
                    history.write().close();
                    render_cache.write().clear(Vec::new());
                    *sv.write() = 0;
                    return EventResult::Consumed;
                }
                KeyCode::Char('b') => {
                    history.write().back_to_page();
                    *sv.write() = 0;
                    return EventResult::Consumed;
                }
                KeyCode::Tab | KeyCode::BackTab => {
                    history.write().select_body(key.code == KeyCode::BackTab);
                    *sv.write() = 0;
                    return EventResult::Consumed;
                }
                _ => {}
            }
        }
        // Esc 单层关闭（面板打开时 active_layer=Panel，消息区 Esc 分支放行，
        // 这里栈顶弹栈收尾；嵌套消息不参与消息区焦点仲裁）。
        match key.code {
            KeyCode::Esc => {
                history.write().close();
                render_cache.write().clear(Vec::new());
                close_panel();
            }
            KeyCode::Tab => {
                if let Some(row) = render_cache.write().focus_next(false) {
                    focus_detail_row(sv, row, viewport_height);
                }
            }
            KeyCode::BackTab => {
                if let Some(row) = render_cache.write().focus_next(true) {
                    focus_detail_row(sv, row, viewport_height);
                }
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
                if let Some(row) = render_cache.write().focus_next(false) {
                    focus_detail_row(sv, row, viewport_height);
                }
            }
            KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => {
                if let Some(row) = render_cache.write().focus_next(true) {
                    focus_detail_row(sv, row, viewport_height);
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let mut cache = render_cache.write();
                if cache.focused().is_none()
                    && let Some(row) = cache.focus_next(false)
                {
                    focus_detail_row(sv, row, viewport_height);
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
                let next = sv.read().saturating_sub(viewport_height);
                *sv.write() = next;
            }
            KeyCode::PageDown => {
                let next = sv.read().saturating_add(viewport_height);
                *sv.write() = next;
            }
            KeyCode::Home => *sv.write() = 0,
            KeyCode::End => *sv.write() = render_cache.read().height(),
            _ => {}
        }
        EventResult::Consumed
    });

    // 嵌套渲染：组内 view_models → vm_to_lines_cached（复用统一水平网格与
    // markdown 增量缓存；md 复制按钮关闭——嵌套内容不提供复制按钮）。
    // wrap 宽度、初始偏移与滚轮仲裁都取 draw 期记录的内容区；几何未知（首帧
    // update）时整段延后——不能用兜底宽度先准备一遍极窄的行。
    let mut lines: Vec<Line<'static>> = Vec::new();
    let reading_history = {
        let history = history.read();
        history.enabled && history.matches(&history_scope)
    };
    if let (Some(content), Some(layout)) = (content, prepare_layout) {
        if reading_history {
            let width = content.width.saturating_sub(1) as usize;
            let (header, text) = history.read().presentation(width);
            // The sparse layout keeps total height without allocating every rendered row.
            render_cache
                .write_no_update()
                .prepare_text(header, text, width);
        } else {
            match &group {
                Some(g) => {
                    let theme = theme_def.read().clone();
                    lines.extend(detail_header_lines(
                        g,
                        &layout.grid,
                        content.width.saturating_sub(1) as usize,
                        &theme,
                    ));
                    lines.push(Line::from(""));
                    if g.preview_omitted {
                        lines.push(Line::from(i18n::tr("subagent-preview-omitted")));
                    }
                    lines.push(Line::from(i18n::tr("subagent-history-open-hint")));
                    let mut cache = render_cache.write_no_update();
                    if cache.prepare(g, &layout.grid, theme, LANG_VERSION.get(), lines) {
                        *sv.write_no_update() = initial_detail_offset(
                            g.is_running,
                            cache.height(),
                            layout.viewport_height,
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
        }
    }

    crate::kit::panel_scroll::register_virtual_panel_scroll(
        PanelKind::SubAgentDetail,
        content.unwrap_or_default(),
        sv,
        render_cache.read().height(),
        viewport_height,
    );

    panel_shell!(PanelKind::SubAgentDetail, {
        View(
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        )
    })
}

pub(super) struct DetailViewportHook {
    pub(super) cache: State<render_cache::DetailRenderCache>,
    pub(super) scroll: State<usize>,
    pub(super) geometry: DetailGeometry,
    pub(super) last_height: usize,
    pub(super) last_viewport_height: usize,
    pub(super) artifacts: State<Arc<render_cache::DetailArtifacts>>,
}

impl Hook for DetailViewportHook {
    fn poll_change(&mut self, cx: &mut std::task::Context) -> std::task::Poll<()> {
        // 首次记录到内容区（或几何变化）时请求一帧校正渲染：update 期 prepare
        // 才拿得到实测内容区宽度。
        self.geometry.poll_change(cx)
    }

    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        // Border 在绘制子节点时收窄 `drawer.area`；这里拿到的是面板外框，
        // 内容区由同一份公式换算（prepare/命中读取的正是这次记录）。
        self.geometry.observe(drawer.area);
    }

    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        let Some(content) = self.geometry.content() else {
            return;
        };
        let viewport = detail_viewport(content);
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
        // 副产物命中与 draw 同源几何、同一次 ensure_visible 结果；滚动或内容
        // 变化后热区随帧更新。
        let hits = render_cache::collect_artifacts(&cache, viewport, top);
        *self.artifacts.write_no_update() = Arc::new(hits);
        crate::kit::scrollbar_chrome::draw_scrollbar(
            drawer,
            Rect {
                width: content.width,
                ..viewport
            },
            height,
            top,
            viewport.height as usize,
        );
    }
}

/// 面板内容区几何——draw 期记录、prepare 与命中读取的唯一来源。
///
/// 仿 `MsgAreaTracker` / `AreaTracker`（值类型 + hook 跨帧持有）：
/// `pre_component_draw` 每帧把面板外框换算成内容区；首次测量与每次几何变化置
/// 校正标记，由 `poll_change` 请求一帧重渲染。首帧 update 拿不到实测几何——
/// 此时 `content()` 与 `prepare_layout()` 都是 `None`，prepare 延后到校正帧，
/// 不再用 2 列兜底宽度先准备一遍（那会把内容折成极窄行并一直停留到下次刷新）。
#[derive(Debug, Default)]
pub(super) struct DetailGeometry {
    content: Option<Rect>,
    changed: bool,
}

impl DetailGeometry {
    /// 已记录的内容区；首帧/从未绘制时为 `None`。
    #[must_use]
    pub(super) fn content(&self) -> Option<Rect> {
        self.content
    }

    /// prepare 布局：几何未知时 `None`（本帧不 prepare）；已记录时由内容区宽度
    /// 派生网格、由内容区高度派生视口高度。
    #[must_use]
    pub(super) fn prepare_layout(&self) -> Option<DetailPrepareLayout> {
        let content = self.content?;
        // 1 列退化几何仍按 2 列取网格（既有下限钳制）；这不是未知几何的兜底——
        // 未知时根本不会走到这里。
        // 面板与主时间线共用断点/缩进（C2）：content 宽度 >= 60 时 indent = 2，
        // 过程区台阶由 render_options 的 transition 驱动，不再清零。
        let grid = GridSpec::grid_for(content.width.max(2));
        Some(DetailPrepareLayout {
            grid,
            viewport_height: content.height as usize,
        })
    }

    /// 记录面板外框对应的内容区；几何变化（含首次测量）置校正标记。
    pub(super) fn observe(&mut self, panel: Rect) {
        let content = detail_content_area(panel);
        if self.content == Some(content) {
            return;
        }
        self.content = Some(content);
        self.changed = true;
    }
}

impl Hook for DetailGeometry {
    fn poll_change(&mut self, _cx: &mut std::task::Context) -> std::task::Poll<()> {
        if std::mem::take(&mut self.changed) {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    }

    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.observe(drawer.area);
    }
}

/// `prepare` 所需的几何——网格与视口高度同取内容区，避免第二套尺寸来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DetailPrepareLayout {
    pub(super) grid: GridSpec,
    pub(super) viewport_height: usize,
}

pub(super) fn focus_detail_row(scroll: State<usize>, row: usize, viewport: usize) {
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

/// 视口上缘口径与主时间线共用一份实现（[`viewport_policy`]）：初始位置。
#[must_use]
pub(super) fn initial_detail_offset(running: bool, height: usize, viewport: usize) -> usize {
    viewport_policy::initial_top(running, height, viewport)
}

/// 视口上缘口径与主时间线共用一份实现（[`viewport_policy`]）：吸底跟随。
pub(super) fn follow_detail_offset(
    top: usize,
    previous_height: usize,
    previous_viewport: usize,
    height: usize,
    viewport: usize,
    source_changed: bool,
) -> usize {
    viewport_policy::follow_top(
        top,
        previous_height,
        previous_viewport,
        height,
        viewport,
        source_changed,
    )
}

/// 面板内容区——`panel_shell!` 的上下边框/标题之后的那块区域。
///
/// 与 `Block::inner` 对 `Borders::TOP | Borders::BOTTOM` 的换算一致：上边框（带
/// 标题）与下边框各占 1 行，左右无边框（最右列由 [`detail_viewport`] 让给滚动条）。
#[must_use]
pub(super) fn detail_content_area(panel: Rect) -> Rect {
    Rect {
        y: panel.y.saturating_add(1),
        height: panel.height.saturating_sub(2),
        ..panel
    }
}

/// 内容区右侧让出滚动条列后的绘制/命中视口。
#[must_use]
pub(super) fn detail_viewport(content: Rect) -> Rect {
    Rect {
        width: content.width.saturating_sub(1),
        ..content
    }
}

#[cfg(test)]
pub(super) fn scroll_content_height(line_count: usize) -> u16 {
    line_count.clamp(1, u16::MAX as usize) as u16
}

/// 解析选中 subagent 的渲染源。
///
/// 两个候选源：VIEW_MODELS 扫描（组的权威投影）与 `BG_LIVE_DETAIL`（某次 bg
/// 运行的权威投影——bg 的工具事件只进这里，且组在 turn 边界被归档冻结）。选中项
/// 能对上某次 bg 运行时以 live 为准，冻结的组只作回退。
pub(super) fn resolve_selected_subagent(
    vm_snapshot: &crate::kit::atoms::ViewModelsSnapshot,
    live: &std::collections::HashMap<String, crate::kit::atoms::BgLiveDetail>,
    display: &[BgDisplayEntry],
    selected_id: Option<&str>,
) -> Option<TuiSubAgentGroup> {
    find_live_detail_subagent(live, display, selected_id)
        .or_else(|| find_selected_subagent(vm_snapshot, selected_id))
}

/// 从 VIEW_MODELS 快照扫描 `TuiSubAgentGroup`，按 instance_id / agent_id 匹配选中项。
/// 扫描口径与 agent.rs `collect_subagents` 一致（含嵌套 SubAgent 递归）。
pub(super) fn find_selected_subagent(
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

/// 递归扫描单个 VM（含嵌套 SubAgent 内层）。
pub(super) fn scan_vm_for_subagent(
    vm: &TuiRenderUnit,
    selected_id: &str,
) -> Option<TuiSubAgentGroup> {
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

pub(super) fn find_live_detail_subagent(
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
    let group = TuiSubAgentGroup::new(
        task_id.to_string(),
        detail
            .agent_id
            .clone()
            .unwrap_or_else(|| selected_id.to_string()),
        detail
            .agent_name
            .clone()
            .unwrap_or_else(|| detail.summary.clone()),
        detail.nested_units.clone(),
        detail.preview_omitted,
        status,
        detail.subagent_result.clone(),
    );
    Some(group)
}

/// 解析选中 id 对应的 `BG_LIVE_DETAIL` task_id。
///
/// 三种来源：底栏行点击的 task_id；旧入口传入的 bg agent_id；消息区 Enter
/// 写入的组 instance_id。后者只认 live 明细记录的同一次运行：resume 复用
/// child_thread_id，按 agent_id 回查会把旧的那次运行当成当前运行。
pub(super) fn live_task_id_for(
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

pub(super) fn close_panel() {
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}

use crate::kit::atoms::ACTIVE_SESSION_ID;

use crate::kit::atoms::BG_DISPLAY;

use crate::kit::atoms::BG_LIVE_DETAIL;

use crate::kit::atoms::BRIDGE_RESET_COUNTER;

use crate::kit::atoms::BgDisplayEntry;

use crate::kit::atoms::LANG_VERSION;

use crate::kit::atoms::RENDER_HEARTBEAT;

use crate::kit::atoms::SELECTED_SUBAGENT_ID;

use crate::kit::atoms::VIEW_MODELS;
