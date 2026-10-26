//! Plan authentication and authorization sub-view rendering.
//!
//! Pure line construction. The edit view and the mouse hit test both consume
//! [`AuthDetailLayout::buttons`], so wrapped URLs and narrow terminals do not
//! shift the action geometry.
//!
//! Edit 表单里的认证区只展示用户可决策的信息：认证方式、登录状态、基础 URL、
//! 警告和动作。账号引用/身份/Client ID 属于内部凭据细节，不渲染（由草稿与
//! 序列化完整保存）；凭据目录只在“高级”区只读展示。

use fluent_bundle::FluentValue;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::chatgpt_auth::AccountStatus;
use crate::i18n;
use crate::kit::image_safety::sanitize_for_terminal;
use crate::kit::panels::login::auth::{
    AuthController, AuthFailure, AuthViewState, DetailAction, LogoutConfirm, LogoutFocus,
};
use crate::kit::panels::login::auth_policy;
use crate::kit::panels::login::edit_handler::auth_scope;
use crate::kit::panels::login::edit_state::{LoginEditState, LoginField};
use crate::kit::panels::login::edit_view::{self, EditViewMetrics, StatusEntry, StatusTone};
use crate::kit::panels::login::render::{EditLayout, LoginRenderColors};
use crate::truncate::{middle_truncate_by_width, wrap_by_width};

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
/// 行与命中区来自同一次构造：点击认证方式/动作按钮的语义行不会因状态行变化
/// 而在绘制与命中之间漂移。账号引用、身份与 Client ID 不出现在任何一行。
pub(super) fn plan_auth_layout(
    es: &LoginEditState,
    auth: &AuthController,
    focus: LoginField,
    colors: &LoginRenderColors,
    metrics: &EditViewMetrics,
) -> EditLayout {
    let mut out = EditLayout::new();
    out.push_field_lines(
        LoginField::AuthMode,
        edit_view::choice_field_lines(
            metrics,
            &i18n::tr(LoginField::AuthMode.i18n_key()),
            format!("[{}]", auth_mode_label(es)),
            None,
            focus == LoginField::AuthMode,
            colors,
        ),
        focus == LoginField::AuthMode,
    );

    if !es.is_plan_mode() {
        return out;
    }

    let actions = auth.available_actions(&es.plan);
    // 认证进行中可能没有任何可用动作：焦点仍停在 AuthActions，用状态组第一行作为
    // 滚动锚点，避免出现“看不见的焦点行”。
    if focus == LoginField::AuthActions && actions.is_empty() {
        out.mark_focus_row(out.lines.len());
    }
    out.push_all(edit_view::status_lines(
        metrics,
        &i18n::tr("login-status-label"),
        &status_entries(es, auth),
        colors,
    ));

    out.push_all(edit_view::readonly_kv_lines(
        metrics,
        &i18n::tr(LoginField::BaseUrl.i18n_key()),
        peri_config::provider::DEFAULT_RESPONSES_URL,
        Some(&i18n::tr("login-auth-endpoint-locked")),
        colors,
    ));
    let custom_endpoint = es.base_url.trim();
    if !custom_endpoint.is_empty()
        && custom_endpoint != peri_config::provider::DEFAULT_RESPONSES_URL
    {
        out.push_all(edit_view::warning_lines(
            metrics,
            &i18n::tr("login-auth-plan-endpoint-warning"),
            colors,
        ));
    }
    if auth.login_succeeded() {
        out.push_all(edit_view::warning_lines(
            metrics,
            &i18n::tr("login-auth-unsaved"),
            colors,
        ));
    }

    if !actions.is_empty() {
        out.push(edit_view::action_group_line(
            metrics,
            &i18n::tr("login-auth-actions"),
            colors,
        ));
        let labels: Vec<String> = actions
            .iter()
            .map(|action| i18n::tr(action.i18n_key()))
            .collect();
        let selected = auth
            .selected_action(&es.plan)
            .and_then(|selected| actions.iter().position(|action| *action == selected));
        let (line, spans) = edit_view::action_row(
            metrics,
            &labels,
            selected,
            focus == LoginField::AuthActions,
            colors,
        );
        out.push_action_row(line, &spans, &actions, focus == LoginField::AuthActions);
    }
    out
}

fn status_entries(es: &LoginEditState, auth: &AuthController) -> Vec<StatusEntry> {
    match auth.view() {
        AuthViewState::Unchecked => vec![StatusEntry::new(
            i18n::tr("login-auth-unchecked"),
            StatusTone::Muted,
        )],
        AuthViewState::Checking => vec![StatusEntry::new(
            i18n::tr("login-auth-checking"),
            StatusTone::Info,
        )],
        AuthViewState::SignedOut => {
            let mut entries = vec![StatusEntry::new(
                i18n::tr("login-auth-signed-out"),
                StatusTone::Muted,
            )];
            // 已有完整引用时保存不阻断，但运行时一定会失败：给出非阻断警告。
            if es.is_plan_mode() && !es.plan.needs_identity() && auth.scope() == &auth_scope(es) {
                entries.push(StatusEntry::new(
                    i18n::tr("login-auth-credentials-missing"),
                    StatusTone::Warning,
                ));
            }
            entries
        }
        AuthViewState::SignedIn(status) => signed_in_entries(es, status),
        AuthViewState::StartingLogin => vec![StatusEntry::new(
            i18n::tr("login-auth-starting"),
            StatusTone::Info,
        )],
        AuthViewState::WaitingCallback { .. } => vec![StatusEntry::new(
            i18n::tr("login-auth-waiting"),
            StatusTone::Info,
        )],
        AuthViewState::Cancelling => vec![StatusEntry::new(
            i18n::tr("login-auth-cancelling"),
            StatusTone::Info,
        )],
        AuthViewState::LoggingOut => vec![StatusEntry::new(
            i18n::tr("login-auth-logging-out"),
            StatusTone::Info,
        )],
        AuthViewState::CancellingLogout => vec![StatusEntry::new(
            i18n::tr("login-auth-cancelling-logout"),
            StatusTone::Info,
        )],
        AuthViewState::LoggedOut(outcome) => vec![
            StatusEntry::new(i18n::tr("login-auth-local-cleared"), StatusTone::Success),
            StatusEntry::new(
                if outcome.remote_revoked {
                    i18n::tr("login-auth-remote-revoked")
                } else {
                    i18n::tr("login-auth-remote-unconfirmed")
                },
                if outcome.remote_revoked {
                    StatusTone::Success
                } else {
                    StatusTone::Warning
                },
            ),
        ],
        AuthViewState::OutcomeUnknown(reason) => {
            tracing::debug!(?reason, "ChatGPT authentication outcome unknown");
            vec![
                StatusEntry::new(i18n::tr("login-auth-outcome-unknown"), StatusTone::Warning),
                StatusEntry::new(i18n::tr("login-auth-unknown-query"), StatusTone::Info),
            ]
        }
        AuthViewState::Failed(failure) => {
            vec![StatusEntry::new(failure_text(*failure), StatusTone::Error)]
        }
    }
}

fn signed_in_entries(es: &LoginEditState, status: &AccountStatus) -> Vec<StatusEntry> {
    // 只展示 email；没有 email 时显示“已登录”，绝不回退到内部 subject。
    let email = status
        .email
        .as_deref()
        .map(sanitize_for_terminal)
        .map(|email| email.into_owned())
        .filter(|email| !email.trim().is_empty());
    let mut entries = vec![match email {
        Some(email) => StatusEntry::limited(
            i18n::tr_args(
                "login-auth-signed-in",
                &[("identity".to_string(), FluentValue::from(email.as_str()))],
            ),
            StatusTone::Success,
            2,
        ),
        None => StatusEntry::new(
            i18n::tr("login-auth-signed-in-no-email"),
            StatusTone::Success,
        ),
    }];
    entries.push(StatusEntry::new(
        i18n::tr(if status.plan_enabled {
            "login-auth-plan-enabled"
        } else {
            "login-auth-plan-disabled"
        }),
        if status.plan_enabled {
            StatusTone::Success
        } else {
            StatusTone::Warning
        },
    ));
    let freshness = auth_policy::TokenFreshness::classify(status.expires_at, now_epoch());
    match freshness {
        auth_policy::TokenFreshness::Unknown => entries.push(StatusEntry::new(
            i18n::tr(freshness.i18n_key()),
            StatusTone::Muted,
        )),
        auth_policy::TokenFreshness::Expired => entries.push(StatusEntry::new(
            i18n::tr_args(
                freshness.i18n_key(),
                &[(
                    "time".to_string(),
                    FluentValue::from(format_epoch(status.expires_at).as_str()),
                )],
            ),
            StatusTone::Warning,
        )),
        auth_policy::TokenFreshness::Valid | auth_policy::TokenFreshness::Expiring => {
            entries.push(StatusEntry::new(
                i18n::tr_args(
                    freshness.i18n_key(),
                    &[(
                        "time".to_string(),
                        FluentValue::from(format_epoch(status.expires_at).as_str()),
                    )],
                ),
                StatusTone::Info,
            ));
        }
    }
    if es.plan.identity_mismatch(status) {
        entries.push(StatusEntry::new(
            i18n::tr("login-auth-identity-mismatch"),
            StatusTone::Warning,
        ));
    }
    entries
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
        Style::default()
            .fg(colors.focus)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));

    match view.url() {
        Some(url) => {
            lines.push(Line::from(vec![Span::styled(
                format!("  {}", i18n::tr("login-auth-url-label")),
                colors.label(),
            )]));
            let content_width = (width.saturating_sub(4) as usize).max(16);
            for chunk in wrap_by_width(&sanitize_for_terminal(url), content_width) {
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

    // 凭据目录保留为只读信息（中间截断）；账号引用不展示。
    if let Some(scope) = auth.scope().target() {
        let label = i18n::tr("login-field-store");
        let display = scope.store.display().to_string();
        let path = sanitize_for_terminal(&display);
        let budget = usize::from(width)
            .saturating_sub(2 + label.width() + 2)
            .max(1);
        lines.push(Line::from(vec![
            Span::styled(format!("  {label}: "), Style::default().fg(colors.dim)),
            Span::styled(
                middle_truncate_by_width(&path, budget),
                Style::default().fg(colors.dim),
            ),
        ]));
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
        Style::default()
            .fg(colors.focus)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));
    // 注销确认不再展示账号引用：只列共享凭据位置的 provider。
    lines.push(Line::from(vec![Span::styled(
        format!(
            "  {}",
            i18n::tr_args(
                "login-auth-logout-confirm",
                &[(
                    "providers".to_string(),
                    FluentValue::from(confirm.providers.join(", ").as_str()),
                )],
            )
        ),
        Style::default().fg(colors.warning),
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
