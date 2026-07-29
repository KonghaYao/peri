//! ratatui-kit SetupWizard —— 完整交互式配置向导。
//!
//! 四步向导：Language → Choose → Form → Done。
//! 状态存储在 `SETUP_WIZARD` atom 中，显隐由 `WIZARD_ACTIVE` 控制。

#![allow(clippy::needless_update)]

use crate::app::setup_wizard::*;
use crate::i18n;
use crate::kit::atoms::{self, LANG_VERSION, SETUP_WIZARD};
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Color, Modifier, Style},
        text::{Line, Span},
        widgets::{Borders, Paragraph},
    },
};

// ── 辅助渲染函数 ──────────────────────────────────────────────────────────────

fn make_hint_line(items: Vec<(String, String)>, dim: Color, accent: Color) -> Line<'static> {
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

fn render_cursor_items(
    items: Vec<(String, String)>,
    cursor: usize,
    cursor_color: Color,
    text_color: Color,
    dim: Color,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (i, (label, desc)) in items.into_iter().enumerate() {
        let is_cursor = i == cursor;
        let c = if is_cursor { "❯" } else { " " };
        let label_style = if is_cursor {
            Style::default()
                .fg(cursor_color)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(text_color)
        };
        let detail_style = if is_cursor {
            Style::default().fg(cursor_color)
        } else {
            Style::default().fg(dim)
        };
        let l = label.clone();
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", c), Style::default().fg(cursor_color)),
            Span::styled(format!("{} ", l), label_style),
        ]));
        if !desc.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(desc, detail_style),
            ]));
        }
        lines.push(Line::from(""));
    }
    lines
}

fn get_raw_field_value(state: &SetupWizardState) -> String {
    let mp = match state.active_provider_ref() {
        Some(mp) => mp,
        None => return String::new(),
    };
    match state.form_focus {
        FormField::ProviderId => mp.provider_id.clone(),
        FormField::BaseUrl => mp.base_url.clone(),
        FormField::ApiKey => mp.api_key.clone(),
        FormField::OpusModel => mp.aliases[0].clone(),
        FormField::SonnetModel => mp.aliases[1].clone(),
        FormField::HaikuModel => mp.aliases[2].clone(),
        _ => String::new(),
    }
}

/// 渲染编辑模式下文本框的内容，带光标指示
#[allow(clippy::too_many_arguments)]
fn render_editable_line(
    label: String,
    display_value: String,
    is_focused: bool,
    cursor_pos: usize,
    cursor_color: Color,
    dim: Color,
    text_color: Color,
    focus_color: Color,
) -> Line<'static> {
    let (_, label_style, prefix) = if is_focused {
        (
            (),
            Style::default()
                .fg(focus_color)
                .add_modifier(Modifier::BOLD),
            "❯ ",
        )
    } else {
        ((), Style::default().fg(dim), "  ")
    };

    let mut spans: Vec<Span<'static>> = vec![
        Span::styled(prefix.to_string(), Style::default().fg(cursor_color)),
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
            // 光标在字符上
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

// ── 主组件 ────────────────────────────────────────────────────────────────────

#[component]
pub fn SetupWizard(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;
    let _lang_ver = hooks.use_atom(&LANG_VERSION);

    // 订阅 wizard 状态
    let wizard_handle = hooks.use_atom(&SETUP_WIZARD);
    let wizard_active = hooks.use_atom(&atoms::WIZARD_ACTIVE);
    let _ = *wizard_active.read();
    let state = wizard_handle.read().clone();

    let step = state.step;
    let cursor_color = semantic.status.warning;
    let accent = semantic.status.warning;
    let dim = semantic.text.dim;
    let text_color = semantic.text.primary;
    let focus_color = semantic.status.success;
    let error_color = semantic.status.error;

    // 渲染内容
    let (title, lines) = match step {
        SetupStep::Language => render_language_step(&state, dim, accent, cursor_color, text_color),
        SetupStep::Choose => {
            render_choose_step(&state, dim, accent, cursor_color, text_color, error_color)
        }
        SetupStep::Form => render_form_step(
            &state,
            dim,
            accent,
            cursor_color,
            text_color,
            focus_color,
            error_color,
        ),
        SetupStep::Done => render_done_step(&state, dim, accent, cursor_color, text_color),
    };

    // 事件处理器
    {
        let state = state.clone();
        hooks.use_event_handler(EventScope::Current, EventPriority::High, move |event| {
            handle_wizard_event(event, state.clone())
        });
    }

    let title_style = Style::default().fg(accent).add_modifier(Modifier::BOLD);

    element! {
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            View(width: Constraint::Fill(1), height: Constraint::Fill(1)) {
                Border(
                    flex_direction: Direction::Vertical,
                    border_style: Style::default().fg(accent),
                    borders: Borders::TOP | Borders::BOTTOM,
                    top_title: Line::from(Span::styled(i18n::tr(&title), title_style)).centered(),
                    width: Constraint::Fill(1),
                ) {
                    Text(text: Paragraph::new(lines))
                }
            }
        }
    }
}

// ── 各步骤渲染 ────────────────────────────────────────────────────────────────

fn render_language_step(
    state: &SetupWizardState,
    dim: Color,
    accent: Color,
    cursor_color: Color,
    text_color: Color,
) -> (String, Vec<Line<'static>>) {
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            i18n::tr("setup-language-prompt"),
            Style::default().fg(dim),
        )),
        Line::from(""),
    ];

    let items: Vec<(String, String)> = LANGUAGE_OPTIONS
        .iter()
        .map(|(_, name)| (name.to_string(), String::new()))
        .collect();
    lines.extend(render_cursor_items(
        items,
        state.language_cursor,
        cursor_color,
        text_color,
        dim,
    ));

    lines.push(Line::from(""));
    lines.push(make_hint_line(
        vec![
            ("Enter".to_string(), i18n::tr("setup-key-confirm")),
            ("↑/↓".to_string(), i18n::tr("setup-key-select")),
            ("Esc".to_string(), i18n::tr("setup-key-quit")),
        ],
        dim,
        accent,
    ));

    ("setup-language-title".to_string(), lines)
}

fn render_choose_step(
    state: &SetupWizardState,
    dim: Color,
    accent: Color,
    cursor_color: Color,
    text_color: Color,
    error_color: Color,
) -> (String, Vec<Line<'static>>) {
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            i18n::tr("setup-choose-provider"),
            Style::default().fg(dim),
        )),
        Line::from(""),
    ];

    let items: Vec<(String, String)> = SetupSource::ALL
        .iter()
        .map(|src| (i18n::tr(src.label()), i18n::tr(src.description())))
        .collect();
    lines.extend(render_cursor_items(
        items,
        state.choose_cursor,
        cursor_color,
        text_color,
        dim,
    ));

    // 迁移失败错误提示
    if let Some(ref err) = state.submit_error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  ⚠ {}", err),
            Style::default().fg(error_color),
        )));
    }

    lines.push(Line::from(""));
    lines.push(make_hint_line(
        vec![
            ("Enter".to_string(), i18n::tr("setup-key-confirm")),
            ("↑/↓".to_string(), i18n::tr("setup-key-select")),
            ("Esc".to_string(), i18n::tr("setup-key-quit")),
        ],
        dim,
        accent,
    ));

    ("setup-welcome-title".to_string(), lines)
}

fn render_form_step(
    state: &SetupWizardState,
    dim: Color,
    accent: Color,
    cursor_color: Color,
    text_color: Color,
    focus_color: Color,
    error_color: Color,
) -> (String, Vec<Line<'static>>) {
    match state.form_mode {
        FormMode::Browse => {
            let (title, lines) =
                render_browse(state, dim, accent, cursor_color, text_color, error_color);
            (title.to_string(), lines)
        }
        FormMode::Edit => render_edit(
            state,
            dim,
            accent,
            cursor_color,
            text_color,
            focus_color,
            error_color,
        ),
    }
}

fn render_browse(
    state: &SetupWizardState,
    dim: Color,
    accent: Color,
    cursor_color: Color,
    text_color: Color,
    error_color: Color,
) -> (String, Vec<Line<'static>>) {
    let mut lines = vec![Line::from("")];
    let submit_pos = state.providers.len();

    if state.providers.is_empty() {
        lines.push(Line::from(Span::styled(
            i18n::tr("setup-no-providers"),
            Style::default().fg(dim),
        )));
        lines.push(Line::from(""));
    }

    for (idx, mp) in state.providers.iter().enumerate() {
        let is_cursor = idx == state.browse_cursor;
        let cursor = if is_cursor { "❯" } else { " " };
        let check_char = if mp.selected { "✓" } else { " " };
        let check_color = if mp.selected { accent } else { dim };
        let name_style = if is_cursor {
            Style::default()
                .fg(cursor_color)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(text_color)
        };
        let detail_style = if is_cursor {
            Style::default().fg(cursor_color)
        } else {
            Style::default().fg(dim)
        };

        let key_summary = if mp.api_key.is_empty() {
            i18n::tr("setup-no-key")
        } else {
            mask_api_key(&mp.api_key)
        };

        lines.push(Line::from(vec![
            Span::styled(format!("{} ", cursor), Style::default().fg(cursor_color)),
            Span::styled(
                format!("[{}] ", check_char),
                Style::default().fg(check_color),
            ),
            Span::styled(
                format!("{} ", i18n::tr(mp.provider_type.label())),
                name_style,
            ),
            Span::styled(format!("({}) ", mp.provider_id), Style::default().fg(dim)),
            Span::styled(key_summary, detail_style),
        ]));

        if !mp.base_url.is_empty() {
            let url_text = if mp.base_url.len() > 60 {
                format!("{}...", &mp.base_url[..57])
            } else {
                mp.base_url.clone()
            };
            lines.push(Line::from(vec![
                Span::styled("     ", Style::default()),
                Span::styled(url_text, Style::default().fg(dim)),
            ]));
        }

        // 显示模型别名（与上方 Provider 信息间空一行）
        lines.push(Line::from(""));
        let alias_labels = [
            i18n::tr("setup-field-opus"),
            i18n::tr("setup-field-sonnet"),
            i18n::tr("setup-field-haiku"),
        ];
        for (ai, label) in alias_labels.iter().enumerate() {
            let model_text = if mp.aliases[ai].len() > 40 {
                format!("{}...", &mp.aliases[ai][..37])
            } else {
                mp.aliases[ai].clone()
            };
            lines.push(Line::from(vec![
                Span::styled("     ", Style::default()),
                Span::styled(format!("{} → ", label), Style::default().fg(dim)),
                Span::styled(model_text, Style::default().fg(accent)),
            ]));
        }

        lines.push(Line::from(""));
    }

    // Submit 错误提示
    if let Some(ref err) = state.submit_error {
        lines.push(Line::from(Span::styled(
            format!("  ⚠ {}", err),
            Style::default().fg(error_color),
        )));
        lines.push(Line::from(""));
    }

    // Submit 按钮
    let submit_active = state.browse_cursor == submit_pos;
    let submit_style = if submit_active {
        Style::default().fg(accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(dim)
    };
    let submit_cursor = if submit_active { "❯ " } else { "  " };
    lines.push(Line::from(vec![
        Span::styled(submit_cursor, Style::default().fg(cursor_color)),
        Span::styled(format!(" {}", i18n::tr("setup-submit")), submit_style),
    ]));

    lines.push(Line::from(""));
    lines.push(make_hint_line(
        vec![
            ("Enter".to_string(), i18n::tr("setup-key-edit-submit")),
            ("Space".to_string(), i18n::tr("setup-key-check")),
            ("↑/↓".to_string(), i18n::tr("setup-key-select")),
            ("Esc".to_string(), i18n::tr("setup-key-back")),
        ],
        dim,
        accent,
    ));

    ("setup-configure-title".to_string(), lines)
}

fn render_edit(
    state: &SetupWizardState,
    dim: Color,
    accent: Color,
    cursor_color: Color,
    text_color: Color,
    focus_color: Color,
    error_color: Color,
) -> (String, Vec<Line<'static>>) {
    let mp = match state.active_provider_ref() {
        Some(mp) => mp,
        None => {
            return (
                "setup-configure-title".to_string(),
                vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "Internal error: invalid provider index",
                        Style::default().fg(focus_color),
                    )),
                ],
            );
        }
    };

    let header = format!(
        "{} ({})",
        i18n::tr(mp.provider_type.label()),
        mp.provider_id
    );
    let mut lines = vec![Line::from("")];
    let standard_labels = [
        (FormField::ProviderType, i18n::tr("setup-field-type")),
        (FormField::ProviderId, i18n::tr("setup-field-id")),
        (FormField::BaseUrl, i18n::tr("setup-field-base-url")),
        (
            FormField::TestConnectivity,
            i18n::tr("setup-field-test-connectivity"),
        ),
        (FormField::ApiKey, i18n::tr("setup-field-api-key")),
    ];
    let max_label_width = standard_labels
        .iter()
        .map(|(_, l)| l.chars().count())
        .max()
        .unwrap_or(4);
    let pad_width = max_label_width + 2; // 2 for "❯ " prefix

    // ProviderType
    let type_focused = state.form_focus == FormField::ProviderType;
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
            format!(
                "{}{}: ",
                pad_label(&i18n::tr("setup-field-type"), pad_width),
                ""
            ),
            type_style,
        ),
        Span::styled(
            format!("[{}]", i18n::tr(mp.provider_type.label())),
            Style::default().fg(text_color).add_modifier(Modifier::BOLD),
        ),
    ]));

    // ProviderId
    lines.push(render_editable_line(
        i18n::tr("setup-field-id"),
        mp.provider_id.clone(),
        state.form_focus == FormField::ProviderId,
        state.edit_cursor_pos,
        cursor_color,
        dim,
        text_color,
        focus_color,
    ));

    // BaseUrl
    lines.push(render_editable_line(
        i18n::tr("setup-field-base-url"),
        mp.base_url.clone(),
        state.form_focus == FormField::BaseUrl,
        state.edit_cursor_pos,
        cursor_color,
        dim,
        text_color,
        focus_color,
    ));

    // TestConnectivity
    let tc_focused = state.form_focus == FormField::TestConnectivity;
    let tc_prefix = if tc_focused { "❯ " } else { "  " };
    let tc_style = if tc_focused {
        Style::default()
            .fg(focus_color)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(dim)
    };
    let tc_label = i18n::tr("setup-field-test-connectivity");
    let (tc_status, tc_color) = match &state.connectivity_result {
        Some((true, msg)) => (msg.clone(), accent),
        Some((false, msg)) => (msg.clone(), error_color),
        None => (i18n::tr("setup-key-check"), dim),
    };
    lines.push(Line::from(vec![
        Span::styled(tc_prefix, Style::default().fg(cursor_color)),
        Span::styled(
            format!("{}{}: ", pad_label(&tc_label, pad_width), ""),
            tc_style,
        ),
        Span::styled(tc_status, Style::default().fg(tc_color)),
    ]));

    // ApiKey（脱敏显示）
    let api_display = if mp.api_key.is_empty() {
        String::new()
    } else {
        mask_api_key(&mp.api_key)
    };
    lines.push(render_editable_line(
        i18n::tr("setup-field-api-key"),
        api_display,
        state.form_focus == FormField::ApiKey,
        state.edit_cursor_pos,
        cursor_color,
        dim,
        text_color,
        focus_color,
    ));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        i18n::tr("setup-model-label"),
        Style::default().fg(dim).add_modifier(Modifier::BOLD),
    )));

    // Opus / Sonnet / Haiku 模型名
    for (i, field) in [
        FormField::OpusModel,
        FormField::SonnetModel,
        FormField::HaikuModel,
    ]
    .iter()
    .enumerate()
    {
        lines.push(render_editable_line(
            i18n::tr(field.i18n_key()),
            mp.aliases[i].clone(),
            state.form_focus == *field,
            state.edit_cursor_pos,
            cursor_color,
            dim,
            text_color,
            focus_color,
        ));
    }

    // Confirm
    let cf_focused = state.form_focus == FormField::Confirm;
    let cf_prefix = if cf_focused { "❯ " } else { "  " };
    let cf_style = if cf_focused {
        Style::default().fg(accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(dim)
    };
    lines.push(Line::from(vec![
        Span::styled(cf_prefix, Style::default().fg(cursor_color)),
        Span::styled(format!("  {}", i18n::tr("setup-confirm")), cf_style),
    ]));

    lines.push(Line::from(""));
    lines.push(make_hint_line(
        vec![
            ("↑/↓".to_string(), i18n::tr("setup-key-select")),
            ("←/→/Space".to_string(), i18n::tr("setup-key-switch-type")),
            ("Esc".to_string(), i18n::tr("setup-key-back-list")),
        ],
        dim,
        accent,
    ));

    (header, lines)
}

fn pad_label(label: &str, width: usize) -> String {
    let len = label.chars().count();
    if len >= width - 2 {
        label.to_string()
    } else {
        label.to_string()
    }
}

fn render_done_step(
    state: &SetupWizardState,
    dim: Color,
    accent: Color,
    _cursor_color: Color,
    text_color: Color,
) -> (String, Vec<Line<'static>>) {
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            i18n::tr("setup-complete-title"),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    for mp in &state.providers {
        if !mp.selected {
            continue;
        }
        let provider_id = mp.provider_id.clone();
        let api_key_display = mask_api_key(&mp.api_key);
        let aliases = mp.aliases.clone();
        let type_label = i18n::tr(mp.provider_type.label());
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(" ● ", Style::default().fg(accent)),
            Span::styled(format!("{} ", type_label), Style::default().fg(text_color)),
            Span::styled(format!("({})", provider_id), Style::default().fg(dim)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                format!("   {} ", i18n::tr("setup-label-key")),
                Style::default().fg(dim),
            ),
            Span::styled(api_key_display, Style::default().fg(text_color)),
        ]));
        lines.push(Line::from(""));
        let alias_labels = [
            i18n::tr("setup-field-opus"),
            i18n::tr("setup-field-sonnet"),
            i18n::tr("setup-field-haiku"),
        ];
        for (i, label) in alias_labels.iter().enumerate() {
            let model = aliases[i].clone();
            lines.push(Line::from(vec![
                Span::styled(format!("   {:>6} → ", label), Style::default().fg(dim)),
                Span::styled(model, Style::default().fg(accent)),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {} ", i18n::tr("setup-press-enter")),
            Style::default().fg(text_color),
        ),
        Span::styled(
            "Enter",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(i18n::tr("setup-to-start"), Style::default().fg(text_color)),
    ]));

    ("setup-complete-title".to_string(), lines)
}

// ── 事件处理 ──────────────────────────────────────────────────────────────────

fn handle_wizard_event(event: Event, mut state: SetupWizardState) -> EventResult {
    // 处理粘贴事件（仅 Form 编辑模式下且当前字段为文本输入时）
    if let Event::Paste(paste_text) = &event {
        if state.step == SetupStep::Form
            && state.form_mode == FormMode::Edit
            && state.form_focus.is_text_input()
        {
            handle_paste_to_text_input(&mut state, paste_text);
            *SETUP_WIZARD.state().write() = state;
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

    match state.step {
        SetupStep::Language => handle_language_keys(&mut state, key),
        SetupStep::Choose => handle_choose_keys(&mut state, key),
        SetupStep::Form => handle_form_keys(&mut state, key),
        SetupStep::Done => handle_done_keys(&mut state, key),
    }

    // 写回 state atom
    *SETUP_WIZARD.state().write() = state;
    EventResult::Consumed
}

fn handle_language_keys(
    state: &mut SetupWizardState,
    key: ratatui_kit::crossterm::event::KeyEvent,
) {
    use KeyCode::*;
    match key.code {
        Up => {
            state.language_cursor =
                (state.language_cursor + LANGUAGE_OPTIONS.len() - 1) % LANGUAGE_OPTIONS.len();
        }
        Down => {
            state.language_cursor = (state.language_cursor + 1) % LANGUAGE_OPTIONS.len();
        }
        Enter | Char(' ') => {
            let lang = LANGUAGE_OPTIONS[state.language_cursor].0.to_string();
            state.language = lang.clone();
            state.step = SetupStep::Choose;
            state.choose_cursor = 0;
            // 切换 i18n 语言
            i18n::switch(&lang);
        }
        Esc => {
            *atoms::WIZARD_ACTIVE.state().write() = false;
        }
        _ => {}
    }
}

fn handle_choose_keys(state: &mut SetupWizardState, key: ratatui_kit::crossterm::event::KeyEvent) {
    use KeyCode::*;
    match key.code {
        Up => {
            state.submit_error = None;
            state.choose_cursor =
                (state.choose_cursor + SetupSource::ALL.len() - 1) % SetupSource::ALL.len();
            state.source = SetupSource::ALL[state.choose_cursor];
        }
        Down => {
            state.submit_error = None;
            state.choose_cursor = (state.choose_cursor + 1) % SetupSource::ALL.len();
            state.source = SetupSource::ALL[state.choose_cursor];
        }
        Enter | Char(' ') => {
            state.submit_error = None;
            if state.source == SetupSource::MigrateClaudeCode {
                if !migrate_from_claude_code(state, None) {
                    state.source = SetupSource::CustomApi;
                    state.choose_cursor = 0;
                    state.submit_error = Some(
                        "迁移失败：未在 ~/.claude/settings.json 中找到有效的 Provider 配置。请确保文件中有 env.ANTHROPIC_API_KEY 或 env.OPENAI_API_KEY。"
                            .into(),
                    );
                    return;
                }
            } else {
                state.providers = vec![MigratedProvider::new(ProviderType::Anthropic)];
                state.active_provider = 0;
            }
            state.step = SetupStep::Form;
            state.form_mode = FormMode::Browse;
            state.browse_cursor = 0;
            state.form_focus = FormField::ProviderType;
        }
        Esc => {
            state.submit_error = None;
            state.step = SetupStep::Language;
        }
        _ => {}
    }
}

fn handle_form_keys(state: &mut SetupWizardState, key: ratatui_kit::crossterm::event::KeyEvent) {
    match state.form_mode {
        FormMode::Browse => handle_browse_keys(state, key),
        FormMode::Edit => handle_edit_keys(state, key),
    }
}

fn handle_done_keys(state: &mut SetupWizardState, key: ratatui_kit::crossterm::event::KeyEvent) {
    use KeyCode::*;
    match key.code {
        Enter => {
            // 保存配置并关闭 wizard
            if let Err(e) = save_setup(state) {
                tracing::error!("setup wizard: save failed: {e}");
            }
            *atoms::WIZARD_ACTIVE.state().write() = false;
        }
        Esc => {
            state.submit_error = None;
            state.step = SetupStep::Form;
            state.form_mode = FormMode::Browse;
        }
        _ => {}
    }
}

fn handle_browse_keys(state: &mut SetupWizardState, key: ratatui_kit::crossterm::event::KeyEvent) {
    use KeyCode::*;
    let max_pos = state.providers.len(); // submit button position
    match key.code {
        Up => {
            state.submit_error = None;
            if state.browse_cursor > 0 {
                state.browse_cursor -= 1;
            }
        }
        Down => {
            state.submit_error = None;
            if state.browse_cursor < max_pos {
                state.browse_cursor += 1;
            }
        }
        Char(' ') => {
            state.submit_error = None;
            if state.browse_cursor < state.providers.len() {
                let mp = &mut state.providers[state.browse_cursor];
                mp.selected = !mp.selected;
            }
        }
        Enter => {
            if state.browse_cursor < state.providers.len() {
                state.submit_error = None;
                state.active_provider = state.browse_cursor;
                state.form_mode = FormMode::Edit;
                state.form_focus = FormField::ProviderType;
                state.edit_cursor_pos = 0;
            } else {
                let has_valid = state
                    .providers
                    .iter()
                    .any(|p| p.selected && p.is_complete());
                if has_valid {
                    state.submit_error = None;
                    state.step = SetupStep::Done;
                } else {
                    state.submit_error = Some(
                        "No provider selected or incomplete. Select at least one provider with all fields filled."
                            .into(),
                    );
                }
            }
        }
        Esc => {
            state.submit_error = None;
            state.step = SetupStep::Choose;
        }
        _ => {}
    }
}

fn handle_edit_keys(state: &mut SetupWizardState, key: ratatui_kit::crossterm::event::KeyEvent) {
    use KeyCode::*;
    let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // 文本编辑按键：先处理
    if state.form_focus.is_text_input() {
        let handled = handle_text_input(state, &key);
        if handled {
            return;
        }
    }

    match key.code {
        Up => {
            state.form_focus = state.form_focus.prev();
            state.edit_cursor_pos = get_raw_field_value(state).chars().count();
        }
        Down => {
            state.form_focus = state.form_focus.next();
            state.edit_cursor_pos = get_raw_field_value(state).chars().count();
        }
        Left | Right if !is_ctrl && state.form_focus == FormField::ProviderType => {
            if let Some(mp) = state.active_provider_mut() {
                mp.provider_type.cycle();
            }
        }
        Char(' ') if state.form_focus == FormField::ProviderType => {
            if let Some(mp) = state.active_provider_mut() {
                mp.provider_type.cycle();
            }
        }
        Enter => {
            if state.form_focus == FormField::TestConnectivity {
                if let Some(mp) = state.active_provider_ref() {
                    state.connectivity_result = Some(test_connectivity(&mp.base_url));
                }
            } else if state.form_focus == FormField::Confirm {
                let mp = match state.active_provider_ref() {
                    Some(mp) => mp,
                    None => return,
                };
                if !mp.provider_id.trim().is_empty()
                    && !mp.api_key.trim().is_empty()
                    && mp.aliases.iter().all(|a| !a.trim().is_empty())
                {
                    state.form_mode = FormMode::Browse;
                }
            }
        }
        Esc => {
            state.form_mode = FormMode::Browse;
        }
        _ => {}
    }
}

fn handle_text_input(
    state: &mut SetupWizardState,
    key: &ratatui_kit::crossterm::event::KeyEvent,
) -> bool {
    use KeyCode::*;
    let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match key.code {
        Char(ch) if !is_ctrl => {
            let mut val = get_raw_field_value(state);
            let chars: Vec<char> = val.chars().collect();
            let pos = state.edit_cursor_pos.min(chars.len());
            let prefix: String = chars[..pos].iter().collect();
            let suffix: String = chars[pos..].iter().collect();
            val = format!("{}{}{}", prefix, ch, suffix);
            state.edit_cursor_pos = pos + 1;
            state.set_active_field_value(val);
            true
        }
        Backspace if !is_ctrl => {
            let mut val = get_raw_field_value(state);
            let chars: Vec<char> = val.chars().collect();
            if state.edit_cursor_pos > 0 && state.edit_cursor_pos <= chars.len() {
                let prefix: String = chars[..state.edit_cursor_pos - 1].iter().collect();
                let suffix: String = chars[state.edit_cursor_pos..].iter().collect();
                val = format!("{}{}", prefix, suffix);
                state.edit_cursor_pos -= 1;
            } else if state.edit_cursor_pos > chars.len() && !chars.is_empty() {
                let prefix: String = chars[..chars.len() - 1].iter().collect();
                val = prefix;
                state.edit_cursor_pos = chars.len() - 1;
            }
            state.set_active_field_value(val);
            true
        }
        Delete => {
            let val = get_raw_field_value(state);
            let chars: Vec<char> = val.chars().collect();
            if state.edit_cursor_pos < chars.len() {
                let prefix: String = chars[..state.edit_cursor_pos].iter().collect();
                let suffix: String = chars[state.edit_cursor_pos + 1..].iter().collect();
                state.set_active_field_value(format!("{}{}", prefix, suffix));
            }
            true
        }
        Left if !is_ctrl => {
            if state.edit_cursor_pos > 0 {
                state.edit_cursor_pos -= 1;
            }
            true
        }
        Right if !is_ctrl => {
            let val = get_raw_field_value(state);
            let max_pos = val.chars().count();
            if state.edit_cursor_pos < max_pos {
                state.edit_cursor_pos += 1;
            }
            true
        }
        Home if !is_ctrl => {
            state.edit_cursor_pos = 0;
            true
        }
        End if !is_ctrl => {
            let val = get_raw_field_value(state);
            state.edit_cursor_pos = val.chars().count();
            true
        }
        Char('w') if is_ctrl => {
            // Ctrl+W: 删除前一个词
            let val = get_raw_field_value(state);
            let chars: Vec<char> = val.chars().collect();
            let pos = state.edit_cursor_pos.min(chars.len());
            if pos == 0 {
                return true;
            }
            // 跳过前导空白
            let mut end = pos;
            while end > 0 && chars[end - 1].is_whitespace() {
                end -= 1;
            }
            // 跳过单词字符
            while end > 0 && !chars[end - 1].is_whitespace() {
                end -= 1;
            }
            let prefix: String = chars[..end].iter().collect();
            let suffix: String = chars[pos..].iter().collect();
            state.edit_cursor_pos = end;
            state.set_active_field_value(format!("{}{}", prefix, suffix));
            true
        }
        _ => false,
    }
}

/// 将剪贴板内容插入当前文本输入字段的光标位置。
/// 归一化换行符（\r\n → \n），截断至 10k 字符（CJK 安全），超出时记录警告。
fn handle_paste_to_text_input(state: &mut SetupWizardState, paste_text: &str) {
    const MAX_PASTE_CHARS: usize = 10_000;
    let normalized = paste_text.replace("\r\n", "\n").replace('\r', "\n");
    let truncated: String = normalized.chars().take(MAX_PASTE_CHARS).collect();
    if normalized.chars().count() != truncated.chars().count() {
        tracing::warn!(
            "setup wizard: paste truncated from {} to {MAX_PASTE_CHARS} chars (CJK-safe)",
            normalized.chars().count()
        );
    }

    let mut val = get_raw_field_value(state);
    let chars: Vec<char> = val.chars().collect();
    let pos = state.edit_cursor_pos.min(chars.len());
    let prefix: String = chars[..pos].iter().collect();
    let suffix: String = chars[pos..].iter().collect();
    let paste_len = truncated.chars().count();
    val = format!("{}{}{}", prefix, truncated, suffix);
    state.edit_cursor_pos = pos + paste_len;
    state.set_active_field_value(val);
}
