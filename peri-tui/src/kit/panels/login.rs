//! ratatui-kit LoginPanel component.
//!
//! H1f（Iteration 14）：从 PROVIDER_LIST atom 读取真实 provider 配置
//! （由 service_snapshot 从 peri_config.providers 派生）。Enter 通过
//! PERI_CONFIG_HANDLE 切换 active_provider_id 并持久化。
//!
//! Browse 模式：只读列表 + Enter/鼠标点击进入编辑。
//! Edit 模式：原地编辑 Provider 字段（样式与 setup_wizard 表单统一），
//! 底部确认按钮 Enter 保存并持久化，Esc 放弃，Ctrl+S 快捷保存。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{NOTIFICATION, Notification, PROVIDER_LIST, ProviderSummary};
use crate::kit::list_nav::{next_selection, previous_selection, scroll_start_for_selected};
use crate::kit::panel_mouse::{AreaTracker, is_scrollbar_column};
use fluent_bundle::FluentValue;
use peri_acp::provider::config::ProviderConfig;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{
        Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    prelude::*,
    ratatui::{
        layout::{Constraint, Rect},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};
use std::time::Duration;

mod config_store;
mod edit_handler;
mod render;

use self::config_store::delete_provider;
use self::edit_handler::{enter_login_edit_mode, handle_login_edit_keys, handle_login_paste};
use self::render::make_hint_line_for_login;

// ── Login 编辑模式类型 ─────────────────────────────────────────────────────────

/// Login 面板操作模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginPanelMode {
    /// 浏览模式：上下导航 + Enter/鼠标点击编辑 + Ctrl+N/Ctrl+D 新建/删除
    Browse,
    /// 编辑模式：文本编辑 + 字段导航 + Ctrl+S 保存 + Esc 放弃
    Edit,
    /// 删除确认模式：Enter 确认删除 + Esc 取消
    ConfirmDelete,
}

/// 编辑模式下可编辑的字段（布局与 setup_wizard 的 Form Edit 一致：
/// Type/ID/BaseUrl/ApiKey + Model 分组 + Confirm 确认按钮）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginEditField {
    ProviderType,
    ProviderId,
    BaseUrl,
    ApiKey,
    FableModel,
    OpusModel,
    SonnetModel,
    HaikuModel,
    Confirm,
}

impl LoginEditField {
    fn next(self) -> Self {
        match self {
            Self::ProviderType => Self::ProviderId,
            Self::ProviderId => Self::BaseUrl,
            Self::BaseUrl => Self::ApiKey,
            Self::ApiKey => Self::FableModel,
            Self::FableModel => Self::OpusModel,
            Self::OpusModel => Self::SonnetModel,
            Self::SonnetModel => Self::HaikuModel,
            Self::HaikuModel => Self::Confirm,
            Self::Confirm => Self::ProviderType,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::ProviderType => Self::Confirm,
            Self::ProviderId => Self::ProviderType,
            Self::BaseUrl => Self::ProviderId,
            Self::ApiKey => Self::BaseUrl,
            Self::FableModel => Self::ApiKey,
            Self::OpusModel => Self::FableModel,
            Self::SonnetModel => Self::OpusModel,
            Self::HaikuModel => Self::SonnetModel,
            Self::Confirm => Self::HaikuModel,
        }
    }

    fn i18n_key(self) -> &'static str {
        match self {
            Self::ProviderType => "login-field-type",
            Self::ProviderId => "login-field-name",
            Self::ApiKey => "login-field-api-key",
            Self::BaseUrl => "login-field-base-url",
            Self::FableModel => "login-field-fable-model",
            Self::OpusModel => "login-field-opus-model",
            Self::SonnetModel => "login-field-sonnet-model",
            Self::HaikuModel => "login-field-haiku-model",
            Self::Confirm => "login-confirm",
        }
    }
}

/// 编辑模式下的字段值工作副本
#[derive(Debug, Clone)]
struct LoginEditState {
    /// 进入编辑时的原始 provider_id（用于 save 时定位配置项）
    original_provider_id: String,
    provider_type: String,
    provider_id: String,
    api_key: String,
    base_url: String,
    fable_model: String,
    opus_model: String,
    sonnet_model: String,
    haiku_model: String,
}

impl LoginEditState {
    fn from_provider_config(config: &ProviderConfig) -> Self {
        Self {
            original_provider_id: config.id.clone(),
            provider_type: config.provider_type.clone(),
            provider_id: config.id.clone(),
            api_key: config.api_key.clone(),
            base_url: config.base_url.clone(),
            fable_model: config.models.fable.clone(),
            opus_model: config.models.opus.clone(),
            sonnet_model: config.models.sonnet.clone(),
            haiku_model: config.models.haiku.clone(),
        }
    }

    fn default_empty() -> Self {
        Self {
            original_provider_id: String::new(),
            provider_type: "anthropic".to_string(),
            provider_id: String::new(),
            api_key: String::new(),
            base_url: String::new(),
            fable_model: String::new(),
            opus_model: String::new(),
            sonnet_model: String::new(),
            haiku_model: String::new(),
        }
    }

    fn field_value(&self, field: LoginEditField) -> &str {
        match field {
            LoginEditField::ProviderType => &self.provider_type,
            LoginEditField::ProviderId => &self.provider_id,
            LoginEditField::ApiKey => &self.api_key,
            LoginEditField::BaseUrl => &self.base_url,
            LoginEditField::FableModel => &self.fable_model,
            LoginEditField::OpusModel => &self.opus_model,
            LoginEditField::SonnetModel => &self.sonnet_model,
            LoginEditField::HaikuModel => &self.haiku_model,
            LoginEditField::Confirm => "",
        }
    }

    fn field_value_mut(&mut self, field: LoginEditField) -> &mut String {
        match field {
            LoginEditField::ProviderType => &mut self.provider_type,
            LoginEditField::ProviderId => &mut self.provider_id,
            LoginEditField::ApiKey => &mut self.api_key,
            LoginEditField::BaseUrl => &mut self.base_url,
            LoginEditField::FableModel => &mut self.fable_model,
            LoginEditField::OpusModel => &mut self.opus_model,
            LoginEditField::SonnetModel => &mut self.sonnet_model,
            LoginEditField::HaikuModel => &mut self.haiku_model,
            LoginEditField::Confirm => unreachable!("Confirm is a button, not a text field"),
        }
    }
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
    let edit_focus = hooks.use_state(|| LoginEditField::ProviderType);
    let edit_cursor = hooks.use_state(|| 0usize);
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
    let edit_state_missing = *mode.read() == LoginPanelMode::Edit && edit_state.read().is_none();
    hooks.use_effect(
        move || fallback_to_browse(mode, edit_state_missing),
        (edit_state_missing,),
    );

    let event_ctx = LoginEventCtx {
        area,
        cursor,
        mode,
        edit_state,
        edit_focus,
        edit_cursor,
        confirm_delete_target,
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
                        Span::styled(format!("{}  ({})", p.id, p.provider_type), row_style),
                    ]));

                    let key_marker = if p.has_api_key {
                        ("api key: configured", semantic.status.success)
                    } else {
                        ("api key: missing", semantic.status.error)
                    };
                    lines.push(Line::from(vec![Span::styled(
                        format!("   {}", key_marker.0),
                        Style::new().fg(key_marker.1),
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
                Some(es) => lines.extend(render::build_edit_lines(
                    es,
                    *edit_focus.read(),
                    *edit_cursor.read(),
                    &render::LoginRenderColors {
                        cursor: cursor_color,
                        dim,
                        text: text_color,
                        focus: focus_color,
                    },
                )),
                // 状态异常：effect 边界已在渲染前回退到 Browse；此处仅为保持 match 穷尽
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
        if current_mode == LoginPanelMode::Browse
            && !is_scrollbar_column(&mouse, area)
            && let Some(idx) = hit_browse_item(&mouse, area, &ctx.providers, ctx.scroll_start)
        {
            *ctx.cursor.write() = idx;
            enter_edit_row(ctx);
            return EventResult::Consumed;
        }
        // 区域内点击（未命中行 / Edit 模式）也消费，防止穿透
        return EventResult::Consumed;
    }
    // 粘贴事件：仅编辑模式下处理
    if let Event::Paste(paste_text) = &event {
        if current_mode == LoginPanelMode::Edit
            && *ctx.edit_focus.read() != LoginEditField::ProviderType
        {
            let mut es_guard = ctx.edit_state.write();
            let mut ec_guard = ctx.edit_cursor.write();
            if let Some(ref mut es) = *es_guard {
                handle_login_paste(&mut ec_guard, es, *ctx.edit_focus.read(), paste_text);
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
                *ctx.edit_state.write() = Some(LoginEditState::default_empty());
                *ctx.edit_focus.write() = LoginEditField::ProviderType;
                *ctx.edit_cursor.write() = 0;
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
            let mut m_guard = ctx.mode.write();
            let mut es_guard = ctx.edit_state.write();
            let mut ef_guard = ctx.edit_focus.write();
            let mut ec_guard = ctx.edit_cursor.write();
            handle_login_edit_keys(
                &mut m_guard,
                &mut es_guard,
                &mut ef_guard,
                &mut ec_guard,
                &key,
            );
        }
        LoginPanelMode::ConfirmDelete => {
            // 在 match 之前已处理并 return；编译器需要匹配臂以保持穷尽性
        }
    }
    EventResult::Consumed
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
    edit_focus: State<LoginEditField>,
    edit_cursor: State<usize>,
    /// 删除确认门冻结的目标身份（provider id）
    confirm_delete_target: State<Option<String>>,
    /// 本帧 provider 快照：命中索引与动作目标同源
    providers: Vec<ProviderSummary>,
    scroll_start: usize,
}

/// Edit 模式缺少编辑状态时的回退（effect 边界）：渲染只读状态（TUI-RENDER-001）。
fn fallback_to_browse(mode: State<LoginPanelMode>, edit_state_missing: bool) {
    if !edit_state_missing {
        return;
    }
    // 先取快照再写：`read()` 的读守卫跨过 `write()` 会在同一线程自锁
    let current_mode = *mode.read();
    if current_mode == LoginPanelMode::Edit {
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

/// Browse 进入编辑：目标按当前列表下标解析 provider id（编辑路径随后按稳定 id 定位）。
fn enter_edit_row(ctx: &LoginEventCtx) {
    let sel = *ctx.cursor.read();
    let provider_state = PROVIDER_LIST.state();
    let store_read = provider_state.read();
    if sel < store_read.len() {
        let provider_id = store_read[sel].id.clone();
        drop(store_read);
        enter_login_edit_mode(
            &mut ctx.edit_state.write(),
            &mut ctx.edit_focus.write(),
            &mut ctx.edit_cursor.write(),
            &provider_id,
        );
        *ctx.mode.write() = LoginPanelMode::Edit;
    }
}

/// Ctrl+D：固定待删除身份（provider id）后进入确认门。
///
/// 身份取自当前投影列表而非进入确认时的下标：列表每 tick 重派生，下标在确认期间会漂移。
fn begin_confirm_delete(ctx: &LoginEventCtx) {
    let sel = *ctx.cursor.read();
    let target = PROVIDER_LIST.state().read().get(sel).map(|p| p.id.clone());
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
