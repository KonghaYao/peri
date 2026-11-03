//! Edit 视觉与隐藏字段的缓冲区级回归。
//!
//! 这些用例只断言用户可观察的结果：渲染缓冲区不出现内部凭据值，但候选/JSON
//! 仍完整保留；状态/警告/只读行不产生鼠标目标；中英文分区与列宽稳定。

use super::*;
use crate::chatgpt_auth::AccountStatus;
use crate::kit::panels::login::auth::{AuthController, AuthViewState, HostConfigAuthScope};
use crate::kit::panels::login::{auth_render, render};
use peri_acp::provider::config::{PeriConfig, ProviderAuthConfig};
use ratatui_kit::ratatui::text::Line;
use std::path::PathBuf;
use unicode_width::UnicodeWidthStr;

fn sentinel_store() -> PathBuf {
    std::env::temp_dir().join("sentinel-store")
}

fn plan_state_with_sentinels() -> (PeriConfig, LoginEditState) {
    let mut cfg = login_config();
    let provider = cfg
        .config
        .providers
        .iter_mut()
        .find(|provider| provider.id == "plan")
        .expect("fixture must contain the plan provider");
    provider.auth = Some(ProviderAuthConfig::ChatGptPlan {
        store: sentinel_store(),
        account: "sentinel-account".to_string(),
        subject: "sentinel-subject".to_string(),
        client_id: "sentinel-client".to_string(),
    });
    let state = LoginEditState::edit_provider(&cfg, "plan", None).unwrap();
    (cfg, state)
}

fn line_width(line: &Line<'_>) -> usize {
    line.spans.iter().map(|span| span.content.width()).sum()
}

fn text_of(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect()
}

#[test]
#[serial]
fn edit_never_renders_hidden_credentials_but_keeps_them_in_candidate() {
    let (_cfg, state) = plan_state_with_sentinels();
    let text = mount_edit_state_sized(
        state.clone(),
        LoginField::AuthActions,
        2,
        100,
        40,
        |_, _| {},
    );
    let text = buffer_text(&text);
    for sentinel in ["sentinel-account", "sentinel-subject", "sentinel-client"] {
        assert!(
            !text.contains(sentinel),
            "Edit 不得渲染内部凭据值 {sentinel}:\n{text}"
        );
    }
    let (_cfg2, layout_state) = plan_state_with_sentinels();
    let layout_text = text_of(
        &render::build_edit_layout(
            &layout_state,
            &AuthController::new(),
            LoginField::AuthActions,
            0,
            &render::LoginRenderColors::reset(),
            100,
        )
        .lines,
    );
    for hidden_label in ["账号引用", "身份", "Client ID", "store/account 引用"] {
        assert!(
            !layout_text.contains(hidden_label),
            "隐藏字段/说明不得出现：{hidden_label}\n{layout_text}"
        );
    }
    assert!(!text.contains("login-"), "不得出现裸 i18n key:\n{text}");
    // 凭据目录仍以只读形式出现在“高级”区。
    assert!(
        text.contains(sentinel_store().to_string_lossy().as_ref()),
        "只读凭据目录必须可见:\n{text}"
    );

    let candidate = state.build_candidate().expect("隐藏值必须仍可保存");
    let provider = candidate
        .config
        .providers
        .iter()
        .find(|provider| provider.id == "plan")
        .unwrap();
    let Some(ProviderAuthConfig::ChatGptPlan {
        store,
        account,
        subject,
        client_id,
    }) = &provider.auth
    else {
        panic!("candidate must keep the plan reference");
    };
    assert_eq!(store, &sentinel_store());
    assert_eq!(account.as_str(), "sentinel-account");
    assert_eq!(subject.as_str(), "sentinel-subject");
    assert_eq!(client_id.as_str(), "sentinel-client");

    let encoded = serde_json::to_value(provider).unwrap();
    assert_eq!(encoded["auth"]["account"], "sentinel-account");
    assert_eq!(encoded["auth"]["subject"], "sentinel-subject");
    assert_eq!(encoded["auth"]["clientId"], "sentinel-client");
    assert_eq!(
        encoded["auth"]["store"],
        serde_json::json!(sentinel_store())
    );
}

#[test]
#[serial]
fn signed_in_without_email_never_falls_back_to_subject() {
    let (_cfg, mut state) = plan_state_with_sentinels();
    state.plan.subject = "sentinel-subject".to_string();
    let mut auth = AuthController::new();
    auth.set_view_for_test(AuthViewState::SignedIn(AccountStatus {
        account_ref: "sentinel-account".to_string(),
        subject: "sentinel-subject".to_string(),
        email: None,
        client_id: "sentinel-client".to_string(),
        plan_enabled: true,
        expires_at: peri_time::now_wall()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0)
            + 3600,
    }));
    let layout = render::build_edit_layout(
        &state,
        &auth,
        LoginField::AuthActions,
        0,
        &render::LoginRenderColors::reset(),
        100,
    );
    let text = text_of(&layout.lines);
    assert!(
        text.contains(&crate::i18n::tr("login-auth-signed-in-no-email")),
        "无 email 时显示“已登录”:\n{text}"
    );
    assert!(
        !text.contains("sentinel-subject"),
        "不得回退显示 subject:\n{text}"
    );
    assert!(
        !text.contains("sentinel-account"),
        "不得显示 account:\n{text}"
    );
}

#[test]
fn auth_detail_and_logout_confirmation_hide_account_reference() {
    let colors = render::LoginRenderColors::reset();
    let mut controller = AuthController::new();
    assert!(
        controller.sync_scope(HostConfigAuthScope::new(
            "fixture-config".to_string(),
            crate::chatgpt_auth::AuthTarget::new(
                std::env::temp_dir()
                    .join("very-long-store-name-".repeat(8))
                    .join("credentials"),
                "sentinel-account".to_string(),
            ),
        ))
    );
    controller.set_view_for_test(AuthViewState::WaitingCallback {
        url: "https://auth.example.test/authorize?state=synthetic".into(),
    });
    let detail = auth_render::build_auth_detail_layout(&controller, 80, &colors);
    let text = text_of(&detail.lines);
    assert!(
        !text.contains("sentinel-account"),
        "AuthDetail 不得显示 account:\n{text}"
    );
    assert!(text.contains('…'), "长凭据目录必须中间截断:\n{text}");

    let mut logout = AuthController::new();
    assert!(logout.open_logout_confirm(
        crate::chatgpt_auth::AuthTarget::new(sentinel_store(), "sentinel-account".to_string(),),
        vec!["plan".to_string()],
    ));
    let layout = auth_render::build_logout_layout(logout.logout_confirm().unwrap(), &colors);
    let text = text_of(&layout.lines);
    assert!(
        !text.contains("sentinel-account"),
        "注销确认不得显示 account:\n{text}"
    );
    assert!(
        text.contains("plan"),
        "共享凭据的 provider 仍要列出:\n{text}"
    );
}

#[test]
#[serial]
fn account_error_moves_focus_to_auth_actions() {
    let (_cfg, mut state) = plan_state_with_sentinels();
    state.plan.account = "invalid/account".to_string();
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_edit_state_sized(
        state,
        LoginField::Confirm,
        2,
        100,
        40,
        move |index, panel| {
            if index == 0 {
                let result = panel.event(key(KeyCode::Enter), TEST_AREA);
                let focus = *panel.edit_focus.read();
                let error = panel
                    .edit_state
                    .read()
                    .as_ref()
                    .and_then(|state| state.inline_error.clone());
                let visible = panel
                    .edit_state
                    .read()
                    .as_ref()
                    .map(LoginEditState::visible_fields)
                    .unwrap_or_default();
                tx.send((result, focus, error, visible)).unwrap();
            }
            frame += 1;
            assert_eq!(frame, index + 1);
        },
    );

    let (result, focus, error, visible) = rx.try_recv().unwrap();
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(focus, LoginField::AuthActions, "隐藏账号错误必须重映射焦点");
    assert!(!visible.contains(&LoginField::PlanAccount));
    let error = error.expect("保存失败必须保留可见错误");
    assert_eq!(error.field, LoginField::AuthActions);
    assert_eq!(
        error.message,
        crate::i18n::tr("login-error-account-reference")
    );
}

#[test]
#[serial]
fn store_error_opens_the_recovery_row_and_focuses_it() {
    let (_cfg, mut state) = plan_state_with_sentinels();
    state.plan.store = "relative/store".to_string();
    assert!(!state.plan_store_editable, "进入时合法 store 默认只读");
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_edit_state_sized(
        state,
        LoginField::Confirm,
        2,
        100,
        40,
        move |index, panel| {
            if index == 0 {
                let _ = panel.event(key(KeyCode::Enter), TEST_AREA);
                let (focus, editable, visible) = {
                    let state = panel.edit_state.read();
                    let state = state.as_ref().unwrap();
                    (
                        *panel.edit_focus.read(),
                        state.plan_store_editable,
                        state.visible_fields(),
                    )
                };
                tx.send((focus, editable, visible)).unwrap();
            }
            frame += 1;
            assert_eq!(frame, index + 1);
        },
    );

    let (focus, editable, visible) = rx.try_recv().unwrap();
    assert!(editable, "保存错误指向凭据目录时必须强制打开恢复行");
    assert_eq!(focus, LoginField::PlanStore);
    assert!(
        visible.contains(&LoginField::PlanStore),
        "恢复行必须可见可聚焦"
    );
}

#[test]
#[serial]
fn status_warning_and_readonly_rows_have_no_mouse_target() {
    let (_cfg, state) = plan_state_with_sentinels();
    let area = tall_area();
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_edit_state_sized(
        state,
        LoginField::ProviderId,
        1,
        area.width,
        area.height,
        move |index, panel| {
            if index == 0 {
                let focus_before = *panel.edit_focus.read();
                let layout = edit_layout(&panel, area.width);
                let mut touched = Vec::new();
                for (row, line) in layout.lines.iter().enumerate() {
                    let text: String = line
                        .spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect();
                    let passive = text.contains('●')
                        || text.contains('⚠')
                        || text.contains(sentinel_store().to_string_lossy().as_ref());
                    if passive {
                        assert!(
                            layout.hits[row].is_none(),
                            "状态/警告/只读行不得有命中: {text}"
                        );
                        touched
                            .push((row, panel.event(click_at(area.y + 1 + row as u16, 3), area)));
                    }
                }
                tx.send((focus_before, *panel.edit_focus.read(), touched))
                    .unwrap();
            }
            frame += 1;
            assert_eq!(frame, index + 1);
        },
    );

    let (before, after, touched) = rx.try_recv().unwrap();
    assert!(!touched.is_empty(), "夹具必须包含状态/警告/只读行");
    for (row, result) in touched {
        assert_eq!(result, EventResult::Consumed, "row {row} 区域内点击仍消费");
    }
    assert_eq!(before, after, "点击状态/警告/只读行不得改变焦点");
}

#[test]
#[serial]
fn edit_sections_and_widths_are_language_stable() {
    let (_cfg, state) = plan_state_with_sentinels();
    for (lang, title, sections, hidden) in [
        (
            "zh-CN",
            "编辑 Provider",
            ["基本信息", "认证", "默认模型", "高级"],
            [
                "档位配置",
                "编辑档位",
                "绑定此 Provider",
                "此档模型",
                "上下文窗口",
                "账号引用",
                "身份",
                "Client ID",
                "store/account 引用",
            ],
        ),
        (
            "en",
            "Edit Provider",
            ["Basic", "Authentication", "Default models", "Advanced"],
            [
                "Profile configuration",
                "Profile",
                "Bind this provider",
                "Profile model",
                "Context window",
                "Account reference",
                "Identity",
                "Client ID",
                "store/account reference",
            ],
        ),
    ] {
        crate::i18n::init(Some(lang));
        let (tx, rx) = std::sync::mpsc::channel();
        let state = state.clone();
        let buffer = mount_edit_state_sized(
            state,
            LoginField::ProviderId,
            2,
            100,
            40,
            move |_, panel| {
                let _ = tx.send(text_of(&edit_layout(&panel, 100).lines));
            },
        );
        let text = rx.try_recv().unwrap();
        assert!(text.contains(title), "{lang} 标题缺失:\n{text}");
        for section in sections {
            assert!(
                text.contains(section),
                "{lang} 分区 {section} 缺失:\n{text}"
            );
        }
        for label in hidden {
            assert!(
                !text.contains(label),
                "{lang} 隐藏字段/说明不得出现：{label}\n{text}"
            );
        }
        let buffer_text = buffer_text(&buffer);
        for label in hidden {
            assert!(
                !buffer_text.contains(label),
                "{lang} 缓冲区不得出现已删除字段：{label}\n{buffer_text}"
            );
        }
        assert!(
            !buffer_text.contains("login-"),
            "{lang} 不得出现裸 key:\n{buffer_text}"
        );
        // 标题只由代码加 2 列缩进，FTL 不再自带前导空格。
        let first_content = buffer_line(&buffer, 1);
        assert!(
            first_content.starts_with("  /login"),
            "{lang} 标题缩进必须恰好 2 列: {first_content:?}"
        );
    }
    crate::i18n::init(Some("en"));
}

#[test]
#[serial]
fn main_decision_area_fits_100x40() {
    let (_cfg, state) = plan_state_with_sentinels();
    let area = Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 40,
    };
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_edit_state_sized(
        state,
        LoginField::ProviderId,
        1,
        area.width,
        area.height,
        move |index, panel| {
            if index == 0 {
                let layout = edit_layout(&panel, area.width);
                let confirm_row = layout
                    .hits
                    .iter()
                    .position(|hit| {
                        *hit == Some(super::render::EditHit::Field(LoginField::Confirm))
                    })
                    .expect("保存按钮必须可见");
                tx.send(confirm_row).unwrap();
            }
            frame += 1;
            assert_eq!(frame, index + 1);
        },
    );
    let confirm_row = rx.try_recv().unwrap();
    let content_height = area.height.saturating_sub(2) as usize;
    assert!(
        confirm_row < content_height,
        "100×40 主决策区（到保存按钮）不应滚动：row {confirm_row} / {content_height}"
    );
}

fn verified_status_for_actions() -> AuthViewState {
    AuthViewState::SignedIn(AccountStatus {
        account_ref: "sentinel-account".to_string(),
        subject: "different-subject".to_string(),
        email: Some("user@example.com".to_string()),
        client_id: "different-client".to_string(),
        plan_enabled: true,
        expires_at: peri_time::now_wall()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0)
            + 3600,
    })
}

fn action_labels_for(lang: &str) -> [&'static str; 4] {
    if lang == "zh-CN" {
        ["登录 ChatGPT", "查询状态", "使用此账号", "注销"]
    } else {
        [
            "Sign in to ChatGPT",
            "Check status",
            "Use this account",
            "Sign out",
        ]
    }
}

fn action_labels() -> [&'static str; 4] {
    action_labels_for("zh-CN")
}

#[test]
#[serial]
fn auth_actions_render_in_one_row_with_three_space_gaps() {
    let (_cfg, state) = plan_state_with_sentinels();
    let mut auth = AuthController::new();
    auth.set_view_for_test(verified_status_for_actions());
    for (lang, labels) in [
        ("zh-CN", action_labels_for("zh-CN")),
        ("en", action_labels_for("en")),
    ] {
        crate::i18n::init(Some(lang));
        for width in [80u16, 100] {
            let layout = render::build_edit_layout(
                &state,
                &auth,
                LoginField::AuthActions,
                0,
                &render::LoginRenderColors::reset(),
                width,
            );
            let matching: Vec<&Line<'_>> = layout
                .lines
                .iter()
                .filter(|line| {
                    labels.iter().any(|label| {
                        let label = format!("[{label}]");
                        line.spans.iter().any(|span| span.content.contains(&label))
                    })
                })
                .collect();
            let matching_text: Vec<String> = matching
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect()
                })
                .collect();
            assert_eq!(
                matching.len(),
                1,
                "width={width} 动作必须只占一行: {matching_text:?}"
            );
            let row: String = matching[0]
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            for label in labels {
                assert!(
                    row.contains(label),
                    "{lang} width={width} 缺少动作 {label}: {row}"
                );
            }
            assert!(
                row.contains("]   ["),
                "width={width} 动作之间必须有 3 空格间隔: {row:?}"
            );
            assert!(
                render::build_edit_layout(
                    &state,
                    &auth,
                    LoginField::AuthActions,
                    0,
                    &render::LoginRenderColors::reset(),
                    width,
                )
                .lines
                .iter()
                .filter(|line| {
                    let text: String = line
                        .spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect();
                    text.contains('❯') || text.contains('»')
                })
                .count()
                    >= 1,
                "动作行必须保留选中标记"
            );
        }
    }
    crate::i18n::init(Some("en"));
}

#[test]
#[serial]
fn auth_action_row_stays_single_line_and_keeps_selected_visible_when_narrow() {
    crate::i18n::init(Some("zh-CN"));
    let (_cfg, state) = plan_state_with_sentinels();
    let mut auth = AuthController::new();
    auth.set_view_for_test(verified_status_for_actions());
    // 走到最后一个动作（Logout），窗口必须跟随选中项。
    for _ in 0..3 {
        auth.cycle_action(true, &state.plan);
    }
    assert_eq!(
        auth.selected_action(&state.plan),
        Some(super::auth::AuthAction::Logout)
    );
    let width = 40u16;
    let layout = render::build_edit_layout(
        &state,
        &auth,
        LoginField::AuthActions,
        0,
        &render::LoginRenderColors::reset(),
        width,
    );
    let rows: Vec<(usize, String)> = layout
        .lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let text: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            action_labels()
                .iter()
                .any(|label| text.contains(&format!("[{label}]")))
                .then_some((index, text))
        })
        .collect();
    assert_eq!(rows.len(), 1, "窄屏也必须只占一行: {rows:?}");
    let (row_index, row) = &rows[0];
    assert!(row.contains("注销"), "选中项必须可见: {row}");
    assert!(
        row.contains('‹') || row.contains('›'),
        "两侧被裁时必须显示指示符: {row}"
    );
    assert!(
        line_width(&layout.lines[*row_index]) < usize::from(width),
        "动作行超宽: {row}"
    );
    crate::i18n::init(Some("en"));
}

#[test]
#[serial]
fn mouse_columns_resolve_each_action_label_only() {
    let (_cfg, state) = plan_state_with_sentinels();
    let mut auth = AuthController::new();
    auth.set_view_for_test(verified_status_for_actions());
    let width = 80u16;
    let layout = render::build_edit_layout(
        &state,
        &auth,
        LoginField::AuthActions,
        0,
        &render::LoginRenderColors::reset(),
        width,
    );
    let spans = layout.action_spans().to_vec();
    assert_eq!(spans.len(), 4, "四个动作都必须有独立命中区间");
    for span in &spans {
        for column in [span.start, span.end - 1] {
            assert_eq!(
                render::edit_hit_at(
                    &state,
                    &auth,
                    LoginField::AuthActions,
                    0,
                    span.row,
                    Some(column),
                    width,
                ),
                Some(super::render::EditHit::AuthAction(span.action)),
                "动作 {:?} 的列 {column} 必须命中自身",
                span.action
            );
        }
    }
    let row = spans[0].row;
    assert!(
        !matches!(
            render::edit_hit_at(
                &state,
                &auth,
                LoginField::AuthActions,
                0,
                row,
                Some(4),
                width,
            ),
            Some(super::render::EditHit::AuthAction(_))
        ),
        "动作行的缩进/标记列不得命中动作"
    );
}

#[test]
#[serial]
fn auth_actions_move_with_left_right_and_enter_executes() {
    let (_cfg, state) = plan_state_with_sentinels();
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_edit_state_sized(
        state,
        LoginField::AuthActions,
        2,
        80,
        40,
        move |index, panel| {
            if index == 0 {
                let plan = panel
                    .edit_state
                    .read()
                    .as_ref()
                    .map(|state| state.plan.clone())
                    .unwrap();
                let first = panel.auth.read().selected_action(&plan);
                let _ = panel.event(key(KeyCode::Right), TEST_AREA);
                let second = panel.auth.read().selected_action(&plan);
                let _ = panel.event(key(KeyCode::Right), TEST_AREA);
                let third = panel.auth.read().selected_action(&plan);
                let focus_after_move = *panel.edit_focus.read();
                let _ = panel.event(key(KeyCode::Left), TEST_AREA);
                let back = panel.auth.read().selected_action(&plan);
                let _ = panel.event(key(KeyCode::Right), TEST_AREA);
                let _ = panel.event(key(KeyCode::Enter), TEST_AREA);
                tx.send((
                    first,
                    second,
                    third,
                    back,
                    focus_after_move,
                    *panel.mode.read(),
                ))
                .unwrap();
            }
            frame += 1;
            assert_eq!(frame, index + 1);
        },
    );

    let (first, second, third, back, focus, mode) = rx.try_recv().unwrap();
    assert_eq!(first, Some(super::auth::AuthAction::Login));
    assert_eq!(second, Some(super::auth::AuthAction::Status));
    assert_eq!(third, Some(super::auth::AuthAction::Logout));
    assert_eq!(back, Some(super::auth::AuthAction::Status));
    assert_eq!(focus, LoginField::AuthActions, "←/→ 不得移出动作组");
    assert_eq!(mode, LoginPanelMode::ConfirmLogout, "Enter 执行选中动作");
}

// ── Access token 语义与“已登录仍要求登录”回归 ────────────────────────────────

fn expired_status() -> AccountStatus {
    AccountStatus {
        account_ref: "sentinel-account".to_string(),
        subject: "sentinel-subject".to_string(),
        email: Some("user@example.com".to_string()),
        client_id: "sentinel-client".to_string(),
        plan_enabled: true,
        expires_at: peri_time::now_wall()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(60)
            .saturating_sub(60),
    }
}

#[test]
#[serial]
fn expired_access_token_copy_is_not_authorization_expiry() {
    let (_cfg, state) = plan_state_with_sentinels();
    let mut auth = AuthController::new();
    auth.set_view_for_test(AuthViewState::SignedIn(expired_status()));
    let layout = render::build_edit_layout(
        &state,
        &auth,
        LoginField::AuthActions,
        0,
        &render::LoginRenderColors::reset(),
        100,
    );
    let text = text_of(&layout.lines);
    assert!(
        !text.contains("Local grant") && !text.contains("本地授权"),
        "过期只能描述访问令牌，不能断言授权失效:\n{text}"
    );
    assert!(
        text.to_lowercase().contains("access token") || text.contains("访问令牌"),
        "必须明确是访问令牌:\n{text}"
    );
    assert!(
        text.contains("refresh") || text.contains("刷新"),
        "必须说明使用时会自动刷新:\n{text}"
    );
}

#[test]
#[serial]
fn signed_in_status_never_renders_login_required() {
    let (_cfg, mut state) = plan_state_with_sentinels();
    state.plan.subject.clear();
    state.plan.client_id.clear();
    let mut auth = AuthController::new();
    auth.set_view_for_test(AuthViewState::SignedIn(AccountStatus {
        account_ref: "sentinel-account".to_string(),
        subject: "verified-subject".to_string(),
        email: Some("user@example.com".to_string()),
        client_id: "verified-client".to_string(),
        plan_enabled: true,
        expires_at: peri_time::now_wall()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0)
            + 3600,
    }));
    let layout = render::build_edit_layout(
        &state,
        &auth,
        LoginField::AuthActions,
        0,
        &render::LoginRenderColors::reset(),
        100,
    );
    let text = text_of(&layout.lines);
    assert!(
        !text.contains(&crate::i18n::tr("login-auth-identity-required")),
        "SignedIn 不得渲染“缺少身份/请先登录”:\n{text}"
    );
    assert!(
        !text.contains(&crate::i18n::tr("login-auth-identity-signed-out")),
        "SignedIn 不得渲染未登录:\n{text}"
    );
}

#[test]
#[serial]
fn new_plan_provider_without_model_source_renders_actionable_hint() {
    let cfg = PeriConfig::default();
    let mut state = LoginEditState::new_provider(&cfg, None);
    state.kind = Some(crate::kit::panels::login::edit_state::ProviderKind::OpenAiResponses);
    state.cycle_auth_mode();
    assert!(state.model_defaults_hint_visible());

    let layout = render::build_edit_layout(
        &state,
        &AuthController::new(),
        LoginField::Confirm,
        0,
        &render::LoginRenderColors::reset(),
        100,
    );
    let text = text_of(&layout.lines);
    assert!(
        text.contains(&crate::i18n::tr("login-model-defaults-source-missing")),
        "没有可复用来源时必须给出可操作提示:\n{text}"
    );
    assert!(!text.contains("login-"), "不得渲染裸 i18n key:\n{text}");
}
