//! Model panel event handling: hit geometry, keyboard/mouse actions and the
//! provider state transitions they trigger.
//!
//! The component in `model.rs` owns hooks and rendering only. `State<T>` has no
//! public constructor, so the event context below is built by the component and
//! handed to [`handle_model_event`]; tests capture the same handles through the
//! mount probe.

use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{LOGIN_PANEL_REQUEST, LoginPanelRequest, PERI_CONFIG_HANDLE};
use crate::kit::list_nav::{next_selection, previous_selection};
use crate::kit::panel_mouse::hit_item;
use crate::kit::panels::shared::list::ListViewport;
use ratatui_kit::{
    crossterm::event::{
        Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    prelude::*,
    ratatui::layout::Rect,
};
use unicode_width::UnicodeWidthStr;

use super::edit::{edit_field, effective_provider_id, set_context_window, switch_active_alias};
use super::editor_view::{PaneHit, kv_row_hits};
use super::panel_render::provider_button_label;
use super::{FIELD_CONTEXT_WINDOW, FIELD_COUNT, PROFILE_KEYS};

/// 右栏内容区几何：标题 1 行 + 空行 1 行 + 主区 + 底部提示 1 行。
///
/// 与 `panel_shell!` 的 TOP|BOTTOM 边框、`45% | 1 | Fill` 分栏同源；滚动条列落在
/// 面板最右列，命中时排除。
struct RightPaneGeometry {
    x: u16,
    width: u16,
    content_top: u16,
    height: u16,
}

fn right_pane_geometry(area: Rect) -> RightPaneGeometry {
    let left_width = area.width * 45 / 100;
    let x = area.x.saturating_add(left_width).saturating_add(1);
    let right_edge = area.x.saturating_add(area.width);
    RightPaneGeometry {
        x,
        width: right_edge.saturating_sub(x),
        content_top: area.y.saturating_add(3),
        height: area.height.saturating_sub(5),
    }
}

/// 右栏语义行命中：滚动偏移与绘制共用同一 `ScrollViewState`，空白/边框/滚动条不命中。
///
/// `hits` 是逐行命中表，与右栏 K/V 绘制行同序。
fn hit_right_pane(
    hits: &[Option<PaneHit>],
    mouse: &MouseEvent,
    area: Rect,
    scroll_offset: u16,
) -> Option<PaneHit> {
    let geo = right_pane_geometry(area);
    if geo.width == 0 {
        return None;
    }
    let right_edge = geo.x.saturating_add(geo.width);
    if mouse.column < geo.x || mouse.column >= right_edge {
        return None;
    }
    // 右栏 ScrollView 的滚动条占最右 1 列。
    if mouse.column == right_edge.saturating_sub(1) {
        return None;
    }
    let content_bottom = geo.content_top.saturating_add(geo.height);
    if mouse.row < geo.content_top || mouse.row >= content_bottom {
        return None;
    }
    let row = (mouse.row - geo.content_top) as usize + scroll_offset as usize;
    hits.get(row).copied().flatten()
}

/// 底部按钮命中：仅左键、且落在提示行（面板最底内容行）的按钮文本列范围内。
/// 其右侧的键盘提示不触发动作。
fn hit_provider_button(mouse: &MouseEvent, area: Rect) -> bool {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) || area.height < 2 || area.width == 0 {
        return false;
    }
    let row = area.y.saturating_add(area.height).saturating_sub(2);
    if mouse.row != row {
        return false;
    }
    let width = UnicodeWidthStr::width(provider_button_label().as_str()) as u16;
    mouse.column >= area.x && mouse.column < area.x.saturating_add(width)
}

/// 面板事件处理——判定集中在这里、组件只做注册，测试可经 `tests::mount_panel`
/// 捕获真实句柄后直接投喂事件（`State<T>` 无公开构造 API）。
///
/// 鼠标命中与绘制共用 `ctx.viewport`（`hit_layout()` / `window()` 同源），
/// 面板高度变化后两者看到的档位数一致。
pub(super) fn handle_model_event(event: Event, ctx: &ModelEventCtx) -> EventResult {
    handle_model_event_with(event, ctx, switch_active_alias)
}

/// 事件判定本体（档位切换入口注入）。
///
/// 注入用于不带配置源的命中测试；持久化测试经 `handle_model_event` 覆盖真实切换。
pub(super) fn handle_model_event_with(
    event: Event,
    ctx: &ModelEventCtx,
    switch_alias: impl Fn(usize),
) -> EventResult {
    if ctx.window_edit.read().is_some() {
        match event {
            Event::Paste(text) => {
                let mut draft = ctx.window_edit.write();
                let value = draft.as_mut().expect("active context window editor");
                if text.bytes().all(|byte| byte.is_ascii_digit()) && value.len() + text.len() <= 10
                {
                    value.push_str(&text);
                } else {
                    crate::chatgpt_auth::notify_key("model-panel-window-invalid", 4);
                }
            }
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => *ctx.window_edit.write() = None,
                KeyCode::Enter => {
                    let value = ctx
                        .window_edit
                        .read()
                        .as_ref()
                        .and_then(|text| text.parse::<u32>().ok())
                        .filter(|value| *value > 0);
                    match value {
                        Some(value) if set_context_window(&ctx.window_target.read(), value) => {
                            *ctx.window_edit.write() = None;
                        }
                        Some(_) => {}
                        None => crate::chatgpt_auth::notify_key("model-panel-window-invalid", 4),
                    }
                }
                KeyCode::Backspace => {
                    ctx.window_edit
                        .write()
                        .as_mut()
                        .expect("active context window editor")
                        .pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    ctx.window_edit
                        .write()
                        .as_mut()
                        .expect("active context window editor")
                        .clear();
                }
                KeyCode::Char(digit) if digit.is_ascii_digit() => {
                    let mut draft = ctx.window_edit.write();
                    let value = draft.as_mut().expect("active context window editor");
                    if value.len() < 10 {
                        value.push(digit);
                    }
                }
                _ => {}
            },
            _ => {}
        }
        *ctx.render_version.write() += 1;
        return EventResult::Consumed;
    }
    // 鼠标：区域内左键点击左侧 profile 卡片行 = 选中并切换（click as enter）；
    // 右栏 K/V 行 = 聚焦该字段并执行与键盘相同的行为；底部按钮 = Ctrl+L。
    if let Event::Mouse(mouse) = event {
        if let Some(area) = ctx.area {
            if hit_provider_button(&mouse, area) {
                open_provider_management(&ctx.selected_alias.read().clone());
                *ctx.render_version.write() += 1;
                return EventResult::Consumed;
            }
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                let offset = ctx.right_scroll.read().offset().y;
                if let Some(PaneHit::Field(field)) =
                    hit_right_pane(&kv_row_hits(FIELD_COUNT), &mouse, area, offset)
                {
                    *ctx.right_cursor.write() = field;
                    *ctx.right_focus.write() = true;
                    if field == FIELD_CONTEXT_WINDOW {
                        begin_window_edit(ctx, &ctx.selected_alias.read().clone());
                    } else {
                        edit_field(ctx.selected_alias.read().clone(), field, true);
                    }
                    *ctx.render_version.write() += 1;
                    return EventResult::Consumed;
                }
            }
            if mouse.column < left_column_end(area)
                && let Some(idx) = hit_item(&mouse, area, ctx.viewport.hit_layout())
            {
                *ctx.selected_alias.write() = PROFILE_KEYS[idx].to_owned();
                switch_alias(idx);
                *ctx.render_version.write() += 1;
                return EventResult::Consumed;
            }
        }
        // 面板区域内未命中行/右侧 K/V 区：消费防穿透（与其它面板一致）
        return match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => EventResult::Consumed,
            _ => EventResult::Ignored,
        };
    }
    let Event::Key(key) = event else {
        return EventResult::Ignored;
    };
    if key.kind != KeyEventKind::Press {
        return EventResult::Ignored;
    }
    // 所有编辑动作只跟随面板选中，激活失败不阻止修复未完成档位。
    let current_alias = ctx.selected_alias.read().clone();
    match key.code {
        // “管理 Provider”：经面板互斥规则打开 Login，并留下一次性目标身份。
        // 不复制配置编辑状态；Login 自己从权威配置构建草稿。
        KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            open_provider_management(&current_alias);
        }
        KeyCode::Esc => {
            if *ctx.right_focus.read() {
                // 退出右侧编辑焦点
                *ctx.right_focus.write() = false;
            } else {
                // 全局 Esc 由 panel_overlay 处理；此处返回 Ignored 让其关闭面板
                return EventResult::Ignored;
            }
        }
        KeyCode::Up => {
            if *ctx.right_focus.read() {
                let mut c = ctx.right_cursor.write();
                *c = previous_selection(*c);
            } else {
                // 从面板选中档位出发上移（与渲染高亮一致）
                let idx = PROFILE_KEYS
                    .iter()
                    .position(|k| *k == current_alias)
                    .unwrap_or(1);
                let next = previous_selection(idx);
                *ctx.selected_alias.write() = PROFILE_KEYS[next].to_owned();
                switch_alias(next);
                *ctx.render_version.write() += 1;
            }
        }
        KeyCode::Down => {
            if *ctx.right_focus.read() {
                let mut c = ctx.right_cursor.write();
                *c = next_selection(*c, FIELD_COUNT);
            } else {
                // 从面板选中档位出发下移（与渲染高亮一致）
                let idx = PROFILE_KEYS
                    .iter()
                    .position(|k| *k == current_alias)
                    .unwrap_or(1);
                let next = next_selection(idx, PROFILE_KEYS.len());
                *ctx.selected_alias.write() = PROFILE_KEYS[next].to_owned();
                switch_alias(next);
                *ctx.render_version.write() += 1;
            }
        }
        KeyCode::Tab => {
            // 左右焦点切换：左侧 → 右侧，右侧 → 左侧
            let rf = *ctx.right_focus.read();
            *ctx.right_focus.write() = !rf;
        }
        KeyCode::Right => {
            if *ctx.right_focus.read() {
                if *ctx.right_cursor.read() == FIELD_CONTEXT_WINDOW {
                    begin_window_edit(ctx, &current_alias);
                } else {
                    edit_field(current_alias.clone(), *ctx.right_cursor.read(), true);
                }
                *ctx.render_version.write() += 1;
            } else {
                // 进入右侧编辑焦点
                *ctx.right_focus.write() = true;
            }
        }
        KeyCode::Left => {
            if *ctx.right_focus.read() {
                if *ctx.right_cursor.read() == FIELD_CONTEXT_WINDOW {
                    begin_window_edit(ctx, &current_alias);
                } else {
                    edit_field(current_alias.clone(), *ctx.right_cursor.read(), false);
                }
                *ctx.render_version.write() += 1;
            } else {
                *ctx.right_focus.write() = false;
            }
        }
        KeyCode::Enter if !*ctx.right_focus.read() => {
            switch_alias(active_profile_idx(&current_alias));
            *ctx.render_version.write() += 1;
        }
        KeyCode::Enter
            if *ctx.right_focus.read() && *ctx.right_cursor.read() == FIELD_CONTEXT_WINDOW =>
        {
            begin_window_edit(ctx, &current_alias);
        }
        _ => {}
    }
    EventResult::Consumed
}

fn begin_window_edit(ctx: &ModelEventCtx, alias: &str) {
    let value = PERI_CONFIG_HANDLE
        .get()
        .and_then(|handle| super::effective_context_window(&handle.read(), alias));
    *ctx.window_target.write() = alias.to_owned();
    *ctx.window_edit.write() = Some(value.map(|window| window.to_string()).unwrap_or_default());
}

/// 打开 Login 面板管理 Provider，并传递一次性目标身份。
fn open_provider_management(alias: &str) {
    let provider_id = PERI_CONFIG_HANDLE.get().and_then(|handle| {
        let cfg = handle.read();
        effective_provider_id(&cfg, alias)
    });
    *LOGIN_PANEL_REQUEST.state().write() = Some(LoginPanelRequest { provider_id });
    crate::kit::panel_registry::open_panel(PanelKind::Login);
}

/// 当前激活档位（配置事实源 `PERI_CONFIG_HANDLE`）——会话状态栏的
/// `SERVICE_SNAPSHOT` 可能仍是另一档模型，不参与本面板的选择与编辑。
pub(super) fn current_active_alias() -> String {
    PERI_CONFIG_HANDLE
        .get()
        .map(|h| h.read().config.active_alias.clone())
        .unwrap_or_else(|| PROFILE_KEYS[1].to_string())
}

/// `active_alias` 在静态档位表里的下标——左栏高亮、键盘导航起点与视口跟随的
/// 唯一推导处（未知 alias 落在 sonnet，与默认档位一致）。
pub(super) fn active_profile_idx(active_alias: &str) -> usize {
    PROFILE_KEYS
        .iter()
        .position(|key| *key == active_alias)
        .unwrap_or(1)
}

/// 左栏（profile 卡片）命中的列上界（不含）：主区宽 45%（`panel_shell` 左右边框
/// 各 1 列）；ScrollView 滚动条占其最右 1 列，落在该列的点击不参与列表命中。
pub(super) fn left_column_end(area: Rect) -> u16 {
    let left_w = (area.width.saturating_sub(2)) * 45 / 100;
    area.x
        .saturating_add(1)
        .saturating_add(left_w)
        .saturating_sub(1)
}

/// 事件处理上下文：本帧视口 + 面板句柄 + 面板区域。
///
/// `State<T>` 无公开构造 API，只能从组件挂载取得；集中成一个结构体，事件入口
/// 签名不再随状态/视口增长（渲染、鼠标命中与滚轮仲裁必须共用同一份区域与视口）。
pub(super) struct ModelEventCtx {
    pub(super) area: Option<Rect>,
    pub(super) right_cursor: State<usize>,
    pub(super) right_focus: State<bool>,
    pub(super) window_edit: State<Option<String>>,
    pub(super) window_target: State<String>,
    pub(super) selected_alias: State<String>,
    pub(super) render_version: State<u64>,
    /// 右栏滚动偏移——内联视图命中与绘制共用同一本帧几何。
    pub(super) right_scroll: State<ScrollViewState>,
    pub(super) viewport: ListViewport,
}
