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
use crate::kit::atoms::{
    ACP_CLIENT_HANDLE, NOTIFICATION, Notification, PERI_CONFIG_HANDLE, PROVIDER_LIST,
    ProviderSummary,
};
use crate::kit::list_nav::{next_selection, previous_selection, scroll_start_for_selected};
use crate::kit::panel_mouse::{AreaTracker, is_scrollbar_column};
use fluent_bundle::FluentValue;
use peri_acp::provider::config::{ProviderConfig, ProviderModels};
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::Constraint,
        style::{Modifier, Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};
use std::time::{Duration, Instant};

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

#[component]
pub fn LoginPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
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

    // 面板绘制区域（上一帧）——鼠标点击行号反推
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    // Browse 列表视口常量（与渲染共用）
    const VISIBLE_ITEMS: usize = 3;

    // Browse 编辑动作：Enter 与鼠标左键点击共用（click as enter）
    let providers_for_closure = providers.clone();
    let enter_edit_row = move || {
        let sel = *cursor.read();
        let provider_state = PROVIDER_LIST.state();
        let store_read = provider_state.read();
        if sel < store_read.len() {
            let provider_id = store_read[sel].id.clone();
            drop(store_read);
            enter_login_edit_mode(
                &mut edit_state.write(),
                &mut edit_focus.write(),
                &mut edit_cursor.write(),
                &provider_id,
            );
            *mode.write() = LoginPanelMode::Edit;
        }
    };

    // ConfirmDelete 确认动作：Enter 与鼠标左键点击共用
    let confirm_delete = move || {
        delete_provider(*cursor.read());
        *mode.write() = LoginPanelMode::Browse;
    };

    // Browse 行命中：每项 3 或 4 行（base_url 存在时 4），线性累加反推
    fn hit_browse_item(
        mouse: &ratatui_kit::crossterm::event::MouseEvent,
        area: ratatui_kit::ratatui::layout::Rect,
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
    let browse_scroll_start = scroll_start_for_selected(*cursor.read(), count, VISIBLE_ITEMS);

    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::Normal,
        EventOptions { hit_test: true },
        {
            move |event| {
                // 鼠标：区域内左键点击 = 执行对应模式的 Enter 动作（click as enter）
                if let Event::Mouse(mouse) = event {
                    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
                        return EventResult::Ignored;
                    }
                    let Some(area) = area else {
                        return EventResult::Ignored;
                    };
                    if *mode.read() == LoginPanelMode::ConfirmDelete {
                        confirm_delete();
                        return EventResult::Consumed;
                    }
                    if *mode.read() == LoginPanelMode::Browse
                        && !is_scrollbar_column(&mouse, area)
                        && let Some(idx) = hit_browse_item(
                            &mouse,
                            area,
                            &providers_for_closure,
                            browse_scroll_start,
                        )
                    {
                        *cursor.write() = idx;
                        enter_edit_row();
                        return EventResult::Consumed;
                    }
                    // 区域内点击（未命中行 / Edit 模式）也消费，防止穿透
                    return EventResult::Consumed;
                }
                // 粘贴事件：仅编辑模式下处理
                if let Event::Paste(paste_text) = &event {
                    if *mode.read() == LoginPanelMode::Edit
                        && *edit_focus.read() != LoginEditField::ProviderType
                    {
                        let mut es_guard = edit_state.write();
                        let mut ec_guard = edit_cursor.write();
                        if let Some(ref mut es) = *es_guard {
                            handle_login_paste(&mut ec_guard, es, *edit_focus.read(), paste_text);
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
                if *mode.read() == LoginPanelMode::ConfirmDelete {
                    match key.code {
                        KeyCode::Enter => confirm_delete(),
                        _ => {
                            // Esc 或任意其他键：取消删除
                            *mode.write() = LoginPanelMode::Browse;
                        }
                    }
                    return EventResult::Consumed;
                }

                let current_mode = *mode.read();

                match current_mode {
                    LoginPanelMode::Browse => match key.code {
                        KeyCode::Esc => close_panel(),
                        KeyCode::Up => {
                            let mut c = cursor.write();
                            *c = previous_selection(*c);
                        }
                        KeyCode::Down => {
                            let latest = PROVIDER_LIST.state().read().len();
                            let mut c = cursor.write();
                            if latest > 0 {
                                *c = next_selection(*c, latest);
                            }
                        }
                        // Ctrl+N：新建 provider（进入编辑模式，空表单）
                        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            *edit_state.write() = Some(LoginEditState::default_empty());
                            *edit_focus.write() = LoginEditField::ProviderType;
                            *edit_cursor.write() = 0;
                            *mode.write() = LoginPanelMode::Edit;
                        }
                        // Ctrl+D：删除当前选中的 provider（进入确认模式）
                        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            if count > 0 {
                                *mode.write() = LoginPanelMode::ConfirmDelete;
                            }
                        }
                        // Enter：进入编辑模式
                        KeyCode::Enter => enter_edit_row(),
                        _ => {}
                    },
                    LoginPanelMode::Edit => {
                        let mut m_guard = mode.write();
                        let mut es_guard = edit_state.write();
                        let mut ef_guard = edit_focus.write();
                        let mut ec_guard = edit_cursor.write();
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
        },
    );

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
                let scroll_start = scroll_start_for_selected(sel, count, VISIBLE_ITEMS);
                for (i, p) in providers
                    .iter()
                    .enumerate()
                    .skip(scroll_start)
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
            if let Some(ref es) = *es_opt {
                let ef = *edit_focus.read();
                let ec = *edit_cursor.read();

                let is_new = es.original_provider_id.is_empty();
                let title_key = if is_new {
                    "login-panel-title-new"
                } else {
                    "login-panel-title-edit"
                };
                let title_text = if is_new {
                    format!("  {}", i18n::tr(title_key))
                } else {
                    format!("  {}: {}", i18n::tr(title_key), es.original_provider_id)
                };
                lines.push(Line::from(vec![Span::styled(
                    title_text,
                    Style::new().fg(focus_color).bold(),
                )]));
                lines.push(Line::from(""));

                // ── 标准字段：Type（toggle）+ ID / BaseUrl / ApiKey（文本输入）
                // 布局与 setup_wizard 的 render_edit 一致：focus 时 ❯ + focus 色 label
                let type_focused = ef == LoginEditField::ProviderType;
                let type_prefix = if type_focused { "❯ " } else { "  " };
                let type_style = if type_focused {
                    Style::default()
                        .fg(focus_color)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(dim)
                };
                lines.push(Line::from(vec![
                    Span::styled(type_prefix, Style::default().fg(cursor_color)),
                    Span::styled(
                        format!("{}: ", i18n::tr(LoginEditField::ProviderType.i18n_key())),
                        type_style,
                    ),
                    Span::styled(
                        format!("[{}]", i18n::tr(provider_type_label(&es.provider_type))),
                        Style::default().fg(text_color).add_modifier(Modifier::BOLD),
                    ),
                ]));

                for field in &[
                    LoginEditField::ProviderId,
                    LoginEditField::BaseUrl,
                    LoginEditField::ApiKey,
                ] {
                    let is_focused = *field == ef;
                    let display_val = if *field == LoginEditField::ApiKey {
                        // API Key 脱敏显示（编辑时也只显示脱敏版本 + 实际输入体现在光标位置）
                        mask_api_key_display(es.field_value(*field))
                    } else {
                        es.field_value(*field).to_string()
                    };
                    lines.push(render_login_edit_line(
                        i18n::tr(field.i18n_key()),
                        display_val,
                        is_focused,
                        ec,
                        cursor_color,
                        dim,
                        text_color,
                        focus_color,
                    ));
                }

                // ── Model 分组标题（与 setup_wizard 一致）
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    i18n::tr("login-model-label"),
                    Style::default().fg(dim).add_modifier(Modifier::BOLD),
                )));

                // ── Fable / Opus / Sonnet / Haiku 模型名
                for field in &[
                    LoginEditField::FableModel,
                    LoginEditField::OpusModel,
                    LoginEditField::SonnetModel,
                    LoginEditField::HaikuModel,
                ] {
                    lines.push(render_login_edit_line(
                        i18n::tr(field.i18n_key()),
                        es.field_value(*field).to_string(),
                        *field == ef,
                        ec,
                        cursor_color,
                        dim,
                        text_color,
                        focus_color,
                    ));
                }

                // ── 确认按钮（参考 setup_wizard 的 Confirm 行：focus 时 ❯ + 强调色）
                let cf_focused = ef == LoginEditField::Confirm;
                let cf_prefix = if cf_focused { "❯ " } else { "  " };
                let cf_style = if cf_focused {
                    Style::default()
                        .fg(cursor_color)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(dim)
                };
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled(cf_prefix, Style::default().fg(cursor_color)),
                    Span::styled(format!("  {}", i18n::tr("login-confirm")), cf_style),
                ]));

                lines.push(Line::from(""));
                lines.push(make_hint_line_for_login(
                    vec![
                        (
                            "\u{2191}/\u{2193}".to_string(),
                            i18n::tr("hint-login-field"),
                        ),
                        (
                            "\u{2190}/\u{2192}/Space".to_string(),
                            i18n::tr("hint-login-toggle"),
                        ),
                        ("Enter".to_string(), i18n::tr("hint-login-confirm")),
                        ("Esc".to_string(), i18n::tr("hint-login-back")),
                    ],
                    dim,
                    focus_color,
                ));
            } else {
                // 状态异常：回退到 Browse
                lines.push(Line::from(vec![Span::styled(
                    "  Internal error: invalid edit state",
                    Style::new().fg(semantic.status.error),
                )]));
                *mode.write() = LoginPanelMode::Browse;
            }
        }
        LoginPanelMode::ConfirmDelete => {
            let sel = *cursor.read();
            let provider_name = providers
                .get(sel)
                .map(|p| p.id.as_str())
                .unwrap_or("(unknown)");

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
    crate::kit::panel_scroll::register_panel_scroll(
        PanelKind::Login,
        hooks.use_previous_size(),
        sv,
    );

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

// ── Browse 模式辅助函数 ───────────────────────────────────────────────────────

fn close_panel() {
    // I19-A: 弹栈而非清空整个栈
    crate::kit::panel_registry::close_active_panel();
}

// ── Edit 模式辅助函数 ─────────────────────────────────────────────────────────

/// 从 PERI_CONFIG_HANDLE 读取完整 provider 配置并初始化编辑状态。
fn enter_login_edit_mode(
    edit_state: &mut Option<LoginEditState>,
    edit_focus: &mut LoginEditField,
    edit_cursor: &mut usize,
    provider_id: &str,
) {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    let cfg = handle.read();
    if let Some(config) = cfg.config.providers.iter().find(|p| p.id == provider_id) {
        *edit_state = Some(LoginEditState::from_provider_config(config));
        *edit_focus = LoginEditField::ProviderType;
        *edit_cursor = 0;
    }
    drop(cfg);
}

/// 编辑模式下的按键处理。
///
/// 文本编辑（字符、退格、删除、光标移动、Ctrl+W）、字段导航（↑/↓）、
/// 确认按钮 Enter 保存、Esc 放弃、Ctrl+S 快捷保存。
fn handle_login_edit_keys(
    mode: &mut LoginPanelMode,
    edit_state: &mut Option<LoginEditState>,
    edit_focus: &mut LoginEditField,
    edit_cursor: &mut usize,
    key: &ratatui_kit::crossterm::event::KeyEvent,
) {
    let Some(es) = edit_state else {
        *mode = LoginPanelMode::Browse;
        return;
    };

    let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // 先处理文本编辑按键（所有字段共用）
    let text_handled = handle_login_text_input(es, *edit_focus, edit_cursor, key);
    if text_handled {
        return;
    }

    // ProviderType toggle（Left/Right/Space 切换，参考 setup_wizard 模式）
    if *edit_focus == LoginEditField::ProviderType {
        match key.code {
            KeyCode::Left | KeyCode::Right if !is_ctrl => {
                es.provider_type = match es.provider_type.as_str() {
                    "anthropic" => "openai".to_string(),
                    _ => "anthropic".to_string(),
                };
                return;
            }
            KeyCode::Char(' ') if !is_ctrl => {
                es.provider_type = match es.provider_type.as_str() {
                    "anthropic" => "openai".to_string(),
                    _ => "anthropic".to_string(),
                };
                return;
            }
            _ => {}
        }
    }

    // 导航 / 确认 / 放弃
    match key.code {
        KeyCode::Up if !is_ctrl => {
            *edit_focus = edit_focus.prev();
            *edit_cursor = if *edit_focus == LoginEditField::ProviderType {
                0
            } else {
                es.field_value(*edit_focus).chars().count()
            };
        }
        KeyCode::Down if !is_ctrl => {
            *edit_focus = edit_focus.next();
            *edit_cursor = if *edit_focus == LoginEditField::ProviderType {
                0
            } else {
                es.field_value(*edit_focus).chars().count()
            };
        }
        KeyCode::Enter => {
            // Enter：聚焦在确认按钮时保存（校验通过才回到 Browse，参考 setup_wizard）
            if *edit_focus == LoginEditField::Confirm && save_login_edit(es) {
                *mode = LoginPanelMode::Browse;
                *edit_state = None;
            }
        }
        KeyCode::Esc => {
            // 放弃编辑，回到 Browse
            *mode = LoginPanelMode::Browse;
            *edit_state = None;
        }
        KeyCode::Char('s') if is_ctrl && save_login_edit(es) => {
            // Ctrl+S 保存（保留快捷键，校验通过才回到 Browse）
            *mode = LoginPanelMode::Browse;
            *edit_state = None;
        }
        _ => {}
    }
}

/// 编辑字段的文本输入处理（与 setup_wizard 的 handle_text_input 模式一致）
fn handle_login_text_input(
    state: &mut LoginEditState,
    field: LoginEditField,
    edit_cursor: &mut usize,
    key: &ratatui_kit::crossterm::event::KeyEvent,
) -> bool {
    // ProviderType 是 toggle、Confirm 是按钮，均不接受文本输入
    if field == LoginEditField::ProviderType || field == LoginEditField::Confirm {
        return false;
    }

    use KeyCode::*;
    let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    let val = state.field_value_mut(field);
    let chars: Vec<char> = val.chars().collect();

    match key.code {
        Char(ch) if !is_ctrl => {
            let pos = (*edit_cursor).min(chars.len());
            let prefix: String = chars[..pos].iter().collect();
            let suffix: String = chars[pos..].iter().collect();
            *val = format!("{}{}{}", prefix, ch, suffix);
            *edit_cursor = pos + 1;
            true
        }
        Backspace if !is_ctrl => {
            if *edit_cursor > 0 && *edit_cursor <= chars.len() {
                let prefix: String = chars[..*edit_cursor - 1].iter().collect();
                let suffix: String = chars[*edit_cursor..].iter().collect();
                *val = format!("{}{}", prefix, suffix);
                *edit_cursor -= 1;
            } else if *edit_cursor > chars.len() && !chars.is_empty() {
                let prefix: String = chars[..chars.len() - 1].iter().collect();
                *val = prefix;
                *edit_cursor = chars.len() - 1;
            }
            true
        }
        Delete => {
            if *edit_cursor < chars.len() {
                let prefix: String = chars[..*edit_cursor].iter().collect();
                let suffix: String = chars[*edit_cursor + 1..].iter().collect();
                *val = format!("{}{}", prefix, suffix);
            }
            true
        }
        Left if !is_ctrl => {
            if *edit_cursor > 0 {
                *edit_cursor -= 1;
            }
            true
        }
        Right if !is_ctrl => {
            let max_pos = chars.len();
            if *edit_cursor < max_pos {
                *edit_cursor += 1;
            }
            true
        }
        Home if !is_ctrl => {
            *edit_cursor = 0;
            true
        }
        End if !is_ctrl => {
            *edit_cursor = chars.len();
            true
        }
        Char('w') if is_ctrl => {
            // Ctrl+W: 删除前一个词
            let pos = (*edit_cursor).min(chars.len());
            if pos == 0 {
                return true;
            }
            let mut end = pos;
            while end > 0 && chars[end - 1].is_whitespace() {
                end -= 1;
            }
            while end > 0 && !chars[end - 1].is_whitespace() {
                end -= 1;
            }
            let prefix: String = chars[..end].iter().collect();
            let suffix: String = chars[pos..].iter().collect();
            *edit_cursor = end;
            *val = format!("{}{}", prefix, suffix);
            true
        }
        _ => false,
    }
}

/// 粘贴到当前编辑字段的光标位置。
fn handle_login_paste(
    edit_cursor: &mut usize,
    state: &mut LoginEditState,
    field: LoginEditField,
    paste_text: &str,
) {
    const MAX_PASTE_CHARS: usize = 10_000;
    let normalized = paste_text.replace("\r\n", "\n").replace('\r', "\n");
    let truncated: String = normalized.chars().take(MAX_PASTE_CHARS).collect();
    if normalized.chars().count() != truncated.chars().count() {
        tracing::warn!(
            "login panel: paste truncated from {} to {MAX_PASTE_CHARS} chars (CJK-safe)",
            normalized.chars().count()
        );
    }

    let val = state.field_value_mut(field);
    let chars: Vec<char> = val.chars().collect();
    let pos = (*edit_cursor).min(chars.len());
    let prefix: String = chars[..pos].iter().collect();
    let suffix: String = chars[pos..].iter().collect();
    let paste_len = truncated.chars().count();
    *val = format!("{}{}{}", prefix, truncated, suffix);
    *edit_cursor = pos + paste_len;
}

/// 保存编辑结果：先持久化到磁盘，成功后才发布到 PERI_CONFIG_HANDLE /
/// PROVIDER_LIST / ACP。返回 `true` 表示保存成功；`false` 表示校验失败、
/// 配置句柄缺失或持久化失败（不退出编辑，防止"假保存成功"）。
fn save_login_edit(es: &LoginEditState) -> bool {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return false;
    };

    let is_new = es.original_provider_id.is_empty();

    // 构建 detached 配置快照（在副本上修改，落盘成功前不动全局 handle）
    let snap = {
        let mut cfg = handle.write().clone();

        if is_new {
            // New 路径：校验 provider_id 非空后 push 新 ProviderConfig，自动激活
            if es.provider_id.trim().is_empty() {
                *NOTIFICATION.state().write() = Some(Notification {
                    message: i18n::tr("app-provider-name-empty"),
                    until: Instant::now() + Duration::from_secs(2),
                });
                return false;
            }
            let new_config = ProviderConfig {
                provider_type: es.provider_type.clone(),
                id: es.provider_id.clone(),
                api_key: es.api_key.clone(),
                base_url: es.base_url.clone(),
                models: ProviderModels {
                    fable: es.fable_model.clone(),
                    opus: es.opus_model.clone(),
                    sonnet: es.sonnet_model.clone(),
                    haiku: es.haiku_model.clone(),
                },
                ..Default::default()
            };
            cfg.config.providers.push(new_config);
            // 激活：写入 active profile 的 provider
            let alias = cfg.config.active_alias.clone();
            if let Some(profile) = cfg.config.profiles.get_mut(&alias) {
                profile.provider = es.provider_id.clone();
            }
        } else {
            // Edit 路径：查找并更新已有 provider
            if let Some(provider) = cfg
                .config
                .providers
                .iter_mut()
                .find(|p| p.id == es.original_provider_id)
            {
                provider.provider_type = es.provider_type.clone();
                provider.id = es.provider_id.clone();
                provider.api_key = es.api_key.clone();
                provider.base_url = es.base_url.clone();
                provider.models.fable = es.fable_model.clone();
                provider.models.opus = es.opus_model.clone();
                provider.models.sonnet = es.sonnet_model.clone();
                provider.models.haiku = es.haiku_model.clone();

                // 如果 id 变化且该 provider 是当前激活的，同步更新 active profile 的 provider
                let active_profile_provider = cfg
                    .config
                    .profiles
                    .get(&cfg.config.active_alias)
                    .map(|p| p.provider.clone())
                    .unwrap_or_default();
                if active_profile_provider == es.original_provider_id
                    && es.provider_id != es.original_provider_id
                {
                    let alias = cfg.config.active_alias.clone();
                    if let Some(profile) = cfg.config.profiles.get_mut(&alias) {
                        profile.provider = es.provider_id.clone();
                    }
                }
            }
        }

        cfg
    };

    // 先持久化：失败则不发布任何变更（handle / PROVIDER_LIST / ACP 均不动）
    if let Err(e) = crate::config::save(&snap) {
        *NOTIFICATION.state().write() = Some(Notification {
            message: i18n::tr_args(
                "config-save-failed",
                &[(
                    "error".to_string(),
                    FluentValue::from(e.to_string().as_str()),
                )],
            ),
            until: Instant::now() + Duration::from_secs(2),
        });
        return false;
    }

    // 持久化成功后：发布到全局 handle + 刷新 PROVIDER_LIST + 推送 ACP
    *handle.write() = snap.clone();
    refresh_provider_list();

    if let Some(client) = ACP_CLIENT_HANDLE.get() {
        tokio::spawn(async move {
            if let Err(e) = client.update_config(&snap).await {
                tracing::warn!(error = %e, "LoginPanel: update_config push failed");
            }
        });
    }

    *NOTIFICATION.state().write() = Some(Notification {
        message: i18n::tr("config-saved").to_string(),
        until: Instant::now() + Duration::from_secs(1),
    });

    if is_new {
        tracing::info!(
            provider_id = %es.provider_id,
            "LoginPanel: new provider created"
        );
    } else {
        tracing::info!(
            provider_id = %es.original_provider_id,
            "LoginPanel: provider edit saved"
        );
    }
    true
}

/// 从 PERI_CONFIG_HANDLE 刷新 PROVIDER_LIST atom（避免多处重复 25 行）
fn refresh_provider_list() {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    let cfg = handle.read();
    let active_profile_provider = cfg
        .config
        .profiles
        .get(&cfg.config.active_alias)
        .map(|p| p.provider.clone())
        .unwrap_or_default();
    let updated_providers: Vec<ProviderSummary> = cfg
        .config
        .providers
        .iter()
        .map(|p| {
            let env_key = format!("{}_API_KEY", p.provider_type.to_uppercase());
            let has_api_key = !p.api_key.is_empty() || std::env::var(env_key).is_ok();
            let base_url = if p.base_url.is_empty() {
                None
            } else {
                Some(p.base_url.clone())
            };
            ProviderSummary {
                id: p.id.clone(),
                provider_type: p.provider_type.clone(),
                is_active: p.id == active_profile_provider,
                has_api_key,
                base_url,
            }
        })
        .collect();
    *PROVIDER_LIST.state().write() = updated_providers;
}

/// 持久化 PeriConfig 快照并显示通知
fn persist_and_notify(snap: &crate::config::PeriConfig) {
    match crate::config::save(snap) {
        Ok(()) => {
            *NOTIFICATION.state().write() = Some(Notification {
                message: i18n::tr("config-saved").to_string(),
                until: Instant::now() + Duration::from_secs(1),
            });
        }
        Err(e) => {
            *NOTIFICATION.state().write() = Some(Notification {
                message: i18n::tr_args(
                    "config-save-failed",
                    &[(
                        "error".to_string(),
                        FluentValue::from(e.to_string().as_str()),
                    )],
                ),
                until: Instant::now() + Duration::from_secs(2),
            });
        }
    }
}

/// 删除当前选中的 provider：从 PERI_CONFIG_HANDLE 移除 + 刷新 + 持久化 + 推送 ACP
fn delete_provider(selected_index: usize) {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };

    let provider_id = {
        let provider_state = PROVIDER_LIST.state();
        let store_read = provider_state.read();
        match store_read.get(selected_index) {
            Some(p) => p.id.clone(),
            None => return,
        }
    };

    let removed = {
        let mut cfg = handle.write();
        let len_before = cfg.config.providers.len();
        cfg.config.providers.retain(|p| p.id != provider_id);
        let removed = cfg.config.providers.len() < len_before;
        let snap = cfg.clone();
        drop(cfg);

        if removed {
            refresh_provider_list();
            persist_and_notify(&snap);

            // 推送 ACP
            if let Some(client) = ACP_CLIENT_HANDLE.get() {
                tokio::spawn(async move {
                    if let Err(e) = client.update_config(&snap).await {
                        tracing::warn!(error = %e, "LoginPanel: update_config push failed after delete");
                    }
                });
            }
        }

        removed
    };

    if removed {
        tracing::info!(provider_id = %provider_id, "LoginPanel: provider deleted");
    }
}

// ── 渲染辅助函数 ──────────────────────────────────────────────────────────────

/// 构建 Login 面板的底部提示行（风格与 setup_wizard 的 make_hint_line 一致）。
fn make_hint_line_for_login(
    items: Vec<(String, String)>,
    dim: ratatui::style::Color,
    accent: ratatui::style::Color,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, (key, desc)) in items.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", Style::default()));
        }
        spans.push(Span::styled(
            key,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {}", desc), Style::default().fg(dim)));
    }
    Line::from(spans)
}

/// 渲染编辑模式下的单行字段（简化自 setup_wizard 的 render_editable_line）。
#[allow(clippy::too_many_arguments)]
fn render_login_edit_line(
    label: String,
    display_value: String,
    is_focused: bool,
    cursor_pos: usize,
    cursor_color: ratatui::style::Color,
    dim: ratatui::style::Color,
    text_color: ratatui::style::Color,
    focus_color: ratatui::style::Color,
) -> Line<'static> {
    let (prefix, label_style) = if is_focused {
        (
            "❯ ",
            Style::default()
                .fg(focus_color)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        ("  ", Style::default().fg(dim))
    };

    let mut spans: Vec<Span<'static>> = vec![
        Span::styled(prefix, Style::default().fg(cursor_color)),
        Span::styled(format!("{}: ", label), label_style),
    ];

    if !is_focused {
        spans.push(Span::styled(display_value, Style::default().fg(text_color)));
        return Line::from(spans);
    }

    // 聚焦字段：渲染文本 + 光标
    let chars: Vec<char> = display_value.chars().collect();
    let clamped_cursor = cursor_pos.min(chars.len());
    for (i, ch) in chars.iter().enumerate() {
        if i == clamped_cursor && clamped_cursor < chars.len() {
            spans.push(Span::styled(
                ch.to_string(),
                Style::default()
                    .fg(text_color)
                    .bg(cursor_color)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                ch.to_string(),
                Style::default().fg(text_color),
            ));
        }
    }
    if clamped_cursor >= chars.len() {
        spans.push(Span::styled(" ", Style::default().bg(cursor_color)));
    }
    Line::from(spans)
}

/// 将 ProviderConfig::provider_type（"anthropic"|"openai"）映射为 i18n 标签 key。
fn provider_type_label(provider_type: &str) -> &'static str {
    match provider_type {
        "anthropic" => "setup-provider-anthropic",
        _ => "setup-provider-openai",
    }
}

/// API Key 脱敏显示：保留最后 4 个字符，其余用 * 代替。
fn mask_api_key_display(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    if raw.len() <= 4 {
        return "*".repeat(raw.len());
    }
    let tail: String = raw
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{}...{}", "*".repeat(4), tail)
}

#[cfg(test)]
#[path = "login_test.rs"]
mod tests;
