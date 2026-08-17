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
use crate::kit::atoms::{PROVIDER_LIST, ProviderSummary};
use crate::kit::list_nav::{next_selection, previous_selection, scroll_start_for_selected};
use crate::kit::panel_mouse::{AreaTracker, is_scrollbar_column};
use peri_acp::provider::config::ProviderConfig;
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

mod config_store;
mod edit_handler;
mod render;

use self::config_store::delete_provider;
use self::edit_handler::{enter_login_edit_mode, handle_login_edit_keys, handle_login_paste};
use self::render::{
    make_hint_line_for_login, mask_api_key_display, provider_type_label, render_login_edit_line,
};

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

#[cfg(test)]
#[path = "login_test.rs"]
mod tests;
