//! Plan authentication and authorization sub-view rendering.
//!
//! Pure line construction. The edit view and the mouse hit test both consume
//! [`AuthDetailLayout::buttons`], so wrapped URLs and narrow terminals do not
//! shift the action geometry.

use fluent_bundle::FluentValue;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::chatgpt_auth::AccountStatus;
use crate::i18n;
use crate::kit::image_safety::sanitize_for_terminal;
use crate::kit::panels::login::auth::{
    AuthController, AuthFailure, AuthViewState, DetailAction, LogoutConfirm, LogoutFocus,
};
use crate::kit::panels::login::edit_state::{LoginEditState, LoginField};
use crate::kit::panels::login::render::{
    EditHit, EditLayout, LoginRenderColors, render_login_edit_line,
};

/// Authorization sub-view geometry.
pub(super) struct AuthDetailLayout {
    pub(super) lines: Vec<Line<'static>>,
    /// `(content row, action)` for mouse hit testing.
    pub(super) buttons: Vec<(usize, DetailAction)>,
}

/// Logout confirmation geometry.
pub(super) struct LogoutLayout {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) buttons: Vec<(usize, LogoutFocus)>,
}

/// Render the Responses authentication block inside the edit form.
///
/// 行与命中区来自同一次构造：点击认证方式/plan 字段/动作按钮的语义行不会因状态
/// 行变化而在绘制与命中之间漂移。
pub(super) fn plan_auth_layout(
    es: &LoginEditState,
    auth: &AuthController,
    focus: LoginField,
    cursor_pos: usize,
    colors: &LoginRenderColors,
) -> EditLayout {
    let mut out = EditLayout::new();
    out.push_field(
        LoginField::AuthMode,
        choice_line(
            &i18n::tr("login-field-auth-mode"),
            format!("[{}]", auth_mode_label(es)),
            focus == LoginField::AuthMode,
            colors,
        ),
    );

    if !es.is_plan_mode() {
        return out;
    }

    out.push_field(
        LoginField::PlanStore,
        render_login_edit_line(
            i18n::tr(LoginField::PlanStore.i18n_key()),
            es.plan.store.clone(),
            focus == LoginField::PlanStore,
            cursor_pos,
            colors.cursor,
            colors.dim,
            colors.text,
            colors.focus,
        ),
    );
    out.push_field(
        LoginField::PlanAccount,
        render_login_edit_line(
            i18n::tr(LoginField::PlanAccount.i18n_key()),
            es.plan.account.clone(),
            focus == LoginField::PlanAccount,
            cursor_pos,
            colors.cursor,
            colors.dim,
            colors.text,
            colors.focus,
        ),
    );

    out.push(read_only_line(
        &i18n::tr("login-field-subject"),
        nonempty_or(&es.plan.subject, i18n::tr("login-auth-not-verified")),
        colors,
    ));
    out.push(read_only_line(
        &i18n::tr("login-field-client-id"),
        nonempty_or(&es.plan.client_id, i18n::tr("login-auth-not-verified")),
        colors,
    ));
    out.push(read_only_line(
        &i18n::tr("login-field-base-url"),
        format!(
            "{} ({})",
            peri_config::provider::DEFAULT_RESPONSES_URL,
            i18n::tr("login-auth-endpoint-locked")
        ),
        colors,
    ));
    let custom_endpoint = es.base_url.trim();
    if !custom_endpoint.is_empty()
        && custom_endpoint != peri_config::provider::DEFAULT_RESPONSES_URL
    {
        out.push(Line::from(vec![Span::styled(
            format!("    {}", i18n::tr("login-auth-plan-endpoint-warning")),
            Style::default().fg(colors.focus),
        )]));
    }

    for line in status_lines(es, auth.view(), colors) {
        out.push(line);
    }
    if auth.login_succeeded() {
        out.push(Line::from(vec![Span::styled(
            format!("  {}", i18n::tr("login-auth-unsaved")),
            Style::default()
                .fg(colors.focus)
                .add_modifier(Modifier::BOLD),
        )]));
    }

    out.push(Line::from(vec![Span::styled(
        format!("  {}", i18n::tr("login-auth-actions")),
        Style::default().fg(colors.dim).add_modifier(Modifier::BOLD),
    )]));
    let actions = auth.available_actions(&es.plan);
    for action in actions {
        let selected = auth.selected_action(&es.plan) == Some(action);
        let marker = if focus == LoginField::AuthActions && selected {
            "❯ "
        } else if selected {
            "» "
        } else {
            "  "
        };
        let style = if focus == LoginField::AuthActions && selected {
            Style::default()
                .fg(colors.focus)
                .add_modifier(Modifier::BOLD)
        } else if selected {
            Style::default().fg(colors.text)
        } else {
            Style::default().fg(colors.dim)
        };
        out.push_hit(
            Line::from(vec![
                Span::styled(marker, Style::default().fg(colors.cursor)),
                Span::styled(format!("[{}]", i18n::tr(action.i18n_key())), style),
            ]),
            EditHit::AuthAction(action),
        );
    }

    out.push(Line::from(vec![Span::styled(
        format!("    {}", i18n::tr("login-auth-plan-reference-note")),
        colors.label(),
    )]));
    out
}

fn status_lines(
    es: &LoginEditState,
    view: &AuthViewState,
    colors: &LoginRenderColors,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match view {
        AuthViewState::Unchecked => lines.push(status_line(
            i18n::tr("login-auth-unchecked"),
            colors.dim,
            colors,
        )),
        AuthViewState::Checking => lines.push(status_line(
            i18n::tr("login-auth-checking"),
            colors.focus,
            colors,
        )),
        AuthViewState::SignedOut => {
            lines.push(status_line(
                i18n::tr("login-auth-signed-out"),
                colors.dim,
                colors,
            ));
        }
        AuthViewState::SignedIn(status) => {
            lines.extend(signed_in_lines(es, status, colors));
        }
        AuthViewState::StartingLogin => lines.push(status_line(
            i18n::tr("login-auth-starting"),
            colors.focus,
            colors,
        )),
        AuthViewState::WaitingCallback { .. } => {
            lines.push(status_line(
                i18n::tr("login-auth-waiting"),
                colors.focus,
                colors,
            ));
        }
        AuthViewState::Cancelling => lines.push(status_line(
            i18n::tr("login-auth-cancelling"),
            colors.focus,
            colors,
        )),
        AuthViewState::LoggingOut => lines.push(status_line(
            i18n::tr("login-auth-logging-out"),
            colors.focus,
            colors,
        )),
        AuthViewState::CancellingLogout => lines.push(status_line(
            i18n::tr("login-auth-cancelling-logout"),
            colors.focus,
            colors,
        )),
        AuthViewState::LoggedOut(outcome) => {
            lines.push(status_line(
                i18n::tr("login-auth-local-cleared"),
                colors.focus,
                colors,
            ));
            lines.push(status_line(
                if outcome.remote_revoked {
                    i18n::tr("login-auth-remote-revoked")
                } else {
                    i18n::tr("login-auth-remote-unconfirmed")
                },
                if outcome.remote_revoked {
                    colors.text
                } else {
                    colors.focus
                },
                colors,
            ));
        }
        AuthViewState::OutcomeUnknown(reason) => {
            tracing::debug!(?reason, "ChatGPT authentication outcome unknown");
            lines.push(status_line(
                i18n::tr("login-auth-outcome-unknown"),
                colors.focus,
                colors,
            ));
            lines.push(status_line(
                i18n::tr("login-auth-unknown-query"),
                colors.dim,
                colors,
            ));
        }
        AuthViewState::Failed(failure) => {
            lines.push(status_line(failure_text(*failure), Color::Red, colors))
        }
    }
    lines
}

fn signed_in_lines(
    es: &LoginEditState,
    status: &AccountStatus,
    colors: &LoginRenderColors,
) -> Vec<Line<'static>> {
    let identity = status
        .email
        .as_deref()
        .or(Some(status.subject.as_str()))
        .unwrap_or("");
    let identity = sanitize_for_terminal(identity).into_owned();
    let mut lines = vec![status_line(
        i18n::tr_args(
            "login-auth-signed-in",
            &[("identity".to_string(), FluentValue::from(identity.as_str()))],
        ),
        colors.focus,
        colors,
    )];
    lines.push(status_line(
        i18n::tr(if status.plan_enabled {
            "login-auth-plan-enabled"
        } else {
            "login-auth-plan-disabled"
        }),
        if status.plan_enabled {
            colors.text
        } else {
            Color::Red
        },
        colors,
    ));
    let expires = format_epoch(status.expires_at);
    if status.expires_at <= now_epoch() {
        lines.push(status_line(
            i18n::tr_args(
                "login-auth-expired",
                &[("time".to_string(), FluentValue::from(expires.as_str()))],
            ),
            Color::Red,
            colors,
        ));
    } else {
        lines.push(status_line(
            i18n::tr_args(
                "login-auth-expires",
                &[("time".to_string(), FluentValue::from(expires.as_str()))],
            ),
            colors.dim,
            colors,
        ));
    }
    if es.plan.identity_mismatch(status) {
        lines.push(status_line(
            i18n::tr("login-auth-identity-mismatch"),
            colors.focus,
            colors,
        ));
    } else if !es.plan.needs_identity() {
        lines.push(status_line(
            i18n::tr("login-auth-identity-match"),
            colors.dim,
            colors,
        ));
    }
    lines
}

pub(super) fn build_auth_detail_layout(
    auth: &AuthController,
    width: u16,
    colors: &LoginRenderColors,
) -> AuthDetailLayout {
    let view = auth.view();
    let mut lines = Vec::new();
    let mut buttons = Vec::new();

    lines.push(Line::from(vec![Span::styled(
        format!("  {}", i18n::tr("login-auth-title")),
        Style::default().fg(colors.focus).bold(),
    )]));
    lines.push(Line::from(""));

    match view.url() {
        Some(url) => {
            lines.push(Line::from(vec![Span::styled(
                format!("  {}", i18n::tr("login-auth-url-label")),
                colors.label(),
            )]));
            let content_width = (width.saturating_sub(4) as usize).max(16);
            for chunk in wrap_chars(&sanitize_for_terminal(url), content_width) {
                lines.push(Line::from(vec![Span::styled(
                    format!("    {chunk}"),
                    Style::default().fg(colors.text),
                )]));
            }
            lines.push(Line::from(""));
        }
        None => {
            lines.push(Line::from(vec![Span::styled(
                format!("  {}", detail_status_text(view)),
                Style::default().fg(colors.focus),
            )]));
            lines.push(Line::from(""));
        }
    }

    if let Some(scope) = auth.scope().target() {
        lines.push(Line::from(vec![Span::styled(
            format!(
                "  {}: {}",
                i18n::tr("login-field-store"),
                sanitize_for_terminal(&scope.store.display().to_string())
            ),
            Style::default().fg(colors.dim),
        )]));
        lines.push(Line::from(vec![Span::styled(
            format!(
                "  {}: {}",
                i18n::tr("login-field-account"),
                sanitize_for_terminal(&scope.account)
            ),
            Style::default().fg(colors.dim),
        )]));
    }
    lines.push(Line::from(vec![Span::styled(
        format!("  {}", i18n::tr("login-auth-loopback-local-hint")),
        colors.label(),
    )]));
    lines.push(Line::from(""));

    for action in auth.detail_actions() {
        let focused = auth.detail_focus() == action;
        let marker = if focused { "❯ " } else { "  " };
        let style = if focused {
            Style::default()
                .fg(colors.focus)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(colors.text)
        };
        buttons.push((lines.len(), action));
        lines.push(Line::from(vec![
            Span::styled(marker, Style::default().fg(colors.cursor)),
            Span::styled(format!("[{}]", i18n::tr(action.i18n_key())), style),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        format!("  {}", i18n::tr("login-auth-detail-hint")),
        colors.label(),
    )]));

    AuthDetailLayout { lines, buttons }
}

pub(super) fn build_logout_layout(
    confirm: &LogoutConfirm,
    colors: &LoginRenderColors,
) -> LogoutLayout {
    let mut lines = Vec::new();
    let mut buttons = Vec::new();
    lines.push(Line::from(vec![Span::styled(
        i18n::tr("login-auth-logout-title"),
        Style::default().fg(colors.focus).bold(),
    )]));
    lines.push(Line::from(""));
    let account = sanitize_for_terminal(&confirm.target.account).into_owned();
    lines.push(Line::from(vec![Span::styled(
        format!(
            "  {}",
            i18n::tr_args(
                "login-auth-logout-confirm",
                &[
                    ("account".to_string(), FluentValue::from(account.as_str())),
                    (
                        "providers".to_string(),
                        FluentValue::from(confirm.providers.join(", ").as_str()),
                    ),
                ],
            )
        ),
        Style::default().fg(colors.focus),
    )]));
    lines.push(Line::from(vec![Span::styled(
        format!("  {}", i18n::tr("login-auth-logout-scope-note")),
        colors.label(),
    )]));
    lines.push(Line::from(""));

    for (focus, key) in [
        (LogoutFocus::Cancel, "login-auth-cancel-logout"),
        (LogoutFocus::Confirm, "login-auth-confirm-logout"),
    ] {
        let marker = if confirm.focus == focus { "❯ " } else { "  " };
        buttons.push((lines.len(), focus));
        lines.push(Line::from(vec![
            Span::styled(marker, Style::default().fg(colors.cursor)),
            Span::styled(
                format!("[{}]", i18n::tr(key)),
                if confirm.focus == focus {
                    Style::default()
                        .fg(colors.focus)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(colors.text)
                },
            ),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        format!("  {}", i18n::tr("login-auth-logout-hint")),
        colors.label(),
    )]));
    LogoutLayout { lines, buttons }
}

fn detail_status_text(view: &AuthViewState) -> String {
    match view {
        AuthViewState::StartingLogin => i18n::tr("login-auth-starting"),
        AuthViewState::Cancelling => i18n::tr("login-auth-cancelling"),
        AuthViewState::Failed(failure) => failure_text(*failure),
        _ => i18n::tr("login-auth-waiting"),
    }
}

fn failure_text(failure: AuthFailure) -> String {
    i18n::tr(failure.i18n_key())
}

fn auth_mode_label(es: &LoginEditState) -> String {
    if es.is_plan_mode() {
        i18n::tr("login-auth-chatgpt-plan")
    } else {
        i18n::tr("login-auth-api-key")
    }
}

fn status_line(message: String, color: Color, colors: &LoginRenderColors) -> Line<'static> {
    Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(
            format!("● {message}"),
            Style::default().fg(color_or(color, colors.text)),
        ),
    ])
}

fn color_or(color: Color, fallback: Color) -> Color {
    if color == Color::Reset {
        fallback
    } else {
        color
    }
}

fn read_only_line(label: &str, value: String, colors: &LoginRenderColors) -> Line<'static> {
    Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(format!("{label}: "), colors.label()),
        Span::styled(
            sanitize_for_terminal(&value).into_owned(),
            Style::default().fg(colors.text),
        ),
    ])
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
            format!("{label}: "),
            if focused {
                colors.focused_label()
            } else {
                colors.label()
            },
        ),
        Span::styled(value, Style::default().fg(colors.text)),
    ])
}

fn nonempty_or(value: &str, fallback: String) -> String {
    if value.trim().is_empty() {
        fallback
    } else {
        value.to_owned()
    }
}

fn wrap_chars(text: &str, width: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    chars
        .chunks(width.max(1))
        .map(|chunk| chunk.iter().collect())
        .collect()
}

fn now_epoch() -> u64 {
    peri_time::now_wall()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn format_epoch(epoch: u64) -> String {
    let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(epoch);
    peri_time::format_utc_rfc3339(time)
}
