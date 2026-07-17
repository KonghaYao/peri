//! ratatui-kit LoginPanel component.
//!
//! H1f（Iteration 14）：从 PROVIDER_LIST atom 读取真实 provider 配置
//! （由 service_snapshot 从 peri_config.providers 派生）。Enter 通过
//! PERI_CONFIG_HANDLE 切换 active_provider_id 并持久化。
//!
//! Browse 模式：只读列表 + Enter 激活 + E/Ctrl+E 进入编辑。
//! Edit 模式：原地编辑 Provider 字段，Esc 放弃、Ctrl+S 保存并持久化。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{
    ACP_CLIENT_HANDLE, NOTIFICATION, Notification, PERI_CONFIG_HANDLE, PROVIDER_LIST,
    ProviderSummary, SERVICE_SNAPSHOT,
};
use crate::kit::list_nav::{next_selection, previous_selection, scroll_start_for_selected};
use fluent_bundle::FluentValue;
use peri_acp::provider::config::ProviderConfig;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
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
    /// 浏览模式：上下导航 + Enter 激活 + E 编辑
    Browse,
    /// 编辑模式：文本编辑 + 字段导航 + Ctrl+S 保存 + Esc 放弃
    Edit,
}

/// 编辑模式下可编辑的字段
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginEditField {
    ProviderType,
    ProviderId,
    ApiKey,
    BaseUrl,
    OpusModel,
    SonnetModel,
    HaikuModel,
}

impl LoginEditField {
    fn next(self) -> Self {
        match self {
            Self::ProviderType => Self::ProviderId,
            Self::ProviderId => Self::ApiKey,
            Self::ApiKey => Self::BaseUrl,
            Self::BaseUrl => Self::OpusModel,
            Self::OpusModel => Self::SonnetModel,
            Self::SonnetModel => Self::HaikuModel,
            Self::HaikuModel => Self::ProviderType,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::ProviderType => Self::HaikuModel,
            Self::ProviderId => Self::ProviderType,
            Self::ApiKey => Self::ProviderId,
            Self::BaseUrl => Self::ApiKey,
            Self::OpusModel => Self::BaseUrl,
            Self::SonnetModel => Self::OpusModel,
            Self::HaikuModel => Self::SonnetModel,
        }
    }

    fn i18n_key(self) -> &'static str {
        match self {
            Self::ProviderType => "login-field-type",
            Self::ProviderId => "login-field-name",
            Self::ApiKey => "login-field-api-key",
            Self::BaseUrl => "login-field-base-url",
            Self::OpusModel => "login-field-opus-model",
            Self::SonnetModel => "login-field-sonnet-model",
            Self::HaikuModel => "login-field-haiku-model",
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
            opus_model: config.models.opus.clone(),
            sonnet_model: config.models.sonnet.clone(),
            haiku_model: config.models.haiku.clone(),
        }
    }

    fn field_value(&self, field: LoginEditField) -> &str {
        match field {
            LoginEditField::ProviderType => &self.provider_type,
            LoginEditField::ProviderId => &self.provider_id,
            LoginEditField::ApiKey => &self.api_key,
            LoginEditField::BaseUrl => &self.base_url,
            LoginEditField::OpusModel => &self.opus_model,
            LoginEditField::SonnetModel => &self.sonnet_model,
            LoginEditField::HaikuModel => &self.haiku_model,
        }
    }

    fn field_value_mut(&mut self, field: LoginEditField) -> &mut String {
        match field {
            LoginEditField::ProviderType => &mut self.provider_type,
            LoginEditField::ProviderId => &mut self.provider_id,
            LoginEditField::ApiKey => &mut self.api_key,
            LoginEditField::BaseUrl => &mut self.base_url,
            LoginEditField::OpusModel => &mut self.opus_model,
            LoginEditField::SonnetModel => &mut self.sonnet_model,
            LoginEditField::HaikuModel => &mut self.haiku_model,
        }
    }
}

// ── 主组件 ────────────────────────────────────────────────────────────────────

#[component]
pub fn LoginPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let cursor = hooks.use_state(|| 0usize);
    let mode = hooks.use_state(|| LoginPanelMode::Browse);
    let edit_state = hooks.use_state(|| None::<LoginEditState>);
    let edit_focus = hooks.use_state(|| LoginEditField::ProviderType);
    let edit_cursor = hooks.use_state(|| 0usize);
    let store = hooks.use_atom(&PROVIDER_LIST);
    let providers: Vec<ProviderSummary> = store.read().clone();
    let _ = store;
    let count = providers.len();

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, {
        move |event| {
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
                    // E 或 Ctrl+E：进入编辑模式
                    KeyCode::Char('e') | KeyCode::Char('E') => {
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
                    }
                    KeyCode::Enter => {
                        let sel = *cursor.read();
                        let latest_providers = PROVIDER_LIST.state().read().clone();
                        if let Some(p) = latest_providers.get(sel) {
                            let provider_id = p.id.clone();
                            let provider_type = p.provider_type.clone();
                            // 同步写 PERI_CONFIG_HANDLE + 更新 PROVIDER_LIST.is_active
                            if let Some(handle) = PERI_CONFIG_HANDLE.get() {
                                let mut cfg = handle.write();
                                cfg.config.active_provider_id = provider_id.clone();
                                // 即时推送 SERVICE_SNAPSHOT——同时更新 provider_name 和
                                // model_name（不同 provider 的 alias→model 映射可能不同）
                                let snap = cfg.clone();
                                drop(cfg);
                                let resolved_name = {
                                    let active_prov =
                                        snap.config.providers.iter().find(|p| p.id == provider_id);
                                    active_prov
                                        .and_then(|p| p.models.get_model(&snap.config.active_alias))
                                        .map(|s| s.to_string())
                                        .filter(|s| !s.is_empty())
                                        .unwrap_or_else(|| snap.config.active_alias.clone())
                                };
                                let s_handle = SERVICE_SNAPSHOT.state();
                                let mut svc_snap = s_handle.read().clone();
                                svc_snap.provider_name = provider_type;
                                svc_snap.model_name = resolved_name;
                                *s_handle.write() = svc_snap;
                            }
                            // 更新 PROVIDER_LIST 的 is_active 标记
                            let updated_providers: Vec<ProviderSummary> = latest_providers
                                .iter()
                                .map(|pr| ProviderSummary {
                                    is_active: pr.id == provider_id,
                                    ..pr.clone()
                                })
                                .collect();
                            *PROVIDER_LIST.state().write() = updated_providers;
                            // 异步持久化 + 推送配置到 ACP 服务端（使切换立即生效）
                            tokio::spawn(async move {
                                activate_provider(&provider_id);
                                if let Some(client) = ACP_CLIENT_HANDLE.get()
                                    && let Some(handle) = PERI_CONFIG_HANDLE.get()
                                {
                                    let snap = handle.read().clone();
                                    if let Err(e) = client.update_config(&snap).await {
                                        tracing::warn!(error = %e, "LoginPanel: update_config push failed");
                                    }
                                }
                            });
                        }
                        // 关闭面板
                        close_panel();
                    }
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
            }
            EventResult::Consumed
        }
    });

    let sel = *cursor.read();
    let semantic = theme_def.read().semantic;
    let cursor_color = semantic.status.warning;
    let dim = semantic.text.dim;
    let text_color = semantic.text.primary;
    let focus_color = semantic.status.success;
    let _accent = semantic.status.warning;
    let muted = semantic.text.muted;

    let mut lines: Vec<Line<'_>> = Vec::new();

    match *mode.read() {
        LoginPanelMode::Browse => {
            // S16：TUI-PAGE.md §6.2 样式——Enter::select · Esc::close
            lines.push(Line::from(vec![Span::styled(
                format!("  {} providers configured", count),
                Style::new().fg(text_color).bold(),
            )]));
            lines.push(Line::from(""));

            if providers.is_empty() {
                lines.push(Line::from(vec![Span::styled(
                    "  No providers configured",
                    Style::new().fg(muted),
                )]));
                lines.push(Line::from(vec![Span::styled(
                    "  Run setup wizard or edit ~/.peri/settings.json",
                    Style::new().fg(muted),
                )]));
            } else {
                // 视口跟随
                const VISIBLE_ITEMS: usize = 3;
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

                    let active_marker = if p.is_active {
                        Span::styled(" \u{2714}", Style::new().fg(semantic.status.success).bold())
                    } else {
                        Span::styled("  ", Style::new())
                    };

                    lines.push(Line::from(vec![
                        Span::styled(
                            format!(" {} ", cursor_mark),
                            Style::new().fg(theme_def.read().component.panel.title),
                        ),
                        active_marker,
                        Span::styled(format!("{}  ({})", p.id, p.provider_type), row_style),
                    ]));

                    let key_marker = if p.has_api_key {
                        ("api key: configured", semantic.status.success)
                    } else {
                        ("api key: missing", semantic.status.error)
                    };
                    lines.push(Line::from(vec![Span::styled(
                        format!("     {}", key_marker.0),
                        Style::new().fg(key_marker.1),
                    )]));
                    if let Some(url) = &p.base_url {
                        let url_display: String = url.chars().take(70).collect();
                        lines.push(Line::from(vec![Span::styled(
                            format!("     base url: {}", url_display),
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
                    ("Enter".to_string(), i18n::tr("hint-login-activate")),
                    ("E".to_string(), i18n::tr("hint-login-edit")),
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

                lines.push(Line::from(vec![Span::styled(
                    format!(
                        "  {}: {}",
                        i18n::tr("login-panel-title-edit"),
                        es.original_provider_id
                    ),
                    Style::new().fg(focus_color).bold(),
                )]));
                lines.push(Line::from(""));

                // 7 个可编辑字段（ProviderType 为 toggle，其余为文本输入）
                for field in &[
                    LoginEditField::ProviderType,
                    LoginEditField::ProviderId,
                    LoginEditField::ApiKey,
                    LoginEditField::BaseUrl,
                    LoginEditField::OpusModel,
                    LoginEditField::SonnetModel,
                    LoginEditField::HaikuModel,
                ] {
                    let is_focused = *field == ef;
                    if *field == LoginEditField::ProviderType {
                        // ProviderType 渲染为 toggle（参考 setup_wizard 模式）
                        let type_prefix = if is_focused { "❯ " } else { "  " };
                        let type_style = if is_focused {
                            Style::default()
                                .fg(focus_color)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(dim)
                        };
                        lines.push(Line::from(vec![
                            Span::styled(type_prefix, Style::default().fg(cursor_color)),
                            Span::styled(format!("{}: ", i18n::tr(field.i18n_key())), type_style),
                            Span::styled(
                                format!(
                                    "[{}]",
                                    i18n::tr(provider_type_label(es.field_value(*field)))
                                ),
                                Style::default().fg(text_color).add_modifier(Modifier::BOLD),
                            ),
                        ]));
                    } else {
                        let display_val = if *field == LoginEditField::ApiKey {
                            // API Key 脱敏显示（编辑时也只显示脱敏版本 + 实际输入体现在光标位置）
                            let raw = es.field_value(*field);
                            if raw.is_empty() {
                                String::new()
                            } else {
                                // 脱敏：显示最后 4 个字符，其余用 * 代替
                                if raw.len() <= 4 {
                                    "*".repeat(raw.len())
                                } else {
                                    format!(
                                        "{}...{}",
                                        "*".repeat(4),
                                        raw.chars()
                                            .rev()
                                            .take(4)
                                            .collect::<String>()
                                            .chars()
                                            .rev()
                                            .collect::<String>()
                                    )
                                }
                            }
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
                }

                lines.push(Line::from(""));
                lines.push(make_hint_line_for_login(
                    vec![
                        (
                            "\u{2191}/\u{2193}".to_string(),
                            i18n::tr("hint-login-field"),
                        ),
                        (
                            "\u{2190}/\u{2192}".to_string(),
                            i18n::tr("hint-login-toggle"),
                        ),
                        ("Ctrl+S".to_string(), i18n::tr("hint-login-save")),
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
    }

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    panel_shell!(PanelKind::Login, {
            ScrollView(
                scrollbars: crate::kit::panel_registry::clean_scrollbars(),
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

/// H1f: 持久化当前 PERI_CONFIG_HANDLE 到 settings.json。
///
/// 不检查 active_provider_id 是否变更——调用方已在事件处理器中同步更新。
fn activate_provider(_provider_id: &str) {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    let cfg = handle.read();
    let snap = cfg.clone();
    drop(cfg);
    match crate::config::save(&snap) {
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
    tracing::info!(provider_id = _provider_id, "LoginPanel: config persisted");
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
/// Esc 放弃、Ctrl+S 保存。
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

    // 导航 / 保存 / 放弃
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
        KeyCode::Esc => {
            // 放弃编辑，回到 Browse
            *mode = LoginPanelMode::Browse;
            *edit_state = None;
        }
        KeyCode::Char('s') if is_ctrl => {
            // Ctrl+S 保存
            save_login_edit(es);
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
    // ProviderType 是 toggle，不接受文本输入
    if field == LoginEditField::ProviderType {
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

/// 保存编辑结果：写 PERI_CONFIG_HANDLE + 持久化 + 刷新 PROVIDER_LIST
fn save_login_edit(es: &LoginEditState) {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    {
        let mut cfg = handle.write();
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
            provider.models.opus = es.opus_model.clone();
            provider.models.sonnet = es.sonnet_model.clone();
            provider.models.haiku = es.haiku_model.clone();

            // 如果 id 变化且该 provider 是当前激活的，同步更新 active_provider_id
            if cfg.config.active_provider_id == es.original_provider_id
                && es.provider_id != es.original_provider_id
            {
                cfg.config.active_provider_id = es.provider_id.clone();
            }
        }
        let snap = cfg.clone();
        drop(cfg);

        // 刷新 PROVIDER_LIST
        {
            let cfg = handle.read();
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
                        is_active: p.id == cfg.config.active_provider_id,
                        has_api_key,
                        base_url,
                    }
                })
                .collect();
            *PROVIDER_LIST.state().write() = updated_providers;
        }

        // 持久化
        match crate::config::save(&snap) {
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
    tracing::info!(
        provider_id = %es.original_provider_id,
        "LoginPanel: provider edit saved"
    );
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

#[cfg(test)]
mod tests {}
