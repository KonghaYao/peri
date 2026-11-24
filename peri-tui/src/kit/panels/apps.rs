//! MCP App 实时视图面板：最新帧 + 状态 + 多视图切换 + 画布点击。
//!
//! 触发入口：
//! - `/apps` slash 命令（命令面板）；
//! - 已完成的工具卡点击「打开应用 · <name>」（A1，见 `message_area::handlers`）。
//!
//! 渲染：图像经 [`crate::kit::apps::image_cache`] 在后台线程编码后由
//! `post_component_draw` 绘制（不阻塞渲染线程）；终端无图片协议（None /
//! ITerm2 / 缺单元格尺寸的 Sixel）时显示文字占位，不 panic。
//!
//! 关闭：Esc 由全局 handler 关闭面板，`panel_registry::close_*` 会通知运行时
//! 对所有视图 `DELETE` 网关会话。

use crate::i18n;
use crate::kit::apps::image_cache::{self, FrameKey};
use crate::kit::apps::mapping::{self, CellSize, FrameGeometry, ViewportPoint};
use crate::kit::apps::state::{APPS_STATE, AppView, AppsState, ViewPhase};
use crate::kit::apps::{self};
use crate::kit::image_preview;
use crate::kit::terminal_caps::TerminalCaps;
use peri_theme::atoms::THEME_ATOM;
use peri_tui_components::panel_mouse::AreaTracker;
use ratatui_kit::crossterm::event::Event;
use ratatui_kit::crossterm::event::KeyCode;
use ratatui_kit::crossterm::event::KeyEventKind;
use ratatui_kit::crossterm::event::KeyModifiers;
use ratatui_kit::crossterm::event::MouseButton;
use ratatui_kit::crossterm::event::MouseEventKind;
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::Constraint;
use ratatui_kit::ratatui::layout::Direction;
use ratatui_kit::ratatui::layout::Rect;
use ratatui_kit::ratatui::style::Style;
use ratatui_kit::ratatui::style::Stylize;
use ratatui_kit::ratatui::text::Line;
use ratatui_kit::ratatui::text::Span;
use ratatui_kit::ratatui::widgets::Paragraph;
use std::sync::Arc;

use crate::app::panel_types::PanelKind;

/// 活动视图的一帧快照（seq, 解码图像, 宽, 高）。
type FrameSnapshot = (u64, Arc<image::DynamicImage>, u32, u32);

/// MCP App 面板。
#[component]
pub fn AppsPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let _lang = hooks.use_atom(&crate::kit::atoms::LANG_VERSION);
    let apps = hooks.use_atom(&APPS_STATE);
    let _hb = hooks.use_atom(&crate::kit::atoms::RENDER_HEARTBEAT);
    let theme = hooks.use_atom(&THEME_ATOM);
    let selected = hooks.use_state(|| 0usize);
    let (_area_w, _area_h) = hooks.use_terminal_size();

    let state = apps.read().clone();
    let descriptor_count = state.descriptors.len();
    let has_views = !state.views.is_empty();
    let runtime_active = state.runtime == crate::kit::apps::AppsRuntimeState::Active;

    hooks.use_event_handler(EventScope::Current, EventPriority::High, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }
        // 弹窗优先：与 mermaid 面板一致，Esc/Enter 不能越过确认框。
        if matches!(
            crate::kit::focus_router::active_layer(),
            crate::kit::focus_router::FocusLayer::Popup(_)
        ) {
            return EventResult::Ignored;
        }
        match key.code {
            KeyCode::Tab => {
                apps::state::cycle_active_view(1);
                EventResult::Consumed
            }
            KeyCode::BackTab => {
                apps::state::cycle_active_view(-1);
                EventResult::Consumed
            }
            KeyCode::Char('x') if key.modifiers == KeyModifiers::NONE => {
                if let Some(view) = APPS_STATE.state().read().active().cloned() {
                    apps::close_view(&view.view_id);
                }
                EventResult::Consumed
            }
            KeyCode::Char('r') if key.modifiers == KeyModifiers::NONE => {
                if let Some(view) = APPS_STATE.state().read().active().cloned()
                    && matches!(
                        view.phase,
                        ViewPhase::Failed { .. }
                            | ViewPhase::Closed { .. }
                            | ViewPhase::LocalError { .. }
                    )
                {
                    apps::open_app(&view.app.app_id);
                }
                EventResult::Consumed
            }
            KeyCode::Up if !has_views && descriptor_count > 0 => {
                let current = *selected.read();
                *selected.write() = current.saturating_sub(1);
                EventResult::Consumed
            }
            KeyCode::Down if !has_views && descriptor_count > 0 => {
                let current = *selected.read();
                *selected.write() = (current + 1).min(descriptor_count.saturating_sub(1));
                EventResult::Consumed
            }
            KeyCode::Enter if !has_views => {
                let index = (*selected.read()).min(descriptor_count.saturating_sub(1));
                let target = APPS_STATE
                    .state()
                    .read()
                    .descriptors
                    .get(index)
                    .map(|entry| entry.app_id.clone());
                if let Some(app_id) = target {
                    apps::open_app(&app_id);
                }
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    });

    // TUI-THEME-001：guard 生命周期内复制主题值。
    let (primary, secondary, error, accent) = {
        let guard = theme.read();
        (
            guard.semantic.text.primary,
            guard.semantic.text.secondary,
            guard.semantic.status.error,
            guard.semantic.accent,
        )
    };

    if !has_views {
        let lines = if descriptor_count == 0 {
            empty_state_lines(runtime_active, &state, primary, secondary, error)
        } else {
            descriptor_lines(&state, *selected.read(), primary, secondary, accent)
        };
        return panel_shell!(PanelKind::McpApps, {
            Text(text: Paragraph::new(ratatui_kit::ratatui::text::Text::from(lines)))
        });
    }

    let Some(view) = state.active().cloned() else {
        return panel_shell!(PanelKind::McpApps, {
            Text(text: Paragraph::new(ratatui_kit::ratatui::text::Text::from(vec![
                Line::from(Span::styled(i18n::tr("apps-no-active"), Style::default().fg(secondary))),
            ])))
        });
    };
    let header = header_lines(&state, &view, primary, secondary, error);
    let footer = Line::from(Span::styled(
        i18n::tr_args(
            "apps-footer-hint",
            &[("index".into(), index_label(&state).into())],
        ),
        Style::default().fg(secondary),
    ));

    panel_shell!(PanelKind::McpApps, {
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            View(height: Constraint::Length(line_count(&header))) {
                Text(text: Paragraph::new(ratatui_kit::ratatui::text::Text::from(header)))
            }
            FrameCanvas()
            View(height: Constraint::Length(1)) {
                Text(text: Paragraph::new(ratatui_kit::ratatui::text::Text::from(vec![footer])))
            }
        }
    })
}

fn line_count(lines: &[Line<'static>]) -> u16 {
    u16::try_from(lines.len()).unwrap_or(u16::MAX)
}

fn index_label(state: &AppsState) -> String {
    let total = state.views.len();
    let current = state
        .active_view
        .as_ref()
        .and_then(|id| state.views.iter().position(|view| &view.view_id == id))
        .map(|index| index + 1)
        .unwrap_or(0);
    format!("{current}/{total}")
}

fn empty_state_lines(
    runtime_active: bool,
    state: &AppsState,
    primary: ratatui_kit::ratatui::style::Color,
    secondary: ratatui_kit::ratatui::style::Color,
    error: ratatui_kit::ratatui::style::Color,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        i18n::tr("apps-empty-title"),
        Style::default().fg(primary),
    ))];
    if !runtime_active {
        lines.push(Line::from(Span::styled(
            i18n::tr("apps-disabled-hint"),
            Style::default().fg(secondary),
        )));
        return lines;
    }
    lines.push(Line::from(Span::styled(
        i18n::tr("apps-empty-hint"),
        Style::default().fg(secondary),
    )));
    if let Some(code) = state.discovery_error.as_deref() {
        lines.push(Line::from(Span::styled(
            i18n::tr_args("apps-discovery-error", &[("code".into(), code.into())]),
            Style::default().fg(error),
        )));
    }
    if let Some(code) = state.open_error.as_deref() {
        lines.push(Line::from(Span::styled(
            i18n::tr_args("apps-open-error", &[("code".into(), code.into())]),
            Style::default().fg(error),
        )));
    }
    lines
}

fn descriptor_lines(
    state: &AppsState,
    selected: usize,
    primary: ratatui_kit::ratatui::style::Color,
    secondary: ratatui_kit::ratatui::style::Color,
    accent: ratatui_kit::ratatui::style::Color,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        i18n::tr_args(
            "apps-list-title",
            &[(
                "count".into(),
                u64::try_from(state.descriptors.len())
                    .unwrap_or(u64::MAX)
                    .into(),
            )],
        ),
        Style::default().fg(primary),
    ))];
    for (index, entry) in state.descriptors.iter().enumerate() {
        let marker = if index == selected { "▶ " } else { "  " };
        let display_name = entry.display_name();
        let name = crate::kit::image_safety::sanitize_for_terminal(&display_name);
        let style = if index == selected {
            Style::default().fg(accent)
        } else {
            Style::default().fg(secondary)
        };
        lines.push(Line::from(Span::styled(format!("{marker}{name}"), style)));
    }
    if let Some(code) = state.open_error.as_deref() {
        lines.push(Line::from(Span::styled(
            i18n::tr_args("apps-open-error", &[("code".into(), code.into())]),
            Style::default().fg(secondary),
        )));
    }
    lines
}

fn header_lines(
    state: &AppsState,
    view: &AppView,
    primary: ratatui_kit::ratatui::style::Color,
    secondary: ratatui_kit::ratatui::style::Color,
    error: ratatui_kit::ratatui::style::Color,
) -> Vec<Line<'static>> {
    let display_name = view.app.display_name();
    let name = crate::kit::image_safety::sanitize_for_terminal(&display_name);
    let title = if matches!(view.phase, ViewPhase::Starting) {
        i18n::tr_args("apps-view-title-starting", &[("name".into(), name.into())])
    } else {
        i18n::tr_args("apps-view-title", &[("name".into(), name.into())])
    };
    let phase = phase_label(&view.phase);
    let frame = view.frame.as_ref().map_or_else(
        || i18n::tr("apps-frame-none"),
        |frame| {
            i18n::tr_args(
                "apps-frame-info",
                &[
                    ("seq".into(), frame.seq.into()),
                    ("w".into(), u64::from(frame.width).into()),
                    ("h".into(), u64::from(frame.height).into()),
                ],
            )
        },
    );
    let mut second = format!("{phase} · {frame}");
    if view.pending_requests > 0 {
        second.push_str(" · ");
        second.push_str(&i18n::tr_args(
            "apps-pending-requests",
            &[(
                "count".into(),
                u64::try_from(view.pending_requests)
                    .unwrap_or(u64::MAX)
                    .into(),
            )],
        ));
    }
    if let Some(code) = view.phase.error_code() {
        second.push_str(&format!(" · {code}"));
    } else if let Some(code) = view.last_error.as_deref() {
        second.push_str(&format!(" · {code}"));
    }
    let index = index_label(state);
    vec![
        Line::from(Span::styled(title, Style::default().fg(primary))),
        Line::from(Span::styled(
            format!("{second}  [{index}]"),
            Style::default().fg(if view.phase.error_code().is_some() {
                error
            } else {
                secondary
            }),
        )),
    ]
}

fn phase_label(phase: &ViewPhase) -> String {
    match phase {
        ViewPhase::Reconnecting { attempt } => i18n::tr_args(
            "apps-phase-reconnecting",
            &[("attempt".into(), u64::from(*attempt).into())],
        ),
        other => i18n::tr(if other.error_code().is_some() {
            "apps-phase-failed"
        } else {
            other.label_key()
        }),
    }
}

// ── 帧画布 ────────────────────────────────────────────────────────────────

/// 画布跨帧状态：活动视图身份 + 最近一次组件区域。
///
/// 绘制键在 `post_component_draw` 用**当帧区域**重建：`use_previous_size`
/// 首帧为零、resize 后滞后一帧，若沿用渲染 body 的旧尺寸，编码结果与预留区
/// 不一致。`poll_change` 在区域变化时补一帧，让 effect 以新尺寸重新编码。
pub(crate) struct FrameDrawHook {
    view: Option<(String, u64)>,
    /// **唯一**的画布区域来源：`pre_component_draw` 的当帧 `drawer.area`。
    /// 绘制用它；面板点击读取它的上一帧副本（`AreaTracker` 语义）。
    area: AreaTracker,
    last_area: Option<Rect>,
    /// 上一帧真正绘制图像的矩形（居中适配后）。点击只认这里；
    /// 与绘制同一次 `post_component_draw` 计算，禁止另一套几何。
    drawn: Option<Rect>,
}

impl Default for FrameDrawHook {
    fn default() -> Self {
        Self {
            view: None,
            area: AreaTracker::new(),
            last_area: None,
            drawn: None,
        }
    }
}

impl Hook for FrameDrawHook {
    fn poll_change(&mut self, _cx: &mut std::task::Context) -> std::task::Poll<()> {
        if self.area.rect != self.last_area {
            self.last_area = self.area.rect;
            return std::task::Poll::Ready(());
        }
        std::task::Poll::Pending
    }

    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.area.pre_component_draw(drawer);
    }

    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.drawn = None;
        let (Some(area), Some((view_id, seq))) = (self.area.rect, self.view.as_ref()) else {
            return;
        };
        if area.width == 0 || area.height == 0 {
            return;
        }
        let key = FrameKey {
            view_id: view_id.clone(),
            seq: *seq,
            cols: area.width,
            rows: area.height,
        };
        if let Some(image) = image_cache::lookup_ready(&key) {
            // 等比适配后的图像在预留区内居中（与 mapping::fit_image 同口径，
            // 阶段 2 的鼠标映射因此可直接复用该几何）。
            let cols = image.cols().min(area.width);
            let rows = image.rows().min(area.height);
            if cols == 0 || rows == 0 {
                return;
            }
            let rect = Rect::new(
                area.x + area.width.saturating_sub(cols) / 2,
                area.y + area.height.saturating_sub(rows) / 2,
                cols,
                rows,
            );
            self.drawn = Some(rect);
            image.render(rect, drawer.buffer_mut());
        }
    }
}

/// 帧画布：订阅全局状态，取活动视图最新帧；后台编码 + `post_component_draw` 绘制。
#[component]
fn FrameCanvas(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let _lang = hooks.use_atom(&crate::kit::atoms::LANG_VERSION);
    let _hb = hooks.use_atom(&crate::kit::atoms::RENDER_HEARTBEAT);
    let apps = hooks.use_atom(&APPS_STATE);
    let caps_handle = hooks.use_atom(&crate::kit::atoms::TERMINAL_CAPS);
    let theme = hooks.use_atom(&THEME_ATOM);
    let area = hooks.use_previous_size();

    let (view_id, frame, active_view) = {
        let state = apps.read();
        match state.active() {
            Some(view) => (
                view.view_id.clone(),
                view.frame.as_ref().map(|frame| {
                    (
                        frame.seq,
                        Arc::clone(&frame.image),
                        frame.width,
                        frame.height,
                    )
                }),
                Some(view.clone()),
            ),
            None => (String::new(), None, None),
        }
    };
    let caps = *caps_handle.read();
    let cell_px = image_preview::cell_pixel_size();
    let protocol_ok = image_preview::protocol_available(&caps, cell_px);

    let key = frame.as_ref().and_then(|(seq, _, _, _)| {
        (area.width > 0 && area.height > 0 && !view_id.is_empty()).then(|| FrameKey {
            view_id: view_id.clone(),
            seq: *seq,
            cols: area.width,
            rows: area.height,
        })
    });
    let encode_key = key.clone();
    let encode_image = frame.as_ref().map(|(_, image, _, _)| Arc::clone(image));
    let caps_for_effect = caps;
    hooks.use_effect(
        move || {
            if let (Some(key), Some(image)) = (encode_key.clone(), encode_image.clone()) {
                image_cache::request(key, image, &caps_for_effect, cell_px);
            }
        },
        (
            key.clone(),
            caps.graphics,
            cell_px,
            frame.as_ref().map(|(_, _, w, h)| (*w, *h)),
            image_cache::revision(),
        ),
    );

    let draw = hooks.use_hook(FrameDrawHook::default);
    draw.view = frame.as_ref().map(|(seq, _, _, _)| (view_id.clone(), *seq));

    // 面板点击（A2）：区域取 AreaTracker 的上一帧实测矩形（与绘制同源）；
    // 事件坐标必须来自真实 MouseEvent，映射失败（黑边/无帧/无单元格尺寸）零请求。
    let click_area = draw.drawn;
    let click_cell = cell_px.map(|(width, height)| CellSize { width, height });
    let click_view = active_view;
    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::High,
        EventOptions { hit_test: true },
        move |event| {
            let Event::Mouse(mouse) = event else {
                return EventResult::Ignored;
            };
            if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
                return EventResult::Ignored;
            }
            if crate::kit::mouse_router::is_popup_active() {
                return EventResult::Ignored;
            }
            let (Some(area), Some(cell), Some(view)) =
                (click_area, click_cell, click_view.as_ref())
            else {
                return EventResult::Ignored;
            };
            // `hit_test` 覆盖整个画布（含黑边）；点击只认实际绘制矩形。
            if mouse.column < area.x
                || mouse.row < area.y
                || mouse.column >= area.x.saturating_add(area.width)
                || mouse.row >= area.y.saturating_add(area.height)
            {
                return EventResult::Ignored;
            }
            let Some((view_id, point)) = click_target(view, area, cell, mouse.column, mouse.row)
            else {
                return EventResult::Ignored;
            };
            if crate::kit::apps::request_view_click(&view_id, point) {
                EventResult::Consumed
            } else {
                EventResult::Ignored
            }
        },
    );

    let ready = key.as_ref().and_then(image_cache::lookup_ready);
    if ready.is_some() {
        let empty: AnyElement<'static> =
            element!(View(width: Constraint::Fill(1), height: Constraint::Fill(1))).into();
        return empty;
    }

    let (secondary, error) = {
        let guard = theme.read();
        (guard.semantic.text.secondary, guard.semantic.status.error)
    };
    let encode_failed = key.as_ref().is_some_and(image_cache::is_failed);
    let placeholder = placeholder_lines(
        &view_id,
        frame.as_ref(),
        protocol_ok,
        encode_failed,
        &caps,
        secondary,
        error,
    );
    let placeholder: AnyElement<'static> =
        element!(Text(text: Paragraph::new(ratatui_kit::ratatui::text::Text::from(placeholder))))
            .into();
    placeholder
}

/// 纯函数：视图 + 画布区域 + 单元格尺寸 + 终端坐标 → (view_id, CSS 点)。
///
/// 面板事件 handler 与单测共用；越界（黑边/区域外/无帧）返回 `None`，
/// 调用方据此保证「映射失败零请求」。
#[must_use]
pub(crate) fn click_target(
    view: &AppView,
    area: Rect,
    cell: CellSize,
    column: u16,
    row: u16,
) -> Option<(String, ViewportPoint)> {
    view.view_session_id.as_ref()?;
    let frame = view.frame.as_ref()?;
    let geometry = FrameGeometry {
        width: frame.width,
        height: frame.height,
        viewport_width: frame.viewport_width,
        viewport_height: frame.viewport_height,
    };
    mapping::terminal_cell_to_viewport(&geometry, area, cell, column, row)
        .map(|point| (view.view_id.clone(), point))
}

fn placeholder_lines(
    view_id: &str,
    frame: Option<&FrameSnapshot>,
    protocol_ok: bool,
    encode_failed: bool,
    caps: &TerminalCaps,
    secondary: ratatui_kit::ratatui::style::Color,
    error: ratatui_kit::ratatui::style::Color,
) -> Vec<Line<'static>> {
    if view_id.is_empty() {
        return vec![Line::from(Span::styled(
            i18n::tr("apps-frame-none"),
            Style::default().fg(secondary),
        ))];
    }
    let Some((seq, _, width, height)) = frame else {
        return vec![Line::from(Span::styled(
            i18n::tr("apps-frame-waiting"),
            Style::default().fg(secondary),
        ))];
    };
    // 帧已到达：无协议 / 编码未完成时都保留可读的帧元数据。
    let meta = i18n::tr_args(
        "apps-frame-info",
        &[
            ("seq".into(), (*seq).into()),
            ("w".into(), u64::from(*width).into()),
            ("h".into(), u64::from(*height).into()),
        ],
    );
    if !protocol_ok {
        let reason = match caps.graphics {
            crate::kit::terminal_caps::GraphicsProtocol::None
            | crate::kit::terminal_caps::GraphicsProtocol::ITerm2 => i18n::tr("apps-no-graphics"),
            _ => i18n::tr("apps-no-cell-size"),
        };
        return vec![
            Line::from(Span::styled(
                i18n::tr_args("apps-placeholder-title", &[("frame".into(), meta.into())]),
                Style::default().fg(secondary),
            )),
            Line::from(Span::styled(reason, Style::default().fg(error))),
        ];
    }
    if encode_failed {
        return vec![
            Line::from(Span::styled(
                i18n::tr_args("apps-placeholder-title", &[("frame".into(), meta.into())]),
                Style::default().fg(secondary),
            )),
            Line::from(Span::styled(
                i18n::tr("apps-frame-encode-failed"),
                Style::default().fg(error),
            )),
        ];
    }
    vec![Line::from(Span::styled(
        i18n::tr_args("apps-frame-encoding", &[("frame".into(), meta.into())]),
        Style::default().fg(secondary),
    ))]
}

// ── 测试辅助：面板纯函数（不依赖渲染后端） ──────────────────────────────────

#[cfg(test)]
pub(crate) fn test_frame(seq: u64) -> crate::kit::apps::state::FrameImage {
    crate::kit::apps::state::FrameImage {
        seq,
        width: 1280,
        height: 720,
        viewport_width: 1280,
        viewport_height: 720,
        image: Arc::new(image::DynamicImage::new_rgb8(8, 8)),
    }
}

#[cfg(test)]
pub(crate) fn rendered_text(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[path = "apps_test.rs"]
mod tests;
