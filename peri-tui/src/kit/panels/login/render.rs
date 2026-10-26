use crate::i18n;
use fluent_bundle::FluentValue;

use super::auth::{AuthAction, AuthController};
use super::auth_render;
use super::edit_state::{LoginEditState, LoginField, ProviderKind};
use ratatui_kit::ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// Edit 视图的鼠标语义目标。与行构造同一过程产生，绘制与命中不各自推导布局。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EditHit {
    Field(LoginField),
    AuthAction(AuthAction),
    TypeChangeCandidate,
    TypeChangeConfirm,
}

/// Edit / 类型确认视图的行与逐行命中区（`hits` 与 `lines` 等长）。
pub(super) struct EditLayout {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) hits: Vec<Option<EditHit>>,
}

impl EditLayout {
    pub(super) fn new() -> Self {
        Self {
            lines: Vec::new(),
            hits: Vec::new(),
        }
    }

    pub(super) fn push(&mut self, line: Line<'static>) {
        self.lines.push(line);
        self.hits.push(None);
    }

    pub(super) fn push_hit(&mut self, line: Line<'static>, hit: EditHit) {
        self.lines.push(line);
        self.hits.push(Some(hit));
    }

    pub(super) fn push_field(&mut self, field: LoginField, line: Line<'static>) {
        self.push_hit(line, EditHit::Field(field));
    }

    fn extend(&mut self, other: EditLayout) {
        self.lines.extend(other.lines);
        self.hits.extend(other.hits);
    }

    fn hit_at(&self, row: usize) -> Option<EditHit> {
        self.hits.get(row).copied().flatten()
    }
}

// ── 渲染辅助函数 ──────────────────────────────────────────────────────────────

/// Edit 视图配色（由调用方从当前主题派生；渲染辅助函数不读全局状态）。
pub(super) struct LoginRenderColors {
    pub(super) cursor: Color,
    pub(super) dim: Color,
    pub(super) text: Color,
    pub(super) focus: Color,
}

impl LoginRenderColors {
    pub(super) fn focused_label(&self) -> Style {
        Style::default().fg(self.focus).add_modifier(Modifier::BOLD)
    }

    pub(super) fn label(&self) -> Style {
        Style::default().fg(self.dim)
    }
}

/// 构建 Edit 视图的行与逐行命中区（同一过程，绘制与鼠标命中不各自推导布局）。
pub(super) fn build_edit_layout(
    es: &LoginEditState,
    auth: &AuthController,
    focus: LoginField,
    cursor_pos: usize,
    colors: &LoginRenderColors,
) -> EditLayout {
    let mut out = EditLayout::new();

    let is_new = es.is_new();
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
    out.push(Line::from(vec![Span::styled(
        title_text,
        Style::default().fg(colors.focus).bold(),
    )]));
    out.push(Line::from(""));

    // ── 类型（新建：方向键循环；已有：Enter 显式更改）──
    out.push_field(LoginField::ProviderType, type_line(es, focus, colors));
    if !is_new {
        out.push_field(
            LoginField::ProviderType,
            Line::from(vec![Span::styled(
                format!("    {}", i18n::tr("login-type-locked-hint")),
                colors.label(),
            )]),
        );
    }

    // ── Provider ID ──
    out.push_field(
        LoginField::ProviderId,
        render_login_edit_line(
            i18n::tr(LoginField::ProviderId.i18n_key()),
            es.provider_id.clone(),
            focus == LoginField::ProviderId,
            cursor_pos,
            colors.cursor,
            colors.dim,
            colors.text,
            colors.focus,
        ),
    );

    // ── 认证方式 / plan 配置 ──
    if es.is_responses() {
        let mut auth_layout = auth_render::plan_auth_layout(es, auth, focus, cursor_pos, colors);
        // API key 模式字段（plan 模式由认证区接管，避免残留引用）。
        if !es.is_plan_mode() {
            for field in [LoginField::BaseUrl, LoginField::ApiKey] {
                let value = if field == LoginField::ApiKey {
                    mask_api_key_display(&es.api_key)
                } else {
                    es.base_url.clone()
                };
                auth_layout.push_field(
                    field,
                    render_login_edit_line(
                        i18n::tr(field.i18n_key()),
                        value,
                        field == focus,
                        cursor_pos,
                        colors.cursor,
                        colors.dim,
                        colors.text,
                        colors.focus,
                    ),
                );
            }
        }
        out.extend(auth_layout);
    } else {
        // 非 Responses provider 没有认证方式选择，保留只读说明。
        out.push(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                format!("{}: ", i18n::tr("login-field-auth-mode")),
                colors.label(),
            ),
            Span::styled(
                i18n::tr("login-auth-api-key"),
                Style::default().fg(colors.text).bold(),
            ),
        ]));
        for field in [LoginField::BaseUrl, LoginField::ApiKey] {
            let value = if field == LoginField::ApiKey {
                mask_api_key_display(&es.api_key)
            } else {
                es.base_url.clone()
            };
            out.push_field(
                field,
                render_login_edit_line(
                    i18n::tr(field.i18n_key()),
                    value,
                    field == focus,
                    cursor_pos,
                    colors.cursor,
                    colors.dim,
                    colors.text,
                    colors.focus,
                ),
            );
        }
    }

    // ── 默认模型映射 ──
    out.push(Line::from(""));
    out.push(Line::from(Span::styled(
        i18n::tr("login-model-label"),
        Style::default().fg(colors.dim).add_modifier(Modifier::BOLD),
    )));
    for field in [
        LoginField::FableModel,
        LoginField::OpusModel,
        LoginField::SonnetModel,
        LoginField::HaikuModel,
    ] {
        out.push_field(
            field,
            render_login_edit_line(
                i18n::tr(field.i18n_key()),
                es.field_text(field),
                field == focus,
                cursor_pos,
                colors.cursor,
                colors.dim,
                colors.text,
                colors.focus,
            ),
        );
    }

    // ── Responses 档位子区 ──
    if es.is_responses() {
        out.push(Line::from(""));
        out.extend(profile_section(es, focus, cursor_pos, colors));
    }

    // ── 保存摘要 ──
    out.push(Line::from(""));
    out.push(Line::from(vec![Span::styled(
        format!(
            "  {}",
            i18n::tr_args(
                "login-save-summary",
                &[
                    (
                        "id".to_string(),
                        FluentValue::from(if es.provider_id.trim().is_empty() {
                            "…"
                        } else {
                            es.provider_id.trim()
                        }),
                    ),
                    (
                        "profiles".to_string(),
                        FluentValue::from(es.profiles_to_update().as_str()),
                    ),
                ],
            )
        ),
        colors.label(),
    )]));

    if let Some(error) = &es.inline_error {
        out.push(Line::from(vec![Span::styled(
            format!("  ⚠ {}", error.message),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )]));
        // 只有 revision/authority 冲突才提示重新载入；字段校验错误重载不会改变结果。
        if error.kind == crate::kit::panels::login::edit_state::LoginErrorKind::Conflict {
            out.push(Line::from(vec![Span::styled(
                format!("    {}", i18n::tr("login-conflict-reload-hint")),
                colors.label(),
            )]));
        }
    }

    // ── 确认按钮 ──
    out.push(Line::from(""));
    out.push_field(
        LoginField::Confirm,
        action_line(
            i18n::tr("login-confirm"),
            focus == LoginField::Confirm,
            colors,
        ),
    );

    out.push(Line::from(""));
    out.push(make_hint_line_for_login(
        vec![
            (
                "\u{2191}/\u{2193}/Tab".to_string(),
                i18n::tr("hint-login-field"),
            ),
            (
                "\u{2190}/\u{2192}/Space".to_string(),
                i18n::tr("hint-login-toggle"),
            ),
            ("Enter".to_string(), i18n::tr("hint-login-confirm")),
            ("Ctrl+S".to_string(), i18n::tr("login-confirm")),
            ("Esc".to_string(), i18n::tr("hint-login-back")),
        ],
        colors.dim,
        colors.focus,
    ));

    out
}

/// 构建 Edit 视图的行（兼容现有调用与纯文本断言；命中用 [`build_edit_layout`]）。
#[cfg(test)]
pub(super) fn build_edit_lines(
    es: &LoginEditState,
    auth: &AuthController,
    focus: LoginField,
    cursor_pos: usize,
    colors: &LoginRenderColors,
) -> Vec<Line<'static>> {
    build_edit_layout(es, auth, focus, cursor_pos, colors).lines
}

fn type_line(es: &LoginEditState, focus: LoginField, colors: &LoginRenderColors) -> Line<'static> {
    let focused = focus == LoginField::ProviderType;
    let prefix = if focused { "❯ " } else { "  " };
    let value = match es.kind {
        Some(kind) => i18n::tr(kind.label_key()),
        None => es.raw_type.clone(),
    };
    let hint = if es.is_new() {
        format!("  ←/→/Space: {}", i18n::tr("hint-login-toggle"))
    } else {
        String::new()
    };
    Line::from(vec![
        Span::styled(prefix, Style::default().fg(colors.cursor)),
        Span::styled(
            format!("{}: ", i18n::tr(LoginField::ProviderType.i18n_key())),
            if focused {
                colors.focused_label()
            } else {
                colors.label()
            },
        ),
        Span::styled(
            format!("[{}]", value),
            Style::default()
                .fg(colors.text)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(hint, colors.label()),
    ])
}

fn profile_section(
    es: &LoginEditState,
    focus: LoginField,
    cursor_pos: usize,
    colors: &LoginRenderColors,
) -> EditLayout {
    let mut out = EditLayout::new();
    let profile = es.selected();
    out.push(Line::from(vec![
        Span::styled(
            format!("  {} ", i18n::tr("login-profile-section")),
            Style::default()
                .fg(colors.focus)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            i18n::tr_args(
                "login-profile-active",
                &[(
                    "alias".to_string(),
                    FluentValue::from(es.active_alias.as_str()),
                )],
            ),
            colors.label(),
        ),
    ]));

    // 档位选择器
    out.push_field(
        LoginField::ProfileSelector,
        choice_line(
            &i18n::tr("login-profile-selector"),
            format!("[{}]", profile.alias),
            focus == LoginField::ProfileSelector,
            colors,
        ),
    );

    // 绑定复选项 / 已绑定状态
    let bind_label = if profile.bound {
        if profile.fallback_bound {
            i18n::tr("login-profile-bound-fallback")
        } else {
            i18n::tr("login-profile-bound")
        }
    } else if profile.bind {
        "[x]".to_string()
    } else {
        "[ ]".to_string()
    };
    out.push_field(
        LoginField::ProfileBind,
        choice_line(
            &i18n::tr("login-profile-bind"),
            bind_label,
            focus == LoginField::ProfileBind,
            colors,
        ),
    );

    if profile.will_bind(es.is_new()) && !profile.bound && !profile.baseline_owner.is_empty() {
        out.push(Line::from(vec![Span::styled(
            format!(
                "    {}",
                i18n::tr_args(
                    "login-profile-rebind-warning",
                    &[(
                        "provider".to_string(),
                        FluentValue::from(profile.baseline_owner.as_str()),
                    )],
                )
            ),
            Style::default().fg(colors.focus),
        )]));
    }

    if profile.will_bind(es.is_new()) {
        out.push_field(
            LoginField::ProfileModel,
            render_login_edit_line(
                i18n::tr(LoginField::ProfileModel.i18n_key()),
                profile.model.clone(),
                focus == LoginField::ProfileModel,
                cursor_pos,
                colors.cursor,
                colors.dim,
                colors.text,
                colors.focus,
            ),
        );
        out.push_field(
            LoginField::ProfileWindow,
            render_login_edit_line(
                i18n::tr(LoginField::ProfileWindow.i18n_key()),
                profile.window.clone(),
                focus == LoginField::ProfileWindow,
                cursor_pos,
                colors.cursor,
                colors.dim,
                colors.text,
                colors.focus,
            ),
        );
        if es.profile_needs_reconfirm(profile) {
            out.push(Line::from(vec![Span::styled(
                format!("    {}", i18n::tr("login-profile-reconfirm")),
                Style::default().fg(colors.focus),
            )]));
        }
    }

    out.push(Line::from(vec![Span::styled(
        format!(
            "    {}",
            i18n::tr_args(
                "login-profile-summary",
                &[(
                    "profiles".to_string(),
                    FluentValue::from(es.profiles_to_update().as_str()),
                )],
            )
        ),
        colors.label(),
    )]));
    out
}

fn choice_line(
    label: &str,
    value: String,
    focused: bool,
    colors: &LoginRenderColors,
) -> Line<'static> {
    let prefix = if focused { "❯ " } else { "  " };
    Line::from(vec![
        Span::styled(prefix, Style::default().fg(colors.cursor)),
        Span::styled(
            format!("{}: ", label),
            if focused {
                colors.focused_label()
            } else {
                colors.label()
            },
        ),
        Span::styled(value, Style::default().fg(colors.text)),
    ])
}

fn action_line(label: String, focused: bool, colors: &LoginRenderColors) -> Line<'static> {
    let prefix = if focused { "❯ " } else { "  " };
    let style = if focused {
        Style::default()
            .fg(colors.cursor)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(colors.dim)
    };
    Line::from(vec![
        Span::styled(prefix, Style::default().fg(colors.cursor)),
        Span::styled(format!("  {}", label), style),
    ])
}

/// 显式类型更改子视图。
pub(super) fn build_type_change_layout(
    es: &LoginEditState,
    colors: &LoginRenderColors,
) -> EditLayout {
    let candidate = es.type_change_candidate.unwrap_or(ProviderKind::Anthropic);
    let current = es.kind.unwrap_or(candidate);
    let profiles = es.affected_aliases();
    let profiles = if profiles.is_empty() {
        i18n::tr("login-type-change-none")
    } else {
        profiles.join(", ")
    };
    let mut out = EditLayout::new();
    out.push(Line::from(vec![Span::styled(
        i18n::tr("login-type-change-title"),
        Style::default().fg(colors.focus).bold(),
    )]));
    out.push(Line::from(""));
    out.push(Line::from(vec![
        Span::styled(
            format!("  {}: ", i18n::tr("login-field-type")),
            colors.label(),
        ),
        Span::styled(
            format!("[{}]", i18n::tr(current.label_key())),
            Style::default().fg(colors.text),
        ),
    ]));
    out.push(Line::from(""));
    out.push_hit(
        Line::from(vec![
            Span::styled("  ❯ ", Style::default().fg(colors.cursor)),
            Span::styled(
                format!("[{}]", i18n::tr(candidate.label_key())),
                Style::default()
                    .fg(colors.text)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        EditHit::TypeChangeCandidate,
    );
    out.push(Line::from(""));
    out.push(Line::from(vec![Span::styled(
        format!(
            "  {}",
            i18n::tr_args(
                "login-type-change-impact",
                &[("profiles".to_string(), FluentValue::from(profiles.as_str()))],
            )
        ),
        Style::default().fg(colors.focus),
    )]));
    out.push(Line::from(""));
    out.push_hit(
        Line::from(vec![Span::styled(
            format!("  {}", i18n::tr("login-type-change-confirm")),
            colors.label(),
        )]),
        EditHit::TypeChangeConfirm,
    );
    out
}

/// 构建 Login 面板的底部提示行（风格与 setup_wizard 的 make_hint_line 一致）。
pub(super) fn make_hint_line_for_login(
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
pub(super) fn render_login_edit_line(
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

/// Provider 类型显示：三态使用本地化标签，未知原值不猜测。
pub(super) fn provider_type_display(provider_type: &str) -> String {
    match provider_type {
        "anthropic" => i18n::tr("setup-provider-anthropic"),
        "openai" => i18n::tr("setup-provider-openai"),
        "openai_responses" => i18n::tr("setup-provider-openai-responses"),
        raw => raw.to_string(),
    }
}

/// API Key 脱敏显示：保留最后 4 个字符，其余用 * 代替。
fn mask_api_key_display(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 4 {
        return "*".repeat(chars.len());
    }
    let visible: String = chars[chars.len() - 4..].iter().collect();
    format!("{}{}", "*".repeat(chars.len() - 4), visible)
}

/// 焦点字段在 Edit 视图中的语义行号（0-based），与 [`build_edit_layout`] 同源。
///
/// 事件边界用它把焦点滚入视口。认证区行数随状态变化，因此按“唯一 ❯ 行”定位，
/// 不再维护第二份手写行数推导。
pub(super) fn focus_row(es: &LoginEditState, auth: &AuthController, focus: LoginField) -> usize {
    build_edit_layout(es, auth, focus, 0, &reset_colors())
        .lines
        .iter()
        .position(|line| {
            line.spans
                .first()
                .is_some_and(|span| span.content.starts_with('❯'))
        })
        .unwrap_or(0)
}

/// Edit 视图总行数（与 [`build_edit_layout`] 共用同一布局实现）。
pub(super) fn line_count(es: &LoginEditState, auth: &AuthController, focus: LoginField) -> usize {
    build_edit_layout(es, auth, focus, 0, &reset_colors())
        .lines
        .len()
}

/// 语义行号 → Edit 鼠标目标（与绘制同一布局；颜色不参与命中）。
pub(super) fn edit_hit_at(
    es: &LoginEditState,
    auth: &AuthController,
    focus: LoginField,
    cursor_pos: usize,
    row: usize,
) -> Option<EditHit> {
    build_edit_layout(es, auth, focus, cursor_pos, &reset_colors()).hit_at(row)
}

/// 类型确认视图的语义行号 → 鼠标目标。
pub(super) fn type_change_hit_at(es: &LoginEditState, row: usize) -> Option<EditHit> {
    build_type_change_layout(es, &reset_colors()).hit_at(row)
}

fn reset_colors() -> LoginRenderColors {
    LoginRenderColors {
        cursor: Color::Reset,
        dim: Color::Reset,
        text: Color::Reset,
        focus: Color::Reset,
    }
}
