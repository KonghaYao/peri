//! Login 保存门与 ChatGPT 身份的端到端提交回归。
//!
//! 与 `commit_test` 共用隔离子进程夹具：`PERI_CONFIG_HANDLE` 是进程级
//! OnceLock，每个场景必须在独立子进程中运行。

use super::*;
use crate::chatgpt_auth::AccountStatus;
use crate::kit::panels::login::auth::AuthViewState;
use crate::kit::panels::login::edit_handler::auth_scope;
use crate::kit::panels::login::edit_state::{AuthChoice, LoginError};

fn plan_store() -> std::path::PathBuf {
    std::env::temp_dir().join("peri-login-commit")
}

fn isolated(name: &str, scenario: impl std::future::Future<Output = ()>) {
    super::run_isolated(name, module_path!().split_once("::").unwrap().1, scenario);
}

/// 新建 Responses + ChatGPT plan，四档默认模型已填，身份暂空。
fn new_plan_state(fixture: &Fixture) -> LoginEditState {
    let cfg = fixture.config.read().clone();
    let mut state = LoginEditState::new_provider(&cfg, Some(fixture.revision()));
    state.kind = Some(ProviderKind::OpenAiResponses);
    state.raw_type = ProviderKind::OpenAiResponses.as_str().to_string();
    state.provider_id = "new-plan".into();
    state.auth_choice = AuthChoice::ChatGptPlan;
    state.plan.store = plan_store().to_string_lossy().into_owned();
    state.plan.account = "work".into();
    state.plan.subject.clear();
    state.plan.client_id.clear();
    state.models = ProviderModels {
        fable: "plan-fable".into(),
        opus: "plan-opus".into(),
        sonnet: "plan-sonnet".into(),
        haiku: "plan-haiku".into(),
    };
    state
}

fn signed_in_status(subject: &str, client_id: &str, expires_at: u64) -> AccountStatus {
    AccountStatus {
        account_ref: "work".into(),
        subject: subject.into(),
        email: Some("synthetic@example.test".into()),
        client_id: client_id.into(),
        plan_enabled: true,
        expires_at,
    }
}

fn press_save(
    mode: &mut LoginPanelMode,
    edit_state: &mut Option<LoginEditState>,
    edit_focus: &mut LoginField,
    edit_cursor: &mut usize,
    auth: &mut AuthController,
) {
    handle_login_edit_keys(
        LoginEditContext {
            mode,
            edit_state,
            edit_focus,
            edit_cursor,
            auth,
        },
        false,
        None,
        &KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
    );
}

#[test]
fn login_commit_signed_in_empty_identity_saves_once_with_four_models() {
    isolated(
        "login_commit_signed_in_empty_identity_saves_once_with_four_models",
        async {
            let fixture = Fixture::new(false);
            let mut state = new_plan_state(&fixture);
            // 上一轮遗留的身份错误不得阻断本次保存。
            state.inline_error = Some(LoginError::identity_required(
                LoginField::AuthActions,
                crate::i18n::tr("login-auth-identity-required"),
            ));
            let mut auth = AuthController::new();
            let scope = auth_scope(&state);
            assert!(auth.sync_scope(scope));
            // 已过期的 access token 也不得阻断保存：刷新由首次使用负责。
            auth.set_view_for_test(AuthViewState::SignedIn(signed_in_status(
                "verified-subject",
                "verified-client",
                1,
            )));

            let mut mode = LoginPanelMode::Edit;
            let mut edit_state = Some(state);
            let mut focus = LoginField::Confirm;
            let mut cursor = 0usize;
            press_save(
                &mut mode,
                &mut edit_state,
                &mut focus,
                &mut cursor,
                &mut auth,
            );

            assert_eq!(mode, LoginPanelMode::Browse, "一次保存必须成功关闭");
            assert!(edit_state.is_none());
            assert_ne!(
                NOTIFICATION
                    .state()
                    .read()
                    .as_ref()
                    .map(|note| note.message.clone()),
                Some(crate::i18n::tr("login-auth-identity-required")),
                "不得再出现“请先登录”类红字"
            );
            fixture.expect_request(false).await;

            let saved = fixture.config.read().clone();
            assert_eq!(saved.config.active_alias, "sonnet");
            let provider = saved
                .config
                .providers
                .iter()
                .find(|provider| provider.id == "new-plan")
                .unwrap();
            let Some(peri_acp::provider::config::ProviderAuthConfig::ChatGptPlan {
                store,
                account,
                subject,
                client_id,
            }) = &provider.auth
            else {
                panic!("plan identity must be saved");
            };
            assert_eq!(store, &plan_store());
            assert_eq!(account, "work");
            assert_eq!(subject, "verified-subject");
            assert_eq!(client_id, "verified-client");
            assert_eq!(provider.models.fable, "plan-fable");
            assert_eq!(provider.models.opus, "plan-opus");
            assert_eq!(provider.models.sonnet, "plan-sonnet");
            assert_eq!(provider.models.haiku, "plan-haiku");
            for alias in Profiles::ALL {
                assert_eq!(
                    saved.config.profiles.get(alias),
                    initial_config().config.profiles.get(alias),
                    "{alias} must stay unchanged: Login only writes provider defaults"
                );
            }
            fixture.finish().await;
        },
    );
}

#[test]
fn login_commit_checking_save_waits_without_red_then_adopts_on_status() {
    isolated(
        "login_commit_checking_save_waits_without_red_then_adopts_on_status",
        async {
            let fixture = Fixture::new(false);
            let state = new_plan_state(&fixture);
            let mut auth = AuthController::new();
            let scope = auth_scope(&state);
            assert!(auth.sync_scope(scope));
            auth.set_view_for_test(AuthViewState::Checking);

            let mut mode = LoginPanelMode::Edit;
            let mut edit_state = Some(state);
            let mut focus = LoginField::Confirm;
            let mut cursor = 0usize;
            press_save(
                &mut mode,
                &mut edit_state,
                &mut focus,
                &mut cursor,
                &mut auth,
            );

            assert_eq!(mode, LoginPanelMode::Edit, "查询中不得假保存");
            let error = edit_state
                .as_ref()
                .and_then(|state| state.inline_error.as_ref());
            assert!(error.is_none(), "Checking 保存不得写红字");
            assert_eq!(
                NOTIFICATION
                    .state()
                    .read()
                    .as_ref()
                    .map(|note| note.message.clone()),
                Some(crate::i18n::tr("login-auth-identity-checking")),
                "Checking 只提示等待"
            );

            // 状态随后成功：保存门最后机会采用，再次保存成功。
            auth.set_view_for_test(AuthViewState::SignedIn(signed_in_status(
                "verified-subject",
                "verified-client",
                u64::MAX / 2,
            )));
            press_save(
                &mut mode,
                &mut edit_state,
                &mut focus,
                &mut cursor,
                &mut auth,
            );
            assert_eq!(mode, LoginPanelMode::Browse);
            assert!(edit_state.is_none());
            fixture.expect_request(false).await;
            fixture.finish().await;
        },
    );
}

#[test]
fn login_commit_mismatch_blocks_then_explicit_adoption_saves() {
    isolated(
        "login_commit_mismatch_blocks_then_explicit_adoption_saves",
        async {
            let fixture = Fixture::new(false);
            let cfg = fixture.config.read().clone();
            let state =
                LoginEditState::edit_provider(&cfg, "plan", Some(fixture.revision())).unwrap();
            let mut auth = AuthController::new();
            let scope = auth_scope(&state);
            assert!(auth.sync_scope(scope));
            auth.set_view_for_test(AuthViewState::SignedIn(signed_in_status(
                "verified-subject",
                "verified-client",
                u64::MAX / 2,
            )));

            let mut mode = LoginPanelMode::Edit;
            let mut edit_state = Some(state);
            let mut focus = LoginField::Confirm;
            let mut cursor = 0usize;
            press_save(
                &mut mode,
                &mut edit_state,
                &mut focus,
                &mut cursor,
                &mut auth,
            );

            assert_eq!(mode, LoginPanelMode::Edit, "不同身份必须阻断保存");
            let blocked = edit_state.as_ref().unwrap();
            let error = blocked.inline_error.as_ref().unwrap();
            assert_eq!(error.kind, LoginErrorKind::AuthReference);
            assert_eq!(
                error.message,
                crate::i18n::tr("login-auth-identity-mismatch")
            );
            assert_eq!(focus, LoginField::AuthActions);
            assert_eq!(
                auth.selected_action(&blocked.plan),
                Some(crate::kit::panels::login::auth::AuthAction::UseAccount),
                "默认选中“使用此账号”，Enter 可直接采用"
            );

            // 显式采用：动作行 Enter。
            handle_login_edit_keys(
                LoginEditContext {
                    mode: &mut mode,
                    edit_state: &mut edit_state,
                    edit_focus: &mut focus,
                    edit_cursor: &mut cursor,
                    auth: &mut auth,
                },
                false,
                None,
                &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            );
            let adopted = edit_state.as_ref().unwrap();
            assert_eq!(adopted.plan.subject, "verified-subject");
            assert_eq!(adopted.plan.client_id, "verified-client");
            assert!(adopted.inline_error.is_none());
            assert_eq!(
                auth.selected_action(&adopted.plan),
                Some(crate::kit::panels::login::auth::AuthAction::Status),
                "采用后动作复位到查询状态"
            );

            press_save(
                &mut mode,
                &mut edit_state,
                &mut focus,
                &mut cursor,
                &mut auth,
            );
            assert_eq!(mode, LoginPanelMode::Browse);
            fixture.expect_request(false).await;
            let saved = fixture.config.read().clone();
            let provider = saved
                .config
                .providers
                .iter()
                .find(|provider| provider.id == "plan")
                .unwrap();
            let Some(peri_acp::provider::config::ProviderAuthConfig::ChatGptPlan {
                subject,
                client_id,
                ..
            }) = &provider.auth
            else {
                panic!("plan identity must be saved");
            };
            assert_eq!(subject, "verified-subject");
            assert_eq!(client_id, "verified-client");
            for alias in Profiles::ALL {
                assert_eq!(
                    saved.config.profiles.get(alias),
                    initial_config().config.profiles.get(alias),
                    "{alias} must stay unchanged"
                );
            }
            fixture.finish().await;
        },
    );
}

#[test]
fn login_commit_ctrl_r_syncs_auth_scope_and_requests_status() {
    isolated(
        "login_commit_ctrl_r_syncs_auth_scope_and_requests_status",
        async {
            let fixture = Fixture::new(false);
            let cfg = fixture.config.read().clone();
            let state =
                LoginEditState::edit_provider(&cfg, "plan", Some(fixture.revision())).unwrap();
            let expected_scope = auth_scope(&state);
            let mut auth = AuthController::new();
            // 旧目标的“已登录”不得在重载后继续显示或参与采用。
            assert!(
                auth.sync_scope(crate::kit::panels::login::auth::HostConfigAuthScope::new(
                    "old-source",
                    crate::chatgpt_auth::AuthTarget::new(
                        std::env::temp_dir().join("old-store"),
                        "old-account".to_string(),
                    ),
                ))
            );
            auth.set_view_for_test(AuthViewState::SignedIn(signed_in_status(
                "old-subject",
                "old-client",
                u64::MAX / 2,
            )));

            let mut mode = LoginPanelMode::Edit;
            let mut edit_state = Some(state);
            let mut focus = LoginField::ProviderId;
            let mut cursor = 0usize;
            let intent = handle_login_edit_keys(
                LoginEditContext {
                    mode: &mut mode,
                    edit_state: &mut edit_state,
                    edit_focus: &mut focus,
                    edit_cursor: &mut cursor,
                    auth: &mut auth,
                },
                false,
                None,
                &KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
            );
            assert_eq!(
                intent,
                crate::kit::panels::login::edit_handler::LoginEditIntent::QueryStatus
            );
            assert_eq!(auth.scope(), &expected_scope);
            assert!(
                matches!(auth.view(), AuthViewState::Unchecked),
                "作用域同步后必须回到未查询，不能保留旧目标的已登录"
            );
            assert_eq!(mode, LoginPanelMode::Edit);
            fixture.finish().await;
        },
    );
}

#[test]
fn login_commit_new_plan_provider_seeds_models_before_saving() {
    isolated(
        "login_commit_new_plan_provider_seeds_models_before_saving",
        async {
            let fixture = Fixture::new(false);
            let cfg = fixture.config.read().clone();
            let mut state = LoginEditState::new_provider(&cfg, Some(fixture.revision()));
            state.kind = Some(ProviderKind::OpenAiResponses);
            state.raw_type = ProviderKind::OpenAiResponses.as_str().to_string();
            state.provider_id = "seeded-plan".into();
            // 切到 ChatGPT plan 时从已保存的同类 plan Provider 预填四档模型。
            state.cycle_auth_mode();
            assert_eq!(state.models.fable, "plan-fable");
            assert_eq!(state.models.opus, "plan-opus");
            assert_eq!(state.models.sonnet, "plan-sonnet");
            assert_eq!(state.models.haiku, "plan-haiku");
            state.ensure_plan_defaults(Some(plan_store()));
            assert_eq!(state.models.opus, "plan-opus", "预填后再次触发不覆盖");

            let mut auth = AuthController::new();
            let scope = auth_scope(&state);
            assert!(auth.sync_scope(scope));
            auth.set_view_for_test(AuthViewState::SignedIn(signed_in_status(
                "verified-subject",
                "verified-client",
                u64::MAX / 2,
            )));

            let mut mode = LoginPanelMode::Edit;
            let mut edit_state = Some(state);
            let mut focus = LoginField::Confirm;
            let mut cursor = 0usize;
            press_save(
                &mut mode,
                &mut edit_state,
                &mut focus,
                &mut cursor,
                &mut auth,
            );
            assert_eq!(mode, LoginPanelMode::Browse);
            fixture.expect_request(false).await;

            let saved = fixture.config.read().clone();
            let provider = saved
                .config
                .providers
                .iter()
                .find(|provider| provider.id == "seeded-plan")
                .unwrap();
            assert_eq!(provider.models.fable, "plan-fable");
            assert_eq!(provider.models.opus, "plan-opus");
            assert_eq!(provider.models.sonnet, "plan-sonnet");
            assert_eq!(provider.models.haiku, "plan-haiku");
            for alias in Profiles::ALL {
                assert_eq!(
                    saved.config.profiles.get(alias),
                    initial_config().config.profiles.get(alias),
                    "{alias} must stay unchanged: auto-fill only touches Login draft"
                );
            }
            fixture.finish().await;
        },
    );
}
