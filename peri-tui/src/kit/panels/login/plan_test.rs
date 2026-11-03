use super::edit_state::{AuthChoice, LoginEditState, LoginField};
use std::path::PathBuf;

fn plan_store() -> String {
    std::env::temp_dir()
        .join("peri-plan-store")
        .to_string_lossy()
        .into_owned()
}

fn default_store() -> PathBuf {
    std::env::temp_dir().join("default-store")
}

// ── Plan candidate serialization ──────────────────────────────────────────────

fn responses_config() -> peri_acp::provider::config::PeriConfig {
    serde_json::from_value(serde_json::json!({"config": {
        "active_alias": "sonnet",
        "providers": [
            {"id":"resp", "type":"openai_responses", "apiKey":"fixture-key",
             "models":{"fable":"fable-model", "opus":"opus-model", "sonnet":"sonnet-model", "haiku":"haiku-model"}}
        ],
        "profiles": {
            "fable":{"provider":"resp", "contextWindow":1000},
            "opus":{"provider":"resp", "contextWindow":2000},
            "sonnet":{"provider":"resp", "contextWindow":3000},
            "haiku":{"provider":"resp", "contextWindow":4000}
        }
    }}))
    .unwrap()
}

#[test]
fn plan_mode_writes_tagged_auth_with_empty_api_key_and_official_endpoint() {
    let cfg = responses_config();
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    edit.cycle_auth_mode();
    edit.plan.store = plan_store();
    edit.plan.account = "work".into();
    edit.plan.subject = "subject-one".into();
    edit.plan.client_id = "client-one".into();
    edit.base_url = "https://custom.invalid/v1/responses".into();

    let candidate = edit.build_candidate().unwrap();
    let provider = candidate
        .config
        .providers
        .iter()
        .find(|provider| provider.id == "resp")
        .unwrap();
    match &provider.auth {
        Some(peri_acp::provider::config::ProviderAuthConfig::ChatGptPlan {
            store,
            account,
            subject,
            client_id,
        }) => {
            assert_eq!(store, &PathBuf::from(plan_store()));
            assert_eq!(account, "work");
            assert_eq!(subject, "subject-one");
            assert_eq!(client_id, "client-one");
        }
        other => panic!("expected tagged chatgpt_plan auth, got {other:?}"),
    }
    assert!(
        provider.api_key.is_empty(),
        "plan mode must not persist apiKey"
    );
    assert!(
        provider.base_url.is_empty(),
        "plan mode clears custom endpoints and uses the official default"
    );
}

#[test]
fn plan_mode_rejects_relative_store_and_missing_identity() {
    let cfg = responses_config();
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    edit.cycle_auth_mode();
    edit.plan.store = "relative/store".into();
    edit.plan.account = "work".into();
    let error = edit.build_candidate().unwrap_err();
    assert_eq!(error.field, LoginField::PlanStore);

    edit.plan.store = plan_store();
    edit.plan.subject.clear();
    let error = edit.build_candidate().unwrap_err();
    assert_eq!(error.field, LoginField::AuthActions);

    edit.plan.subject = "subject".into();
    edit.plan.client_id = "client".into();
    assert!(edit.build_candidate().is_ok());
}

#[test]
fn plan_mode_rejects_invalid_account_reference() {
    let cfg = responses_config();
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    edit.cycle_auth_mode();
    edit.plan.store = plan_store();
    edit.plan.account = "work/slash".into();
    edit.plan.subject = "subject".into();
    edit.plan.client_id = "client".into();
    let error = edit.build_candidate().unwrap_err();
    assert_eq!(error.field, LoginField::AuthActions);
    assert_eq!(
        error.message,
        crate::i18n::tr("login-error-account-reference")
    );
}

#[test]
fn plan_account_uses_unmodified_shared_reference_rules() {
    let cfg = responses_config();
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    edit.cycle_auth_mode();
    edit.plan.store = plan_store();
    edit.plan.subject = "subject".into();
    edit.plan.client_id = "client".into();

    for input in [
        "a".to_owned(),
        "SYNTHETIC_account-42".to_owned(),
        "x".repeat(128),
    ] {
        edit.plan.account = input.clone();
        let candidate = edit.build_candidate().unwrap();
        assert!(matches!(
            &candidate.config.providers[0].auth,
            Some(peri_acp::provider::config::ProviderAuthConfig::ChatGptPlan { account, .. })
                if account == &input
        ));
        let target = super::edit_handler::auth_scope(&edit).target().unwrap();
        assert_eq!(target.account, input);
    }
    for input in [
        "".to_owned(),
        "work/team".to_owned(),
        " work".to_owned(),
        "work ".to_owned(),
        " work ".to_owned(),
        " ".to_owned(),
        "work team".to_owned(),
        "work\n".to_owned(),
        "合成".to_owned(),
        "x".repeat(129),
        format!("{} ", "x".repeat(128)),
    ] {
        edit.plan.account = input;
        assert!(super::edit_handler::auth_scope(&edit).target().is_none());
        let error = edit.build_candidate().unwrap_err();
        assert_eq!(error.field, LoginField::AuthActions);
        assert_eq!(
            error.message,
            crate::i18n::tr("login-error-account-reference")
        );
    }
}

#[test]
fn switching_back_to_api_key_drops_plan_reference() {
    let cfg = responses_config();
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    edit.cycle_auth_mode();
    edit.plan.store = plan_store();
    edit.plan.account = "work".into();
    edit.plan.subject = "subject".into();
    edit.plan.client_id = "client".into();
    edit.cycle_auth_mode();
    assert_eq!(edit.auth_choice, AuthChoice::ApiKey);
    edit.api_key = "api-mode-key".into();
    edit.base_url = "https://api.openai.com/v1/responses".into();

    let candidate = edit.build_candidate().unwrap();
    let provider = candidate
        .config
        .providers
        .iter()
        .find(|provider| provider.id == "resp")
        .unwrap();
    assert!(!matches!(
        provider.auth,
        Some(peri_acp::provider::config::ProviderAuthConfig::ChatGptPlan { .. })
    ));
    assert_eq!(provider.api_key, "api-mode-key");
}

#[test]
fn auth_mode_cycle_keeps_both_drafts() {
    let cfg = responses_config();
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    edit.api_key = "api-key-draft".into();
    edit.cycle_auth_mode();
    edit.plan.store = plan_store();
    edit.plan.account = "work".into();
    edit.cycle_auth_mode();
    assert_eq!(edit.api_key, "api-key-draft");
    edit.cycle_auth_mode();
    assert_eq!(edit.plan.store, plan_store());
}

#[test]
fn plan_defaults_only_fill_empty_fields() {
    let cfg = responses_config();
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    edit.cycle_auth_mode();
    edit.plan.store = "/custom/store".into();
    edit.plan.account = "custom".into();
    edit.ensure_plan_defaults(Some(default_store()));
    assert_eq!(edit.plan.store, "/custom/store");
    assert_eq!(edit.plan.account, "custom");

    edit.plan.account = " ".into();
    edit.ensure_plan_defaults(Some(default_store()));
    assert_eq!(edit.plan.account, " ", "空白输入不得被归一为默认账号");

    edit.plan.store.clear();
    edit.plan.account.clear();
    edit.ensure_plan_defaults(Some(default_store()));
    assert_eq!(edit.plan.store, default_store().to_string_lossy());
    assert_eq!(edit.plan.account, "default");
}

#[test]
fn plan_mode_navigation_uses_plan_fields_and_never_api_key() {
    let cfg = responses_config();
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    edit.cycle_auth_mode();
    edit.ensure_plan_defaults(Some(default_store()));
    let fields = edit.visible_fields();
    assert!(fields.contains(&LoginField::AuthMode));
    assert!(
        !fields.contains(&LoginField::PlanStore),
        "合法 store 默认只读，不进入导航"
    );
    assert!(!fields.contains(&LoginField::PlanAccount), "账号引用不展示");
    assert!(fields.contains(&LoginField::AuthActions));
    assert!(
        !fields.contains(&LoginField::BaseUrl) && !fields.contains(&LoginField::ApiKey),
        "plan mode hides API-key fields"
    );

    edit.cycle_auth_mode();
    let fields = edit.visible_fields();
    assert!(fields.contains(&LoginField::BaseUrl));
    assert!(fields.contains(&LoginField::ApiKey));
    assert!(!fields.contains(&LoginField::PlanStore));
    assert!(!fields.contains(&LoginField::AuthActions));
}
