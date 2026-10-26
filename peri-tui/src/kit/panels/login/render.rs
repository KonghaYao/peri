use crate::i18n;
use fluent_bundle::FluentValue;

use super::auth::{AuthAction, AuthController};
use super::auth_render;
use super::edit_state::{LoginEditState, LoginField, ProviderKind};
use super::edit_view::{self, EditViewMetrics};
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

/// 单行动作区里的一个动作命中区间（内容列坐标；`end` 不含）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ActionHitSpan {
    pub(super) row: usize,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) action: AuthAction,
}

/// Edit / 类型确认视图的行与逐行命中区（`hits` 与 `lines` 等长）。
pub(super) struct EditLayout {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) hits: Vec<Option<EditHit>>,
    /// 单行动作区的列区间命中（同一行含多个动作）。
    pub(super) action_hits: Vec<ActionHitSpan>,
    /// 焦点字段的语义行（0-based）。显式记录，不再扫描文本里的 `❯`。
    pub(super) focus_row: Option<usize>,
}

impl EditLayout {
    pub(super) fn new() -> Self {
        Self {
            lines: Vec::new(),
            hits: Vec::new(),
            action_hits: Vec::new(),
            focus_row: None,
        }
    }

    pub(super) fn push(&mut self, line: Line<'static>) {
        self.lines.push(line);
        self.hits.push(None);
    }

    pub(super) fn push_all(&mut self, lines: Vec<Line<'static>>) {
        for line in lines {
            self.push(line);
        }
    }

    pub(super) fn push_hit(&mut self, line: Line<'static>, hit: EditHit) {
        self.lines.push(line);
        self.hits.push(Some(hit));
    }

    /// 带焦点标记的命中行：`focused` 时把该行记为焦点行（只取第一处）。
    pub(super) fn push_hit_flagged(&mut self, line: Line<'static>, hit: EditHit, focused: bool) {
        if focused && self.focus_row.is_none() {
            self.focus_row = Some(self.lines.len());
        }
        self.push_hit(line, hit);
    }

    /// 一个字段的多行（窄屏标签/值两行，或字段 + 锁定提示）：共用同一命中。
    pub(super) fn push_field_lines(
        &mut self,
        field: LoginField,
        lines: Vec<Line<'static>>,
        focused: bool,
    ) {
        for (index, line) in lines.into_iter().enumerate() {
            self.push_hit_flagged(line, EditHit::Field(field), focused && index == 0);
        }
    }

    /// 单行动作区：所有动作在同一行，按列区间区分目标；行本身没有字段命中。
    pub(super) fn push_action_row(
        &mut self,
        line: Line<'static>,
        spans: &[edit_view::ActionSpan],
        actions: &[AuthAction],
        focused: bool,
    ) {
        let row = self.lines.len();
        if focused {
            self.mark_focus_row(row);
        }
        for span in spans {
            if let Some(action) = actions.get(span.index) {
                self.action_hits.push(ActionHitSpan {
                    row,
                    start: span.start,
                    end: span.end,
                    action: *action,
                });
            }
        }
        self.push(line);
    }

    /// 焦点没有可命中的行时（如认证进行中暂无动作），把状态区作为滚动锚点。
    pub(super) fn mark_focus_row(&mut self, row: usize) {
        if self.focus_row.is_none() {
            self.focus_row = Some(row);
        }
    }

    fn extend(&mut self, other: EditLayout) {
        let base = self.lines.len();
        if self.focus_row.is_none()
            && let Some(row) = other.focus_row
        {
            self.focus_row = Some(base + row);
        }
        for mut span in other.action_hits {
            span.row += base;
            self.action_hits.push(span);
        }
        self.lines.extend(other.lines);
        self.hits.extend(other.hits);
    }

    /// 行 + 内容列 → 语义目标。动作行先按列区间命中，其它行沿用整行命中。
    fn hit_at(&self, row: usize, column: Option<usize>) -> Option<EditHit> {
        if let Some(column) = column
            && let Some(span) = self
                .action_hits
                .iter()
                .find(|span| span.row == row && column >= span.start && column < span.end)
        {
            return Some(EditHit::AuthAction(span.action));
        }
        self.hits.get(row).copied().flatten()
    }

    /// 测试/滚动用：某语义目标所在的语义行。
    #[cfg(test)]
    pub(super) fn semantic_row(&self, hit: EditHit) -> Option<usize> {
        if let EditHit::AuthAction(action) = hit {
            return self
                .action_hits
                .iter()
                .find(|span| span.action == action)
                .map(|span| span.row);
        }
        self.hits
            .iter()
            .position(|candidate| *candidate == Some(hit))
    }

    /// 测试用：动作区的列区间。
    #[cfg(test)]
    pub(super) fn action_spans(&self) -> &[ActionHitSpan] {
        &self.action_hits
    }
}

// ── 渲染辅助函数 ──────────────────────────────────────────────────────────────

/// Edit 视图配色（由调用方从当前主题派生；渲染辅助函数不读全局状态）。
pub(super) struct LoginRenderColors {
    pub(super) cursor: Color,
    pub(super) dim: Color,
    pub(super) text: Color,
    pub(super) focus: Color,
    pub(super) muted: Color,
    pub(super) warning: Color,
    pub(super) error: Color,
}

impl LoginRenderColors {
    /// 从主题语义令牌派生；分区标题/警告/错误各自有独立角色。
    pub(super) fn from_semantic(semantic: &peri_theme::semantic::SemanticTokens) -> Self {
        Self {
            cursor: semantic.status.warning,
            dim: semantic.text.dim,
            text: semantic.text.primary,
            focus: semantic.status.success,
            muted: semantic.text.muted,
            warning: semantic.status.warning,
            error: semantic.status.error,
        }
    }

    /// 命中/行数推导只关心几何，颜色不参与。
    pub(super) fn reset() -> Self {
        Self {
            cursor: Color::Reset,
            dim: Color::Reset,
            text: Color::Reset,
            focus: Color::Reset,
            muted: Color::Reset,
            warning: Color::Reset,
            error: Color::Reset,
        }
    }

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
    width: u16,
) -> EditLayout {
    let metrics = EditViewMetrics::for_width(width);
    let mut out = EditLayout::new();
    let is_new = es.is_new();

    // ── 页面标题 ──
    let title = if is_new {
        i18n::tr("login-panel-title-new")
    } else {
        format!(
            "{}: {}",
            i18n::tr("login-panel-title-edit"),
            es.original_provider_id
        )
    };
    out.push(edit_view::title_line(&metrics, &title, colors));
    out.push(edit_view::blank_line());

    // ── 基本信息 ──
    out.push(edit_view::section_line(
        &metrics,
        &i18n::tr("login-section-basic"),
        None,
        colors,
    ));
    out.push_field_lines(
        LoginField::ProviderType,
        type_lines(es, focus, colors, &metrics),
        focus == LoginField::ProviderType,
    );
    out.push_field_lines(
        LoginField::ProviderId,
        edit_view::text_field_lines(
            &metrics,
            &i18n::tr(LoginField::ProviderId.i18n_key()),
            &es.provider_id,
            cursor_pos,
            focus == LoginField::ProviderId,
            colors,
        ),
        focus == LoginField::ProviderId,
    );

    // ── 认证 ──
    out.push(edit_view::blank_line());
    out.push(edit_view::section_line(
        &metrics,
        &i18n::tr("login-section-auth"),
        None,
        colors,
    ));
    if es.is_responses() {
        out.extend(auth_render::plan_auth_layout(
            es, auth, focus, colors, &metrics,
        ));
        // API key 模式字段（plan 模式由认证区接管，避免残留引用）。
        if !es.is_plan_mode() {
            for field in [LoginField::BaseUrl, LoginField::ApiKey] {
                out.push_field_lines(
                    field,
                    edit_view::text_field_lines(
                        &metrics,
                        &i18n::tr(field.i18n_key()),
                        &field_display_value(es, field),
                        cursor_pos,
                        field == focus,
                        colors,
                    ),
                    field == focus,
                );
            }
        }
    } else {
        // 非 Responses provider 没有认证方式选择，保留只读说明。
        out.push_all(edit_view::readonly_kv_lines(
            &metrics,
            &i18n::tr("login-field-auth-mode"),
            &i18n::tr("login-auth-api-key"),
            None,
            colors,
        ));
        for field in [LoginField::BaseUrl, LoginField::ApiKey] {
            out.push_field_lines(
                field,
                edit_view::text_field_lines(
                    &metrics,
                    &i18n::tr(field.i18n_key()),
                    &field_display_value(es, field),
                    cursor_pos,
                    field == focus,
                    colors,
                ),
                field == focus,
            );
        }
    }

    // ── 默认模型 ──
    out.push(edit_view::blank_line());
    out.push(edit_view::section_line(
        &metrics,
        &i18n::tr("login-model-label"),
        None,
        colors,
    ));
    for field in [
        LoginField::FableModel,
        LoginField::OpusModel,
        LoginField::SonnetModel,
        LoginField::HaikuModel,
    ] {
        out.push_field_lines(
            field,
            edit_view::text_field_lines(
                &metrics,
                &i18n::tr(field.i18n_key()),
                &es.field_text(field),
                cursor_pos,
                field == focus,
                colors,
            ),
            field == focus,
        );
    }
    if es.model_defaults_hint_visible() {
        out.push_all(edit_view::note_lines(
            &metrics,
            &i18n::tr("login-model-defaults-source-missing"),
            colors,
        ));
    }

    // ── 高级：凭据目录只读；缺失/非法/保存错误时恢复为可编辑 ──
    if es.is_plan_mode() {
        out.push(edit_view::blank_line());
        out.push(edit_view::section_line(
            &metrics,
            &i18n::tr("login-section-advanced"),
            None,
            colors,
        ));
        if es.plan_store_editable {
            out.push_field_lines(
                LoginField::PlanStore,
                edit_view::text_field_lines(
                    &metrics,
                    &i18n::tr(LoginField::PlanStore.i18n_key()),
                    &es.plan.store,
                    cursor_pos,
                    focus == LoginField::PlanStore,
                    colors,
                ),
                focus == LoginField::PlanStore,
            );
        } else {
            out.push_all(edit_view::readonly_kv_lines(
                &metrics,
                &i18n::tr(LoginField::PlanStore.i18n_key()),
                &es.plan.store,
                None,
                colors,
            ));
        }
    }

    // ── 保存 ──
    out.push(edit_view::blank_line());
    out.push(edit_view::summary_line(
        &metrics,
        i18n::tr_args(
            "login-save-summary",
            &[(
                "id".to_string(),
                FluentValue::from(if es.provider_id.trim().is_empty() {
                    "…"
                } else {
                    es.provider_id.trim()
                }),
            )],
        ),
        colors,
    ));
    if let Some(error) = &es.inline_error {
        out.push_all(edit_view::error_lines(&metrics, &error.message, colors));
        // 只有 revision/authority 冲突才提示重新载入；字段校验错误重载不会改变结果。
        if error.kind == crate::kit::panels::login::edit_state::LoginErrorKind::Conflict {
            out.push_all(edit_view::note_lines(
                &metrics,
                &i18n::tr("login-conflict-reload-hint"),
                colors,
            ));
        }
    }
    out.push_field_lines(
        LoginField::Confirm,
        vec![edit_view::button_line(
            &metrics,
            &i18n::tr("login-confirm"),
            focus == LoginField::Confirm,
            colors,
        )],
        focus == LoginField::Confirm,
    );

    out.push(edit_view::blank_line());
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

/// API key 字段显示值（掩码），其它字段直接取草稿文本。
fn field_display_value(es: &LoginEditState, field: LoginField) -> String {
    if field == LoginField::ApiKey {
        mask_api_key_display(&es.api_key)
    } else {
        es.field_text(field)
    }
}

/// 构建 Edit 视图的行（兼容现有调用与纯文本断言；命中用 [`build_edit_layout`]）。
#[cfg(test)]
pub(super) fn build_edit_lines(
    es: &LoginEditState,
    auth: &AuthController,
    focus: LoginField,
    cursor_pos: usize,
    colors: &LoginRenderColors,
    width: u16,
) -> Vec<Line<'static>> {
    build_edit_layout(es, auth, focus, cursor_pos, colors, width).lines
}

/// 类型行：新建时方向键循环；已有 provider 需要 Enter 显式确认。
///
/// 锁定提示作为字段续行，与类型行同一命中（点击提示也可聚焦）。
fn type_lines(
    es: &LoginEditState,
    focus: LoginField,
    colors: &LoginRenderColors,
    metrics: &EditViewMetrics,
) -> Vec<Line<'static>> {
    let value = match es.kind {
        Some(kind) => i18n::tr(kind.label_key()),
        None => es.raw_type.clone(),
    };
    let hint = if es.is_new() {
        Some(format!(
            "\u{2190}/\u{2192}/Space: {}",
            i18n::tr("hint-login-toggle")
        ))
    } else {
        None
    };
    let mut lines = edit_view::choice_field_lines(
        metrics,
        &i18n::tr(LoginField::ProviderType.i18n_key()),
        format!("[{value}]"),
        hint,
        focus == LoginField::ProviderType,
        colors,
    );
    if !es.is_new() {
        lines.push(edit_view::continuation_line(
            metrics,
            &i18n::tr("login-type-locked-hint"),
            colors,
        ));
    }
    lines
}

/// 显式类型更改子视图。
pub(super) fn build_type_change_layout(
    es: &LoginEditState,
    colors: &LoginRenderColors,
) -> EditLayout {
    let candidate = es.type_change_candidate.unwrap_or(ProviderKind::Anthropic);
    let current = es.kind.unwrap_or(candidate);
    let profiles = es.bound_aliases();
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
/// 焦点行由布局显式记录；找不到时回退到第一个可命中行，最后才回退 0。
/// 事件边界用它把焦点滚入视口，不再扫描文本里的 `❯`。
pub(super) fn focus_row(
    es: &LoginEditState,
    auth: &AuthController,
    focus: LoginField,
    width: u16,
) -> usize {
    let layout = build_edit_layout(es, auth, focus, 0, &LoginRenderColors::reset(), width);
    layout
        .focus_row
        .or_else(|| layout.hits.iter().position(|hit| hit.is_some()))
        .unwrap_or(0)
}

/// Edit 视图总行数（与 [`build_edit_layout`] 共用同一布局实现）。
pub(super) fn line_count(
    es: &LoginEditState,
    auth: &AuthController,
    focus: LoginField,
    width: u16,
) -> usize {
    build_edit_layout(es, auth, focus, 0, &LoginRenderColors::reset(), width)
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
    column: Option<usize>,
    width: u16,
) -> Option<EditHit> {
    build_edit_layout(
        es,
        auth,
        focus,
        cursor_pos,
        &LoginRenderColors::reset(),
        width,
    )
    .hit_at(row, column)
}

/// 类型确认视图的语义行号 → 鼠标目标。
pub(super) fn type_change_hit_at(es: &LoginEditState, row: usize) -> Option<EditHit> {
    build_type_change_layout(es, &LoginRenderColors::reset()).hit_at(row, None)
}
