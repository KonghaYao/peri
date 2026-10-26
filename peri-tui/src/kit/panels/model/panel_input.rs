//! Model panel event handling: hit geometry, keyboard/mouse actions and the
//! provider/draft state transitions they trigger.
//!
//! The component in `model.rs` owns hooks and rendering only. `State<T>` has no
//! public constructor, so the event context below is built by the component and
//! handed to [`handle_model_event`]; tests capture the same handles through the
//! mount probe.

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{
    LOGIN_PANEL_REQUEST, LoginPanelRequest, NOTIFICATION, Notification, PERI_CONFIG_HANDLE,
};
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

use super::edit::{edit_field, submit_profile_draft, submit_window_edit, switch_active_alias};
use super::editor_view::{PaneHit, kv_row_hits};
use super::panel_render::{profile_draft_layout, provider_button_label, window_editor_layout};
use super::profile_draft::{
    self, DraftFocus, ProfileDraftState, WindowEditState, effective_provider_id, provider_by_id,
};
use super::{FIELD_CONTEXT_1M, FIELD_COUNT, FIELD_MODEL, FIELD_PROVIDER, PROFILE_KEYS};

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
/// `hits` 是逐行命中表：普通 K/V 行用 [`kv_row_hits`]，内联 editor / 档位草稿用
/// `PaneLayout::hits`——两者都与本帧绘制行同源。
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
/// 注入只为测试：单测进程没有 `CONFIG_SOURCE_HANDLE`，真实切换必然保存失败，
/// 因此「点击哪一行 → 切换哪个档位」只能在动作出口可断言；真实分支由人工/E2E 验证。
pub(super) fn handle_model_event_with(
    event: Event,
    ctx: &ModelEventCtx,
    switch_alias: impl Fn(usize),
) -> EventResult {
    // 内联 editor / 档位草稿各自独占事件：方向键不得切换 active alias。
    if ctx.profile_draft.read().is_some() {
        return handle_profile_draft_event(event, ctx);
    }
    if ctx.window_editor.read().is_some() {
        return handle_window_editor_event(event, ctx);
    }

    // 鼠标：区域内左键点击左侧 profile 卡片行 = 选中并切换（click as enter）；
    // 右栏 K/V 行 = 聚焦该字段并执行与键盘相同的行为；底部按钮 = Ctrl+L。
    if let Event::Mouse(mouse) = event {
        if let Some(area) = ctx.area {
            if hit_provider_button(&mouse, area) {
                open_provider_management(&current_active_alias());
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
                    activate_right_field(ctx, &current_active_alias(), true);
                    *ctx.render_version.write() += 1;
                    return EventResult::Consumed;
                }
            }
            if mouse.column < left_column_end(area)
                && let Some(idx) = hit_item(&mouse, area, ctx.viewport.hit_layout())
            {
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
    // 编辑动作按 `active_alias` 定位档位（配置事实源；不从列表下标推导）
    let current_alias = current_active_alias();
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
        KeyCode::Enter => {
            if *ctx.right_focus.read() && *ctx.right_cursor.read() == FIELD_CONTEXT_1M {
                enter_window_editor(ctx, &current_alias);
            }
        }
        KeyCode::Up => {
            if *ctx.right_focus.read() {
                let mut c = ctx.right_cursor.write();
                *c = previous_selection(*c);
            } else {
                // 从当前 active 档位出发上移（与渲染高亮一致）
                let idx = PROFILE_KEYS
                    .iter()
                    .position(|k| *k == current_alias)
                    .unwrap_or(1);
                switch_active_alias(previous_selection(idx));
                *ctx.render_version.write() += 1;
            }
        }
        KeyCode::Down => {
            if *ctx.right_focus.read() {
                let mut c = ctx.right_cursor.write();
                *c = next_selection(*c, FIELD_COUNT);
            } else {
                // 从当前 active 档位出发下移（与渲染高亮一致）
                let idx = PROFILE_KEYS
                    .iter()
                    .position(|k| *k == current_alias)
                    .unwrap_or(1);
                switch_active_alias(next_selection(idx, PROFILE_KEYS.len()));
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
                activate_right_field(ctx, &current_alias, true);
                *ctx.render_version.write() += 1;
            } else {
                // 进入右侧编辑焦点
                *ctx.right_focus.write() = true;
            }
        }
        KeyCode::Left => {
            if *ctx.right_focus.read() {
                activate_right_field(ctx, &current_alias, false);
                *ctx.render_version.write() += 1;
            } else {
                *ctx.right_focus.write() = false;
            }
        }
        _ => {}
    }
    EventResult::Consumed
}

/// 右侧字段动作：Responses provider/model/window 进入草稿，其余沿用即时保存。
fn activate_right_field(ctx: &ModelEventCtx, alias: &str, forward: bool) {
    let field = *ctx.right_cursor.read();
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    let cfg = handle.read().clone();
    let provider = effective_provider_id(&cfg, alias)
        .and_then(|provider_id| provider_by_id(&cfg, &provider_id).cloned());
    let Some(provider) = provider else {
        edit_field(alias.to_string(), field, forward);
        return;
    };
    match field {
        FIELD_PROVIDER => {
            let Some(next_id) = profile_draft::next_provider_id(&cfg, &provider.id, forward) else {
                return;
            };
            let next_is_responses = provider_by_id(&cfg, &next_id)
                .is_some_and(|provider| provider.provider_type == "openai_responses");
            if provider.provider_type == "openai_responses" || next_is_responses {
                enter_profile_draft(ctx, alias, Some(next_id), None);
            } else {
                edit_field(alias.to_string(), field, forward);
            }
        }
        FIELD_MODEL => {
            if provider.provider_type == "openai_responses" {
                let current = profile_draft::actual_model(&cfg, alias);
                let next =
                    profile_draft::next_model_name(&cfg, alias, &provider.id, &current, forward);
                enter_profile_draft(ctx, alias, Some(provider.id.clone()), next);
            } else {
                edit_field(alias.to_string(), field, forward);
            }
        }
        FIELD_CONTEXT_1M if provider.provider_type == "openai_responses" => {
            enter_window_editor(ctx, alias);
        }
        _ => edit_field(alias.to_string(), field, forward),
    }
}

fn enter_window_editor(ctx: &ModelEventCtx, alias: &str) {
    refresh_before_editor();
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    let revision = crate::config::configuration_revision().ok();
    let cfg = handle.read();
    if let Some(state) = WindowEditState::enter(&cfg, alias, revision) {
        *ctx.window_editor.write() = Some(state);
        *ctx.right_focus.write() = true;
    }
}

/// 进入长草稿/内联 editor 前重读权威配置：expected revision 与基线都来自最新快照，
/// 因而“Esc 后重新进入”是真实可用的 revision 冲突恢复路径。
fn refresh_before_editor() {
    if crate::kit::atoms::CONFIG_SOURCE_HANDLE.get().is_none() {
        return;
    }
    if let Err(error) = crate::kit::config_service::reload_configuration() {
        crate::kit::config_service::notify_reload_failed(&error);
    }
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

fn notify_transient(key: &str, seconds: u64) {
    *NOTIFICATION.state().write() = Some(Notification {
        message: i18n::tr(key),
        until: peri_time::monotonic_now() + std::time::Duration::from_secs(seconds),
    });
}

fn enter_profile_draft(
    ctx: &ModelEventCtx,
    alias: &str,
    provider_id: Option<String>,
    model: Option<String>,
) {
    refresh_before_editor();
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    let revision = crate::config::configuration_revision().ok();
    let cfg = handle.read();
    if let Some(state) = ProfileDraftState::enter(&cfg, alias, revision, provider_id, model) {
        *ctx.profile_draft.write() = Some(state);
        *ctx.right_focus.write() = true;
    }
}

fn handle_window_editor_event(event: Event, ctx: &ModelEventCtx) -> EventResult {
    let mut guard = ctx.window_editor.write();
    let Some(editor) = guard.as_mut() else {
        return EventResult::Ignored;
    };
    if let Event::Mouse(mouse) = event {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            // 滚轮/抬起等交给 ScrollView 与面板滚动仲裁。
            return EventResult::Ignored;
        }
        if let Some(area) = ctx.area {
            // 底部按钮与 Ctrl+L 等价：草稿未提交时只提示先保存或取消。
            if hit_provider_button(&mouse, area) {
                notify_transient("model-panel-draft-exit-first", 3);
                *ctx.render_version.write() += 1;
                return EventResult::Consumed;
            }
            let layout = window_editor_layout(editor);
            let offset = ctx.right_scroll.read().offset().y;
            if let Some(hit) = hit_right_pane(&layout.hits, &mouse, area, offset) {
                match hit {
                    PaneHit::Save => {
                        if submit_window_edit(editor) {
                            *guard = None;
                        }
                    }
                    PaneHit::Cancel => *guard = None,
                    PaneHit::Field(_) | PaneHit::Provider | PaneHit::Model | PaneHit::Window => {}
                }
            }
        }
        *ctx.render_version.write() += 1;
        return EventResult::Consumed;
    }
    if let Event::Paste(text) = event {
        // 单行数值字段：多行/控制字符/非数字整段拒绝，保留原文本并显示既有错误。
        editor.paste(&text);
        *ctx.render_version.write() += 1;
        return EventResult::Consumed;
    }
    let Event::Key(key) = event else {
        return EventResult::Consumed;
    };
    if key.kind != KeyEventKind::Press {
        return EventResult::Consumed;
    }
    let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let mut reload_conflict = false;
    let mut exit_editor_first = false;
    match key.code {
        KeyCode::Esc => *guard = None,
        KeyCode::Enter => {
            if submit_window_edit(editor) {
                *guard = None;
            }
        }
        KeyCode::Char('s') if is_ctrl => {
            if submit_window_edit(editor) {
                *guard = None;
            }
        }
        // 冲突提示指向的操作：重读权威配置并离开本 editor，随后重新进入即可用新
        // revision 保存。非冲突输入错误不提供此动作。
        KeyCode::Char('r') if is_ctrl && editor.conflict => reload_conflict = true,
        KeyCode::Char('l') if is_ctrl => exit_editor_first = true,
        KeyCode::Char('w') if is_ctrl => editor.word_backspace(),
        KeyCode::Char(ch) if !is_ctrl => {
            editor.insert(ch);
        }
        KeyCode::Backspace if !is_ctrl => editor.backspace(),
        KeyCode::Delete => editor.delete(),
        KeyCode::Left if !is_ctrl => editor.move_left(),
        KeyCode::Right if !is_ctrl => editor.move_right(),
        KeyCode::Home if !is_ctrl => editor.home(),
        KeyCode::End if !is_ctrl => editor.end(),
        // 方向键不切换 active alias；其余键不穿透。
        _ => {}
    }
    if reload_conflict {
        match crate::kit::config_service::reload_configuration() {
            Ok(_) => {
                *guard = None;
                crate::kit::config_service::notify_reloaded();
            }
            Err(error) => crate::kit::config_service::notify_reload_failed(&error),
        }
    }
    if exit_editor_first {
        notify_transient("model-panel-draft-exit-first", 3);
    }
    *ctx.render_version.write() += 1;
    EventResult::Consumed
}

fn handle_profile_draft_event(event: Event, ctx: &ModelEventCtx) -> EventResult {
    let mut guard = ctx.profile_draft.write();
    let Some(draft) = guard.as_mut() else {
        return EventResult::Ignored;
    };
    if let Event::Mouse(mouse) = event {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            // 滚轮/抬起等交给 ScrollView 与面板滚动仲裁。
            return EventResult::Ignored;
        }
        let mut save = false;
        let mut close = false;
        if let Some(area) = ctx.area {
            // 底部按钮与 Ctrl+L 等价：草稿未提交时只提示先保存或取消。
            if hit_provider_button(&mouse, area) {
                notify_transient("model-panel-draft-exit-first", 3);
                *ctx.render_version.write() += 1;
                return EventResult::Consumed;
            }
            let layout = profile_draft_layout(draft);
            let offset = ctx.right_scroll.read().offset().y;
            if let Some(hit) = hit_right_pane(&layout.hits, &mouse, area, offset) {
                match hit {
                    // 点击 choice 行与键盘 Enter/→ 语义一致：聚焦并切换。
                    PaneHit::Provider => {
                        move_draft_focus(draft, DraftFocus::Provider);
                        draft.cycle_provider(true);
                    }
                    // 键盘 Enter：未编辑时进入文本编辑，编辑中则确认并落到 Save。
                    PaneHit::Model => {
                        if draft.model_editing {
                            draft.confirm_model_edit();
                            draft.focus = DraftFocus::Save;
                        } else {
                            move_draft_focus(draft, DraftFocus::Model);
                            draft.begin_model_edit();
                        }
                    }
                    PaneHit::Window => move_draft_focus(draft, DraftFocus::Window),
                    // 与键盘 Save/Ctrl+S 共用 submit_profile_draft（校验、冲突与错误保持同源）。
                    PaneHit::Save => {
                        move_draft_focus(draft, DraftFocus::Save);
                        save = true;
                    }
                    PaneHit::Cancel => close = true,
                    PaneHit::Field(_) => {}
                }
            }
        }
        if save && submit_profile_draft(draft) {
            close = true;
        }
        if close {
            *guard = None;
        }
        *ctx.render_version.write() += 1;
        return EventResult::Consumed;
    }
    if let Event::Paste(text) = event {
        // 单行字段：多行/控制字符整段拒绝并显示既有错误；choice/按钮不接收粘贴。
        match draft.focus {
            DraftFocus::Window => {
                draft.paste(&text);
            }
            DraftFocus::Model if draft.model_editing => {
                draft.paste_model(&text);
            }
            _ => {}
        };
        *ctx.render_version.write() += 1;
        return EventResult::Consumed;
    }
    let Event::Key(key) = event else {
        return EventResult::Consumed;
    };
    if key.kind != KeyEventKind::Press {
        return EventResult::Consumed;
    }
    let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let mut close = false;
    let mut save = false;
    let mut reload_conflict = false;
    let mut exit_draft_first = false;
    match key.code {
        KeyCode::Esc => {
            if draft.model_editing {
                draft.cancel_model_edit();
            } else {
                close = true;
            }
        }
        KeyCode::Enter => match draft.focus {
            DraftFocus::Save => save = true,
            DraftFocus::Cancel => close = true,
            DraftFocus::Provider => move_draft_focus(draft, DraftFocus::Model),
            DraftFocus::Model => {
                if draft.model_editing {
                    // 确认文本后进入 Save：容量也需显式确认，三字段一起原子保存。
                    draft.confirm_model_edit();
                    draft.focus = DraftFocus::Save;
                } else {
                    draft.begin_model_edit();
                }
            }
            DraftFocus::Window => save = true,
        },
        KeyCode::Char(' ') if !is_ctrl => {
            if draft.focus == DraftFocus::Model && draft.model_editing {
                draft.insert_model(' ');
            } else {
                match draft.focus {
                    DraftFocus::Provider => draft.cycle_provider(true),
                    DraftFocus::Model => draft.cycle_model(true),
                    DraftFocus::Save => save = true,
                    DraftFocus::Cancel => close = true,
                    DraftFocus::Window => {}
                }
            }
        }
        KeyCode::Char('s') if is_ctrl => save = true,
        KeyCode::Char('r') if is_ctrl && draft.conflict => reload_conflict = true,
        KeyCode::Char('l') if is_ctrl => exit_draft_first = true,
        KeyCode::Char('w') if is_ctrl && draft.focus == DraftFocus::Window => {
            draft.word_backspace()
        }
        KeyCode::Char('w')
            if is_ctrl && draft.focus == DraftFocus::Model && draft.model_editing =>
        {
            draft.word_backspace_model()
        }
        KeyCode::Char(ch) if !is_ctrl && draft.focus == DraftFocus::Window => {
            draft.insert(ch);
        }
        KeyCode::Char(ch)
            if !is_ctrl && draft.focus == DraftFocus::Model && draft.model_editing =>
        {
            draft.insert_model(ch);
        }
        KeyCode::Up | KeyCode::BackTab => move_draft_focus(draft, draft.focus.previous()),
        KeyCode::Down | KeyCode::Tab => move_draft_focus(draft, draft.focus.next()),
        KeyCode::Left if !is_ctrl => match draft.focus {
            DraftFocus::Provider => draft.cycle_provider(false),
            DraftFocus::Model if draft.model_editing => draft.model_move_left(),
            DraftFocus::Model => draft.cycle_model(false),
            DraftFocus::Window => draft.move_left(),
            DraftFocus::Save | DraftFocus::Cancel => {}
        },
        KeyCode::Right if !is_ctrl => match draft.focus {
            DraftFocus::Provider => draft.cycle_provider(true),
            DraftFocus::Model if draft.model_editing => draft.model_move_right(),
            DraftFocus::Model => draft.cycle_model(true),
            DraftFocus::Window => draft.move_right(),
            DraftFocus::Save | DraftFocus::Cancel => {}
        },
        KeyCode::Backspace if !is_ctrl && draft.focus == DraftFocus::Window => draft.backspace(),
        KeyCode::Backspace
            if !is_ctrl && draft.focus == DraftFocus::Model && draft.model_editing =>
        {
            draft.backspace_model()
        }
        KeyCode::Delete if draft.focus == DraftFocus::Window => draft.delete(),
        KeyCode::Delete if draft.focus == DraftFocus::Model && draft.model_editing => {
            draft.delete_model()
        }
        KeyCode::Home if !is_ctrl && draft.focus == DraftFocus::Window => draft.home(),
        KeyCode::Home if !is_ctrl && draft.focus == DraftFocus::Model && draft.model_editing => {
            draft.model_home()
        }
        KeyCode::End if !is_ctrl && draft.focus == DraftFocus::Window => draft.end(),
        KeyCode::End if !is_ctrl && draft.focus == DraftFocus::Model && draft.model_editing => {
            draft.model_end()
        }
        _ => {}
    }
    if save && submit_profile_draft(draft) {
        close = true;
    }
    if close {
        *guard = None;
    }
    if reload_conflict {
        match crate::kit::config_service::reload_configuration() {
            Ok(_) => {
                *guard = None;
                crate::kit::config_service::notify_reloaded();
            }
            Err(error) => crate::kit::config_service::notify_reload_failed(&error),
        }
    }
    drop(guard);
    if exit_draft_first {
        notify_transient("model-panel-draft-exit-first", 3);
    }
    *ctx.render_version.write() += 1;
    EventResult::Consumed
}

/// 离开 Model 字段时确认正在编辑的文本，避免焦点移动后仍处于半编辑状态。
fn move_draft_focus(draft: &mut ProfileDraftState, next: DraftFocus) {
    if draft.focus == DraftFocus::Model && draft.model_editing && next != DraftFocus::Model {
        draft.confirm_model_edit();
    }
    draft.focus = next;
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
    pub(super) render_version: State<u64>,
    pub(super) window_editor: State<Option<WindowEditState>>,
    pub(super) profile_draft: State<Option<ProfileDraftState>>,
    /// 右栏滚动偏移——内联视图命中与绘制共用同一本帧几何。
    pub(super) right_scroll: State<ScrollViewState>,
    pub(super) viewport: ListViewport,
}
