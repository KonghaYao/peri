//! ratatui-kit LoginPanel component.
//!
//! Login 是宿主配置的 Provider 编辑器：Browse 列表来自 `PROVIDER_LIST` 投影，
//! Edit 使用 typed 草稿（三态类型、API key / 既有 ChatGPT plan 引用、四档
//! profile model/contextWindow）。保存捕获进入时的 revision，只发布 authority
//! 接纳后的快照；失败保持草稿与字段错误。
//!
//! Browse：只读列表 + Enter/鼠标点击进入编辑。
//! Edit：动态字段导航、Ctrl+S/Confirm 保存、Esc 放弃；已有 provider 的类型
//! 只有显式确认子视图才能更改（方向键不会误改）。Ctrl+R 重读权威配置并重新
//! 基线化草稿，是 revision 冲突后唯一可用的面板内恢复动作。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{
    AUTH_EVENT_REVISION, LOGIN_PANEL_REQUEST, NOTIFICATION, Notification, PROVIDER_LIST,
    ProviderSummary,
};
use crate::kit::list_nav::{next_selection, previous_selection, scroll_start_for_selected};
use crate::kit::panel_mouse::{AreaTracker, is_scrollbar_column};
use fluent_bundle::FluentValue;

use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    },
    prelude::*,
    ratatui::{
        layout::{Constraint, Position, Rect},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};
use std::time::Duration;

mod auth;
mod auth_render;
mod auth_ui;
mod config_store;
mod edit_handler;
mod edit_state;
mod edit_types;
mod plan;
mod render;

use self::auth::AuthController;
use self::auth_ui::{
    apply_auth_effects, execute_auth_detail, execute_edit_intent, execute_logout_focus,
    handle_auth_detail_key, handle_logout_confirm_key, hit_auth_detail_button, hit_logout_button,
    sync_auth_scope,
};
use self::config_store::delete_provider;
use self::edit_handler::{
    LoginEditContext, default_credential_store, enter_login_edit_mode, enter_login_new_mode,
    handle_login_edit_keys, handle_login_paste, load_latest_configuration,
};
use self::edit_state::{LoginEditState, LoginField};
use self::render::{EditHit, make_hint_line_for_login};

// ── Login 编辑模式类型 ─────────────────────────────────────────────────────────

/// Login 面板操作模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginPanelMode {
    /// 浏览模式：上下导航 + Enter/鼠标点击编辑 + Ctrl+N/Ctrl+D 新建/删除
    Browse,
    /// 编辑模式：文本编辑 + 字段导航 + Ctrl+S 保存 + Esc 放弃
    Edit,
    /// 类型更改确认：方向键只改候选，Enter 确认，Esc 保持原值
    ConfirmTypeChange,
    /// 认证授权子视图：完整 URL、打开浏览器、复制、取消
    AuthDetail,
    /// 注销确认：默认取消，固定 store/account
    ConfirmLogout,
    /// 删除确认模式：Enter 确认删除 + Esc 取消
    ConfirmDelete,
}

// ── 主组件 ────────────────────────────────────────────────────────────────────

/// Browse 列表视口常量（渲染、命中与翻页共用）
const VISIBLE_ITEMS: usize = 3;

#[component]
pub fn LoginPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // ── hooks：无条件、稳定顺序（TUI-HOOK-001）──
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let cursor = hooks.use_state(|| 0usize);
    // 外部滚动状态——面板滚轮仲裁（panel_scroll.rs）驱动，统一 3 行/格 + 节流
    let sv = hooks.use_state(ScrollViewState::default);
    let mode = hooks.use_state(|| LoginPanelMode::Browse);
    let edit_state = hooks.use_state(|| None::<LoginEditState>);
    let edit_focus = hooks.use_state(|| LoginField::ProviderType);
    let edit_cursor = hooks.use_state(|| 0usize);
    let auth = hooks.use_state(AuthController::new);
    let auth_atom = hooks.use_atom(&AUTH_EVENT_REVISION);
    let auth_revision = *auth_atom.read();
    let _ = hooks.use_atom(&crate::kit::atoms::LANG_VERSION);
    let store = hooks.use_atom(&PROVIDER_LIST);
    let providers: Vec<ProviderSummary> = store.read().clone();
    let _ = store;
    let count = providers.len();
    // 删除确认门的目标身份（provider id，进入确认时冻结）——确认期间列表刷新不改变目标
    let confirm_delete_target = hooks.use_state(|| None::<String>);

    // 面板绘制区域（上一帧）——鼠标点击行号反推
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }
    // 面板上一帧区域（滚轮仲裁注册用）——hook 前置，渲染段不再调用 hook
    let previous_size = hooks.use_previous_size();

    // Browse 列表视口常量（与渲染共用）
    let browse_scroll_start = scroll_start_for_selected(*cursor.read(), count, VISIBLE_ITEMS);

    // Edit 模式缺少编辑状态（状态异常）时回退到 Browse：写在 effect 边界而不是渲染体
    // （TUI-RENDER-001），且发生在渲染之前，本帧即渲染 Browse 视图（时机不变）
    let edit_state_missing = matches!(
        *mode.read(),
        LoginPanelMode::Edit
            | LoginPanelMode::ConfirmTypeChange
            | LoginPanelMode::AuthDetail
            | LoginPanelMode::ConfirmLogout
    ) && edit_state.read().is_none();
    hooks.use_effect(
        move || fallback_to_browse(mode, edit_state_missing),
        (edit_state_missing,),
    );

    // 认证后台任务只递增 AUTH_EVENT_REVISION 并投递 inbox；这里的 effect 是唯一
    // 把事件过完成门、写回面板状态的地方。
    hooks.use_effect(
        move || apply_auth_effects(auth, mode, edit_state, edit_focus, edit_cursor),
        (auth_revision,),
    );
    let auth_idle = matches!(*mode.read(), LoginPanelMode::Browse) && edit_state.read().is_none();
    hooks.use_effect(
        move || {
            if auth_idle {
                auth.write().shutdown();
            }
        },
        (auth_idle,),
    );

    // 打开面板即重读权威配置：Browse 列表与后续 revision 基线都来自最新快照。
    // 无配置源（单测/lenient 启动）时保持已发布值。
    hooks.use_effect(
        move || {
            if crate::kit::atoms::CONFIG_SOURCE_HANDLE.get().is_some() {
                load_latest_configuration();
            }
        },
        (),
    );

    // Model 面板的一次性“管理 Provider”请求：打开 Login 后按目标身份进入编辑。
    // 请求本身不携带配置草稿，编辑状态始终由 Login 从权威配置重新构建。
    let login_request = hooks.use_atom(&LOGIN_PANEL_REQUEST);
    let request_pending = login_request.read().clone();
    let request_dependency = request_pending.clone();
    hooks.use_effect(
        move || apply_login_request(mode, edit_state, edit_focus, edit_cursor, request_pending),
        (request_dependency,),
    );

    let event_ctx = LoginEventCtx {
        area,
        cursor,
        mode,
        edit_state,
        edit_focus,
        edit_cursor,
        confirm_delete_target,
        auth,
        sv,
        providers: providers.clone(), // 本帧列表快照：命中与动作目标同源
        scroll_start: browse_scroll_start,
    };
    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::Normal,
        EventOptions { hit_test: true },
        move |event| handle_login_event(event, &event_ctx),
    );

    #[cfg(test)]
    tests::visit_mounted_panel(tests::MountedPanel {
        cursor,
        mode,
        edit_state,
        edit_focus,
        edit_cursor,
        confirm_delete_target,
        auth,
        sv,
    });

    // ── 渲染段（只读；状态写入都在上面的 effect / 事件边界，TUI-RENDER-001）──
    let sel = *cursor.read();
    let semantic = theme_def.read().semantic;
    let cursor_color = semantic.status.warning;
    let dim = semantic.text.dim;
    let text_color = semantic.text.primary;
    let focus_color = semantic.status.success;
    let muted = semantic.text.muted;

    let mut lines: Vec<Line<'_>> = Vec::new();

    match *mode.read() {
        LoginPanelMode::Browse => {
            // S16：spec/global/domains/tui/tui-panels.md §6.2 样式——Enter::select · Esc::close
            lines.push(Line::from(vec![Span::styled(
                format!("  {} providers configured", count),
                Style::new().fg(text_color).bold(),
            )]));
            lines.push(Line::from(""));

            if providers.is_empty() {
                lines.push(Line::from(vec![Span::styled(
                    format!("  {}", i18n::tr("login-empty-hint")),
                    Style::new().fg(muted),
                )]));
            } else {
                // 视口跟随
                for (i, p) in providers
                    .iter()
                    .enumerate()
                    .skip(browse_scroll_start)
                    .take(VISIBLE_ITEMS)
                {
                    let is_cursor = i == sel;
                    let cursor_mark = if is_cursor { ">" } else { " " };
                    let row_style = if is_cursor {
                        Style::new()
                            .fg(theme_def.read().component.panel.title)
                            .bold()
                    } else {
                        Style::new().fg(text_color)
                    };

                    lines.push(Line::from(vec![
                        Span::styled(
                            format!(" {} ", cursor_mark),
                            Style::new().fg(theme_def.read().component.panel.title),
                        ),
                        Span::styled(
                            format!(
                                "{}  ({})",
                                p.id,
                                render::provider_type_display(&p.provider_type)
                            ),
                            row_style,
                        ),
                    ]));

                    let (key_marker, key_color) = if p.has_chatgpt_plan {
                        (i18n::tr("login-browse-plan-unchecked"), semantic.text.muted)
                    } else if p.has_api_key {
                        ("api key: configured".to_string(), semantic.status.success)
                    } else {
                        ("api key: missing".to_string(), semantic.status.error)
                    };
                    lines.push(Line::from(vec![Span::styled(
                        format!("   {}", key_marker),
                        Style::new().fg(key_color),
                    )]));
                    if let Some(url) = &p.base_url {
                        let url_display: String = url.chars().take(70).collect();
                        lines.push(Line::from(vec![Span::styled(
                            format!("   base url: {}", url_display),
                            Style::new().fg(dim),
                        )]));
                    }
                    lines.push(Line::from(""));
                }
            }

            // S16：底部 hints（i18n 化）
            lines.push(Line::from(""));
            lines.push(make_hint_line_for_login(
                vec![
                    (
                        "\u{2191}/\u{2193}".to_string(),
                        i18n::tr("hint-login-browse"),
                    ),
                    ("Enter".to_string(), i18n::tr("hint-login-edit")),
                    ("Ctrl+N".to_string(), i18n::tr("hint-login-new")),
                    ("Ctrl+D".to_string(), i18n::tr("hint-login-delete")),
                    ("Esc".to_string(), i18n::tr("hint-login-close")),
                ],
                dim,
                focus_color,
            ));
        }
        LoginPanelMode::Edit => {
            let es_opt = edit_state.read();
            match es_opt.as_ref() {
                Some(es) => lines.extend(
                    render::build_edit_layout(
                        es,
                        &auth.read(),
                        *edit_focus.read(),
                        *edit_cursor.read(),
                        &render::LoginRenderColors {
                            cursor: cursor_color,
                            dim,
                            text: text_color,
                            focus: focus_color,
                        },
                    )
                    .lines,
                ),
                // 状态异常：effect 边界已在渲染前回退到 Browse；此处仅为保持 match 穷尽
                None => lines.push(Line::from(vec![Span::styled(
                    "  Internal error: invalid edit state",
                    Style::new().fg(semantic.status.error),
                )])),
            }
        }
        LoginPanelMode::AuthDetail => {
            let layout = auth_render::build_auth_detail_layout(
                &auth.read(),
                area.map(|area| area.width).unwrap_or(80),
                &render::LoginRenderColors {
                    cursor: cursor_color,
                    dim,
                    text: text_color,
                    focus: focus_color,
                },
            );
            lines.extend(layout.lines);
        }
        LoginPanelMode::ConfirmLogout => {
            let auth_guard = auth.read();
            match auth_guard.logout_confirm() {
                Some(confirm) => {
                    let layout = auth_render::build_logout_layout(
                        confirm,
                        &render::LoginRenderColors {
                            cursor: cursor_color,
                            dim,
                            text: text_color,
                            focus: focus_color,
                        },
                    );
                    lines.extend(layout.lines);
                }
                None => lines.push(Line::from(vec![Span::styled(
                    "  Internal error: missing logout target",
                    Style::new().fg(semantic.status.error),
                )])),
            }
        }
        LoginPanelMode::ConfirmTypeChange => {
            let es_opt = edit_state.read();
            match es_opt.as_ref() {
                Some(es) => lines.extend(
                    render::build_type_change_layout(
                        es,
                        &render::LoginRenderColors {
                            cursor: cursor_color,
                            dim,
                            text: text_color,
                            focus: focus_color,
                        },
                    )
                    .lines,
                ),
                None => lines.push(Line::from(vec![Span::styled(
                    "  Internal error: invalid edit state",
                    Style::new().fg(semantic.status.error),
                )])),
            }
        }
        LoginPanelMode::ConfirmDelete => {
            // 目标取确认时冻结的身份：确认期间列表刷新不改变待执行目标
            let provider_name = confirm_delete_target.read().clone().unwrap_or_default();
            let provider_name = if provider_name.is_empty() {
                "(unknown)"
            } else {
                provider_name.as_str()
            };

            lines.push(Line::from(vec![Span::styled(
                format!("  {}", i18n::tr("login-panel-title-confirm-delete")),
                Style::new().fg(semantic.status.error).bold(),
            )]));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                format!("  Provider: {}", provider_name),
                Style::new().fg(text_color),
            )]));
            lines.push(Line::from(vec![Span::styled(
                i18n::tr("login-confirm-delete-warning"),
                Style::new().fg(semantic.status.warning),
            )]));
            lines.push(Line::from(""));
            lines.push(make_hint_line_for_login(
                vec![
                    ("Enter".to_string(), i18n::tr("login-confirm-delete")),
                    ("Esc".to_string(), i18n::tr("hint-login-back")),
                ],
                dim,
                focus_color,
            ));
        }
    }

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    // 面板滚轮仲裁注册（每帧覆盖写入，area 用上一帧组件区域）
    crate::kit::panel_scroll::register_panel_scroll(PanelKind::Login, previous_size, sv);

    panel_shell!(PanelKind::Login, {
            ScrollView(
                scrollbars: crate::kit::panel_registry::clean_scrollbars(),
                state: Some(sv),
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                Text(text: content)
            }
    })
}

// ── 事件判定与面板动作 ────────────────────────────────────────────────────────

/// 面板事件处理——判定集中在这里、组件只做注册，测试经 `tests::mount_panel` 取得真实
/// 句柄后直接投喂事件（`State<T>` 无公开构造 API）。鼠标与键盘共用同一批动作函数。
fn handle_login_event(event: Event, ctx: &LoginEventCtx) -> EventResult {
    // 模式快照：`State::read()` 的读守卫不能跨过本帧的状态写入（同线程自锁）
    let current_mode = *ctx.mode.read();

    // 鼠标：区域内左键点击 = 执行对应模式的 Enter 动作（click as enter）
    if let Event::Mouse(mouse) = event {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return EventResult::Ignored;
        }
        let Some(area) = ctx.area else {
            return EventResult::Ignored;
        };
        if current_mode == LoginPanelMode::ConfirmDelete {
            confirm_delete(ctx);
            return EventResult::Consumed;
        }
        if current_mode == LoginPanelMode::AuthDetail {
            if !is_scrollbar_column(&mouse, area)
                && let Some(action) = hit_auth_detail_button(ctx, &mouse, area)
            {
                ctx.auth.write().set_detail_focus(action);
                execute_auth_detail(ctx, action);
            }
            return EventResult::Consumed;
        }
        if current_mode == LoginPanelMode::ConfirmLogout {
            if !is_scrollbar_column(&mouse, area)
                && let Some(focus) = hit_logout_button(ctx, &mouse, area)
            {
                execute_logout_focus(ctx, focus);
            }
            return EventResult::Consumed;
        }
        if current_mode == LoginPanelMode::Edit {
            if !is_scrollbar_column(&mouse, area)
                && let Some(hit) = hit_edit_target(ctx, &mouse, area)
            {
                activate_edit_hit(ctx, hit);
            }
            return EventResult::Consumed;
        }
        if current_mode == LoginPanelMode::ConfirmTypeChange {
            if !is_scrollbar_column(&mouse, area)
                && let Some(hit) = hit_type_change_target(ctx, &mouse, area)
            {
                activate_type_change_hit(ctx, hit);
            }
            return EventResult::Consumed;
        }
        if current_mode == LoginPanelMode::Browse
            && !is_scrollbar_column(&mouse, area)
            && let Some(idx) = hit_browse_item(&mouse, area, &ctx.providers, ctx.scroll_start)
        {
            *ctx.cursor.write() = idx;
            enter_edit_row(ctx);
            scroll_focus_into_view(ctx);
            return EventResult::Consumed;
        }
        // 区域内点击（未命中行 / Edit 模式）也消费，防止穿透
        return EventResult::Consumed;
    }
    // 粘贴事件：仅编辑模式的文本字段处理
    if let Event::Paste(paste_text) = &event {
        if current_mode == LoginPanelMode::Edit && ctx.edit_focus.read().is_text() {
            let field = *ctx.edit_focus.read();
            if ctx.auth.read().blocks_config_save()
                && matches!(field, LoginField::PlanStore | LoginField::PlanAccount)
            {
                self::auth::notify_key("login-auth-wait-before-edit", 3);
                return EventResult::Consumed;
            }
            let mut es_guard = ctx.edit_state.write();
            let mut ec_guard = ctx.edit_cursor.write();
            if let Some(ref mut es) = *es_guard {
                handle_login_paste(&mut ec_guard, es, field, paste_text);
            }
            drop(es_guard);
            if matches!(field, LoginField::PlanStore | LoginField::PlanAccount) {
                ctx.auth.write().invalidate_target();
            }
            return EventResult::Consumed;
        }
        return EventResult::Ignored;
    }

    let Event::Key(key) = event else {
        return EventResult::Ignored;
    };
    if key.kind != KeyEventKind::Press {
        return EventResult::Ignored;
    }

    // ConfirmDelete 模式：优先处理（不依赖 mode match）
    if current_mode == LoginPanelMode::ConfirmDelete {
        match key.code {
            KeyCode::Enter => confirm_delete(ctx),
            _ => {
                // Esc 或任意其他键：取消删除（一次性门，目标一并清空）
                *ctx.mode.write() = LoginPanelMode::Browse;
                *ctx.confirm_delete_target.write() = None;
            }
        }
        return EventResult::Consumed;
    }

    // AuthDetail / ConfirmLogout 子视图：消费全部按键，Esc 不穿透关闭面板。
    if current_mode == LoginPanelMode::AuthDetail {
        handle_auth_detail_key(ctx, &key);
        return EventResult::Consumed;
    }
    if current_mode == LoginPanelMode::ConfirmLogout {
        handle_logout_confirm_key(ctx, &key);
        return EventResult::Consumed;
    }

    match current_mode {
        LoginPanelMode::Browse => match key.code {
            KeyCode::Esc => close_panel(),
            KeyCode::Up => {
                let mut c = ctx.cursor.write();
                *c = previous_selection(*c);
            }
            KeyCode::Down => {
                let latest = PROVIDER_LIST.state().read().len();
                let mut c = ctx.cursor.write();
                if latest > 0 {
                    *c = next_selection(*c, latest);
                }
            }
            // Ctrl+N：新建 provider（进入编辑模式，空表单）
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                ctx.auth.write().shutdown();
                enter_login_new_mode(
                    &mut ctx.edit_state.write(),
                    &mut ctx.edit_focus.write(),
                    &mut ctx.edit_cursor.write(),
                );
                *ctx.mode.write() = LoginPanelMode::Edit;
            }
            // Ctrl+D：固定待删除身份后进入确认模式
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                begin_confirm_delete(ctx);
            }
            // Enter：进入编辑模式
            KeyCode::Enter => enter_edit_row(ctx),
            _ => {}
        },
        LoginPanelMode::Edit => {
            apply_login_edit_key(ctx, &key);
        }
        LoginPanelMode::ConfirmTypeChange => {
            apply_type_change_key(ctx, &key);
        }
        LoginPanelMode::AuthDetail | LoginPanelMode::ConfirmLogout => {
            // 子视图在 match 之前已处理并 return；保持穷尽性。
        }
        LoginPanelMode::ConfirmDelete => {
            // 在 match 之前已处理并 return；编译器需要匹配臂以保持穷尽性
        }
    }
    scroll_focus_into_view(ctx);
    EventResult::Consumed
}

/// 键盘 Edit 动作（键盘与鼠标共用的唯一入口）。
fn apply_login_edit_key(ctx: &LoginEventCtx, key: &KeyEvent) {
    let auth_busy = ctx.auth.read().blocks_config_save();
    let default_store = default_credential_store();
    let intent = {
        let mut m_guard = ctx.mode.write();
        let mut es_guard = ctx.edit_state.write();
        let mut ef_guard = ctx.edit_focus.write();
        let mut ec_guard = ctx.edit_cursor.write();
        let mut auth_guard = ctx.auth.write();
        let intent = handle_login_edit_keys(
            LoginEditContext {
                mode: &mut m_guard,
                edit_state: &mut es_guard,
                edit_focus: &mut ef_guard,
                edit_cursor: &mut ec_guard,
                auth: &mut auth_guard,
            },
            auth_busy,
            default_store,
            key,
        );
        if *m_guard == LoginPanelMode::Browse && es_guard.is_none() {
            auth_guard.shutdown();
        }
        intent
    };
    execute_edit_intent(ctx, intent);
}

/// 类型更改确认子视图的键盘动作（鼠标确认/切换候选共用）。
fn apply_type_change_key(ctx: &LoginEventCtx, key: &KeyEvent) {
    let mut m_guard = ctx.mode.write();
    let mut es_guard = ctx.edit_state.write();
    let mut ef_guard = ctx.edit_focus.write();
    let mut ec_guard = ctx.edit_cursor.write();
    edit_handler::handle_login_type_change_keys(
        &mut m_guard,
        &mut es_guard,
        &mut ef_guard,
        &mut ec_guard,
        key,
    );
}

/// 左键语义行 → 全量内容行（滚动偏移来自本帧同一 `ScrollViewState`）。
///
/// 只接受面板内容区：上/下边框、面板外列与右侧滚动条列都不产生行号。
fn mouse_content_row(mouse: &MouseEvent, area: Rect, scroll_offset: u16) -> Option<usize> {
    if mouse.column < area.x || mouse.column >= area.x.saturating_add(area.width) {
        return None;
    }
    if mouse.row <= area.y || mouse.row >= area.y.saturating_add(area.height.saturating_sub(1)) {
        return None;
    }
    let visual = mouse.row - area.y - 1;
    if visual >= area.height.saturating_sub(2) {
        return None;
    }
    Some(visual as usize + scroll_offset as usize)
}

fn hit_edit_target(ctx: &LoginEventCtx, mouse: &MouseEvent, area: Rect) -> Option<EditHit> {
    let row = mouse_content_row(mouse, area, ctx.sv.read().offset().y)?;
    let edit_state = ctx.edit_state.read();
    let es = edit_state.as_ref()?;
    let auth = ctx.auth.read();
    render::edit_hit_at(
        es,
        &auth,
        *ctx.edit_focus.read(),
        *ctx.edit_cursor.read(),
        row,
    )
}

fn hit_type_change_target(ctx: &LoginEventCtx, mouse: &MouseEvent, area: Rect) -> Option<EditHit> {
    let row = mouse_content_row(mouse, area, ctx.sv.read().offset().y)?;
    let edit_state = ctx.edit_state.read();
    let es = edit_state.as_ref()?;
    render::type_change_hit_at(es, row)
}

/// 聚焦字段（文本字段光标落在末尾，与 Tab 导航一致）。
fn focus_edit_field(ctx: &LoginEventCtx, field: LoginField) {
    let cursor = if field.is_text() {
        ctx.edit_state
            .read()
            .as_ref()
            .map(|es| es.field_text(field).chars().count())
            .unwrap_or(0)
    } else {
        0
    };
    *ctx.edit_focus.write() = field;
    *ctx.edit_cursor.write() = cursor;
}

/// 点击语义：文本行只聚焦；choice/按钮复用键盘动作；认证动作先选中再执行。
fn activate_edit_hit(ctx: &LoginEventCtx, hit: EditHit) {
    match hit {
        EditHit::Field(field) if field.is_text() => focus_edit_field(ctx, field),
        EditHit::Field(field) => {
            focus_edit_field(ctx, field);
            // 档位选择器的键盘切换键是 Space；其余 choice/按钮用 Enter。
            let code = if field == LoginField::ProfileSelector {
                KeyCode::Char(' ')
            } else {
                KeyCode::Enter
            };
            apply_login_edit_key(ctx, &KeyEvent::new(code, KeyModifiers::NONE));
        }
        EditHit::AuthAction(action) => {
            let selected = {
                let edit_state = ctx.edit_state.read();
                match edit_state.as_ref() {
                    Some(es) => ctx.auth.write().select_action(action, &es.plan),
                    None => false,
                }
            };
            if selected {
                focus_edit_field(ctx, LoginField::AuthActions);
                apply_login_edit_key(ctx, &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            }
        }
        EditHit::TypeChangeCandidate | EditHit::TypeChangeConfirm => {
            activate_type_change_hit(ctx, hit);
        }
    }
    scroll_focus_into_view(ctx);
}

fn activate_type_change_hit(ctx: &LoginEventCtx, hit: EditHit) {
    let code = match hit {
        EditHit::TypeChangeCandidate => KeyCode::Right,
        EditHit::TypeChangeConfirm => KeyCode::Enter,
        EditHit::Field(_) | EditHit::AuthAction(_) => return,
    };
    apply_type_change_key(ctx, &KeyEvent::new(code, KeyModifiers::NONE));
}

/// 把当前焦点字段滚入视口（键盘焦点移动/尺寸变化后的唯一滚动入口）。
///
/// 行号与绘制同源（[`render::focus_row`] / [`render::line_count`]），只在事件边界
/// 写 `ScrollViewState`，不在渲染段改状态（TUI-RENDER-001）。
fn scroll_focus_into_view(ctx: &LoginEventCtx) {
    if *ctx.mode.read() != LoginPanelMode::Edit {
        return;
    }
    let Some(area) = ctx.area else {
        return;
    };
    let edit_state = ctx.edit_state.read();
    let Some(edit_state) = edit_state.as_ref() else {
        return;
    };
    let focus = *ctx.edit_focus.read();
    let height = area.height.saturating_sub(2).max(1) as usize;
    let auth = ctx.auth.read();
    let row = render::focus_row(edit_state, &auth, focus);
    let total = render::line_count(edit_state, &auth, focus);
    drop(auth);
    let max_offset = total.saturating_sub(height);
    let mut sv = ctx.sv.write();
    let offset = sv.offset().y as usize;
    let next = if row < offset {
        row
    } else if row >= offset + height {
        row + 1 - height
    } else {
        offset
    };
    sv.set_offset(Position::new(0, next.min(max_offset) as u16));
}

/// 事件处理上下文：本帧区域 + 面板句柄 + 列表快照。
///
/// `State<T>` 无公开构造 API，只能从组件挂载取得；集中成一个结构体，事件入口签名不再
/// 随状态增长（鼠标命中与动作目标必须共用同一个本帧列表快照与视口起点）。
struct LoginEventCtx {
    area: Option<Rect>,
    cursor: State<usize>,
    mode: State<LoginPanelMode>,
    edit_state: State<Option<LoginEditState>>,
    edit_focus: State<LoginField>,
    edit_cursor: State<usize>,
    /// 删除确认门冻结的目标身份（provider id）
    confirm_delete_target: State<Option<String>>,
    /// 认证视图、完成门与后台事件接收。
    auth: State<AuthController>,
    /// Edit 视图滚动状态：焦点移动时把字段滚入视口。
    sv: State<ScrollViewState>,
    /// 本帧 provider 快照：命中索引与动作目标同源
    providers: Vec<ProviderSummary>,
    scroll_start: usize,
}

/// 消费 Model 面板留下的一次性“管理 Provider”请求。
///
/// 只在目标仍是当前权威配置中的 provider 时进入编辑；否则保持 Browse，避免对
/// 已删除身份创建草稿。
fn apply_login_request(
    mode: State<LoginPanelMode>,
    edit_state: State<Option<LoginEditState>>,
    edit_focus: State<LoginField>,
    edit_cursor: State<usize>,
    request: Option<crate::kit::atoms::LoginPanelRequest>,
) {
    let Some(request) = request else {
        return;
    };
    // 一次性：无论能否进入编辑都清空，避免下次挂载重复执行旧导航。
    let _ = LOGIN_PANEL_REQUEST.state().write().take();
    let Some(provider_id) = request.provider_id else {
        return;
    };
    let entered = enter_login_edit_mode(
        &mut edit_state.write(),
        &mut edit_focus.write(),
        &mut edit_cursor.write(),
        &provider_id,
    );
    if entered {
        *mode.write() = LoginPanelMode::Edit;
    }
}

/// Edit 模式缺少编辑状态时的回退（effect 边界）：渲染只读状态（TUI-RENDER-001）。
fn fallback_to_browse(mode: State<LoginPanelMode>, edit_state_missing: bool) {
    if !edit_state_missing {
        return;
    }
    // 先取快照再写：`read()` 的读守卫跨过 `write()` 会在同一线程自锁
    let current_mode = *mode.read();
    if matches!(
        current_mode,
        LoginPanelMode::Edit
            | LoginPanelMode::ConfirmTypeChange
            | LoginPanelMode::AuthDetail
            | LoginPanelMode::ConfirmLogout
    ) {
        *mode.write() = LoginPanelMode::Browse;
    }
}

/// Browse 行命中：每项 3 或 4 行（base_url 存在时 4），线性累加反推
fn hit_browse_item(
    mouse: &MouseEvent,
    area: Rect,
    providers: &[ProviderSummary],
    scroll_start: usize,
) -> Option<usize> {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return None;
    }
    let row = mouse.row;
    // 顶部/底部边框行不命中
    if row <= area.y || row >= area.y + area.height.saturating_sub(1) {
        return None;
    }
    let visual = row - area.y - 1;
    let content_height = area.height.saturating_sub(2);
    if visual >= content_height {
        return None;
    }
    // header 2 行（标题 + 空行）
    if visual < 2 {
        return None;
    }
    let mut cur = 2u16;
    for (i, p) in providers
        .iter()
        .enumerate()
        .skip(scroll_start)
        .take(VISIBLE_ITEMS)
    {
        let item_h = if p.base_url.is_some() { 4u16 } else { 3u16 };
        if visual >= cur && visual < cur + item_h {
            return Some(i);
        }
        cur += item_h;
    }
    None
}

/// Browse 进入编辑：目标取自本帧列表快照（与命中同源），编辑路径随后按稳定 id 定位。
fn enter_edit_row(ctx: &LoginEventCtx) {
    let sel = *ctx.cursor.read();
    if let Some(provider_id) = ctx.providers.get(sel).map(|p| p.id.clone()) {
        ctx.auth.write().shutdown();
        enter_login_edit_mode(
            &mut ctx.edit_state.write(),
            &mut ctx.edit_focus.write(),
            &mut ctx.edit_cursor.write(),
            &provider_id,
        );
        *ctx.mode.write() = LoginPanelMode::Edit;
        sync_auth_scope(ctx);
        let plan_mode = ctx
            .edit_state
            .read()
            .as_ref()
            .is_some_and(LoginEditState::is_plan_mode);
        if plan_mode {
            self::auth::dispatch_status(ctx.auth, false);
        }
    }
}

/// Ctrl+D：固定待删除身份（provider id）后进入确认门。
///
/// 身份取自本帧列表快照（`ctx.providers`，与命中同源）：投影列表每 tick 重派生，按下标
/// 取实时列表会在「渲染到事件」之间漂移到另一项。确认期间列表刷新由 `confirm_delete`
/// 按冻结身份校验实时列表兜住。
fn begin_confirm_delete(ctx: &LoginEventCtx) {
    let sel = *ctx.cursor.read();
    let target = ctx.providers.get(sel).map(|p| p.id.clone());
    if let Some(target) = target {
        *ctx.confirm_delete_target.write() = Some(target);
        *ctx.mode.write() = LoginPanelMode::ConfirmDelete;
    }
}

/// 确认删除：目标按确认时冻结的身份解析，与编辑路径同一身份口径（provider id）。
///
/// 目标已不在当前列表（列表刷新 / 他处删除）时不发送请求、不误删其他项，并给出可见提示。
fn confirm_delete(ctx: &LoginEventCtx) {
    let target = ctx.confirm_delete_target.read().clone();
    // 一次性门：无论删除是否发出，确认态都在本分支内清除（同一次确认不会连发）
    *ctx.mode.write() = LoginPanelMode::Browse;
    *ctx.confirm_delete_target.write() = None;

    let Some(target) = target else {
        return;
    };
    if provider_list_contains(&target) {
        request_delete_provider(&target);
    } else {
        notify_delete_target_missing(&target);
    }
}

/// 目标身份是否仍在当前投影列表里。
fn provider_list_contains(provider_id: &str) -> bool {
    PROVIDER_LIST
        .state()
        .read()
        .iter()
        .any(|p| p.id == provider_id)
}

/// 发出删除请求（生产路径：`config_store` 按身份删除）。
///
/// 测试构建下请求交给接缝记录并直接返回——单元测试不触碰真实配置与磁盘
/// （`PERI_CONFIG_HANDLE` 是进程级 `OnceLock`，测试进程无法安装）。
fn request_delete_provider(provider_id: &str) {
    #[cfg(test)]
    if tests::intercept_delete_request(provider_id) {
        return;
    }
    delete_provider(provider_id);
}

/// 提示「删除目标已消失」——走本面板既有提示机制（`NOTIFICATION`，同保存失败提示）。
///
/// 复用跨面板的确认目标守卫文案：与 Cron 的删除确认同一个语义（确认时固定的身份
/// 在列表刷新后已不存在，操作未执行），不借用插件删除失败的文案。
fn notify_delete_target_missing(provider_id: &str) {
    *NOTIFICATION.state().write() = Some(Notification {
        message: i18n::tr_args(
            "app-confirm-target-missing",
            &[("target".to_string(), FluentValue::from(provider_id))],
        ),
        until: peri_time::monotonic_now() + Duration::from_secs(2),
    });
}

// ── Browse 模式辅助函数 ───────────────────────────────────────────────────────

fn close_panel() {
    // I19-A: 弹栈而非清空整个栈
    crate::kit::panel_registry::close_active_panel();
}

#[cfg(test)]
#[path = "login_test.rs"]
mod tests;
