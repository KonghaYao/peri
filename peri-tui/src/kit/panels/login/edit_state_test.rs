//! Login 草稿的纯逻辑回归：provider 候选、改名引用与档位隔离。
//!
//! 这些用例不安装全局句柄、不落盘：`build_candidate` 只从进入表单时的基线派生。
//! Login 不再编辑档位；新建 provider 必须不写任何 profile，编辑已有 provider 必须
//! 保持既有绑定与参数不变。

use super::*;
use serde_json::json;

fn config(value: serde_json::Value) -> PeriConfig {
    serde_json::from_value(value).unwrap()
}

fn responses_config() -> PeriConfig {
    config(json!({"config": {
        "active_alias": "sonnet",
        "providers": [
            {"id":"resp", "type":"openai_responses", "apiKey":"fixture-key",
             "models":{"fable":"fable-model", "opus":"opus-model", "sonnet":"sonnet-model", "haiku":"haiku-model"}},
            {"id":"other", "type":"anthropic", "apiKey":"other-key",
             "models":{"fable":"other-fable", "opus":"other-opus", "sonnet":"other-sonnet", "haiku":"other-haiku"}}
        ],
        "profiles": {
            "fable":{"provider":"resp", "contextWindow":1000},
            "opus":{"provider":"resp", "contextWindow":2000},
            "sonnet":{"provider":"resp", "contextWindow":3000},
            "haiku":{"provider":"resp", "contextWindow":4000}
        }
    }}))
}

fn new_responses_edit(cfg: &PeriConfig) -> LoginEditState {
    let mut edit = LoginEditState::new_provider(cfg, None);
    edit.kind = Some(ProviderKind::OpenAiResponses);
    edit.provider_id = "new-responses".into();
    edit.api_key = "new-key".into();
    edit.models = ProviderModels {
        fable: "new-fable".into(),
        opus: "new-opus".into(),
        sonnet: "new-sonnet".into(),
        haiku: "new-haiku".into(),
    };
    edit
}

fn assert_profiles_unchanged(candidate: &PeriConfig, baseline: &PeriConfig) {
    for alias in Profiles::ALL {
        assert_eq!(
            candidate.config.profiles.get(alias),
            baseline.config.profiles.get(alias),
            "{alias} must stay byte-for-byte unchanged"
        );
    }
}

#[test]
fn new_provider_cycles_three_types_and_edit_requires_explicit_change() {
    let cfg = PeriConfig::default();
    let mut new = LoginEditState::new_provider(&cfg, None);
    assert_eq!(new.kind, Some(ProviderKind::Anthropic));
    new.cycle_type(true);
    assert_eq!(new.kind, Some(ProviderKind::OpenAi));
    new.cycle_type(true);
    assert_eq!(new.kind, Some(ProviderKind::OpenAiResponses));
    new.cycle_type(false);
    assert_eq!(new.kind, Some(ProviderKind::OpenAi));

    let cfg = responses_config();
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    assert_eq!(edit.kind, Some(ProviderKind::OpenAiResponses));
    edit.cycle_type(true);
    assert_eq!(
        edit.kind,
        Some(ProviderKind::OpenAiResponses),
        "已有 provider 不能靠方向键/空格误改类型"
    );

    edit.begin_type_change();
    edit.cycle_type_change_candidate(true);
    assert_eq!(
        edit.type_change_candidate,
        Some(ProviderKind::Anthropic),
        "显式类型子视图只改候选"
    );
    edit.cancel_type_change();
    assert_eq!(edit.kind, Some(ProviderKind::OpenAiResponses));

    edit.begin_type_change();
    edit.cycle_type_change_candidate(true);
    edit.confirm_type_change();
    assert_eq!(edit.kind, Some(ProviderKind::Anthropic));
}

#[test]
fn new_responses_provider_never_binds_or_rewrites_profiles() {
    let cfg = responses_config();
    let edit = new_responses_edit(&cfg);
    let candidate = edit.build_candidate().unwrap();

    assert_eq!(candidate.config.active_alias, "sonnet");
    let new_provider = candidate
        .config
        .providers
        .iter()
        .find(|provider| provider.id == "new-responses")
        .expect("new provider must be appended");
    assert_eq!(new_provider.provider_type, "openai_responses");
    assert_eq!(new_provider.auth, Some(ProviderAuthConfig::ApiKey));
    assert_eq!(new_provider.api_key, "new-key");

    assert_profiles_unchanged(&candidate, &cfg);
    let mut expected = cfg.clone();
    expected.config.providers.push(new_provider.clone());
    assert_eq!(
        candidate, expected,
        "candidate must contain the provider addition only"
    );
    let value = serde_json::to_value(&candidate).unwrap();
    assert_eq!(
        value["config"]["profiles"],
        serde_json::to_value(&cfg.config.profiles).unwrap(),
        "disk projection must contain provider changes only"
    );
}

#[test]
fn new_provider_without_any_baseline_provider_is_not_blocked_by_profiles() {
    let mut cfg = PeriConfig::default();
    cfg.config.active_alias = "opus".into();
    let edit = new_responses_edit(&cfg);

    let candidate = edit
        .build_candidate()
        .expect("新建 Responses provider 不得因档位未绑定/缺少 contextWindow 被阻断");
    assert_profiles_unchanged(&candidate, &cfg);
    assert_eq!(candidate.config.profiles, cfg.config.profiles);
    assert_eq!(candidate.config.active_alias, "opus");
}

#[test]
fn unbound_profiles_keep_first_provider_fallback_after_adding_provider() {
    let cfg = config(json!({"config": {
        "active_alias": "opus",
        "providers": [{"id":"legacy", "type":"anthropic", "apiKey":"legacy-key",
            "models":{"fable":"legacy-fable", "opus":"legacy-opus", "sonnet":"legacy-sonnet", "haiku":"legacy-haiku"}}],
        "profiles": {"opus":{}}
    }}));
    let edit = new_responses_edit(&cfg);
    let candidate = edit.build_candidate().unwrap();

    assert_profiles_unchanged(&candidate, &cfg);
    assert_eq!(
        candidate.config.profiles.opus.provider, "",
        "empty reference must stay empty and keep using first-provider fallback"
    );
    let resolved = peri_config::provider::try_resolve_for_alias(&candidate, "opus")
        .expect("fallback resolution must keep working")
        .expect("legacy provider must remain resolvable");
    let peri_config::provider::ResolvedProvider::Anthropic { model, .. } = resolved else {
        panic!("fallback must still select the first Anthropic provider");
    };
    assert_eq!(model, "legacy-opus");
}

#[test]
fn editing_existing_provider_preserves_every_profile_field() {
    let mut cfg = responses_config();
    cfg.config.profiles.sonnet.model = Some("custom-model".into());
    cfg.config.profiles.sonnet.effort = "low".into();
    cfg.config.profiles.sonnet.max_tokens = 777;
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    edit.models.sonnet = "mapped-next".into();

    let candidate = edit.build_candidate().unwrap();
    assert_profiles_unchanged(&candidate, &cfg);
    let provider = candidate
        .config
        .providers
        .iter()
        .find(|provider| provider.id == "resp")
        .unwrap();
    assert_eq!(provider.models.sonnet, "mapped-next");
}

#[test]
fn renaming_provider_updates_every_explicit_reference_and_keeps_fallback_empty() {
    let cfg = config(json!({"config": {
        "active_alias": "opus",
        "providers": [
            {"id":"old", "type":"openai_responses", "apiKey":"key",
             "models":{"fable":"f", "opus":"o", "sonnet":"s", "haiku":"h"}},
            {"id":"other", "type":"anthropic", "apiKey":"other"}
        ],
        "profiles": {
            "fable":{"provider":"old", "contextWindow":10},
            "opus":{"contextWindow":20},
            "sonnet":{"provider":"old", "contextWindow":30},
            "haiku":{"provider":"other"}
        }
    }}));
    let mut edit = LoginEditState::edit_provider(&cfg, "old", None).unwrap();
    edit.provider_id = "renamed".into();

    let candidate = edit.build_candidate().unwrap();
    assert_eq!(
        candidate.config.profiles.fable.provider, "renamed",
        "显式引用必须跟随改名"
    );
    assert_eq!(
        candidate.config.profiles.sonnet.provider, "renamed",
        "显式引用必须跟随改名"
    );
    assert_eq!(
        candidate.config.profiles.opus.provider, "",
        "空引用仍走首 provider 回退，改名不写死"
    );
    assert_eq!(candidate.config.profiles.opus.context_window, Some(20));
    assert_eq!(candidate.config.profiles.haiku.provider, "other");
    assert_eq!(candidate.config.active_alias, "opus");
}

#[test]
fn changing_type_to_responses_keeps_existing_bindings_and_windows() {
    let cfg = config(json!({"config": {
        "active_alias": "opus",
        "providers": [{"id":"first", "type":"anthropic", "apiKey":"legacy-key",
            "models":{"fable":"f", "opus":"o", "sonnet":"s", "haiku":"h"}}],
        "profiles": {
            "fable":{"provider":"first", "contextWindow":1000},
            "opus":{"provider":"first", "contextWindow":2000},
            "sonnet":{"provider":"first", "contextWindow":3000},
            "haiku":{"provider":"first", "contextWindow":4000}
        }
    }}));
    let mut edit = LoginEditState::edit_provider(&cfg, "first", None).unwrap();
    assert_eq!(edit.bound_aliases(), Profiles::ALL.to_vec());
    edit.kind = Some(ProviderKind::OpenAiResponses);
    edit.api_key = "new-key".into();

    let candidate = edit.build_candidate().unwrap();
    assert_eq!(
        candidate.config.providers[0].provider_type,
        "openai_responses"
    );
    assert_profiles_unchanged(&candidate, &cfg);
}

#[test]
fn switching_to_responses_without_capacity_points_to_model_panel() {
    let cfg = config(json!({"config": {
        "active_alias": "opus",
        "providers": [{"id":"first", "type":"anthropic", "apiKey":"legacy-key",
            "models":{"fable":"f", "opus":"o", "sonnet":"s", "haiku":"h"}}],
        "profiles": {"fable":{}, "opus":{}, "sonnet":{}, "haiku":{}}
    }}));
    let mut edit = LoginEditState::edit_provider(&cfg, "first", None).unwrap();
    edit.kind = Some(ProviderKind::OpenAiResponses);
    edit.api_key = "new-key".into();

    let error = edit.build_candidate().unwrap_err();
    assert_eq!(error.field, LoginField::ProviderType);
    assert_eq!(
        error.message,
        i18n::tr_args(
            "login-error-profile-window-missing",
            &[(
                "alias".to_string(),
                fluent_bundle::FluentValue::from("fable")
            )],
        )
    );
    assert!(
        error.message.contains("Model panel"),
        "error must point users at the Model panel: {}",
        error.message
    );
}

#[test]
fn newly_unresolved_binding_blocks_save_with_precise_model_error() {
    let cfg = responses_config();
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    edit.models.sonnet.clear();

    let error = edit.build_candidate().unwrap_err();
    assert_eq!(
        error.field,
        LoginField::SonnetModel,
        "模型缺失应指向 Login 的 Sonnet 默认模型字段"
    );
    assert_eq!(
        error.message,
        i18n::tr_args(
            "login-error-profile-model-missing",
            &[(
                "alias".to_string(),
                fluent_bundle::FluentValue::from("sonnet")
            )],
        )
    );
}

#[test]
fn existing_explicit_profile_model_wins_without_rewrite() {
    let mut cfg = responses_config();
    cfg.config.profiles.sonnet.model = Some("explicit-model".into());
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    edit.models.sonnet = "mapped-model".into();

    let (candidate, unresolved) = edit.build_candidate_with_warnings().unwrap();
    assert!(unresolved.is_empty());
    assert_eq!(
        candidate.config.profiles.sonnet.model.as_deref(),
        Some("explicit-model"),
        "显式 profile.model 优先，Login 不得覆盖"
    );
    assert_eq!(
        candidate.config.providers[0].models.sonnet, "mapped-model",
        "Provider 默认模型仍要落盘"
    );
}

#[test]
fn fable_empty_follows_opus_mapping_without_rewrite() {
    let cfg = responses_config();
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    edit.models.fable.clear();

    let (candidate, unresolved) = edit.build_candidate_with_warnings().unwrap();
    assert!(unresolved.is_empty(), "Fable 留空跟随 Opus，不是坏档位");
    assert_eq!(candidate.config.providers[0].models.fable, "");
    assert_eq!(candidate.config.providers[0].models.opus, "opus-model");
    assert_profiles_unchanged(&candidate, &cfg);
}

#[test]
fn editing_mapping_does_not_rewrite_profile_models_or_windows() {
    let mut cfg = responses_config();
    cfg.config.profiles.sonnet.model = Some("custom-model".into());
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    edit.models.sonnet = "mapped-model".into();

    let candidate = edit.build_candidate().unwrap();
    assert_profiles_unchanged(&candidate, &cfg);
}

#[test]
fn pre_existing_unresolved_binding_does_not_block_save_without_auto_filling() {
    let mut cfg = responses_config();
    cfg.config.profiles.sonnet.context_window = None;
    let edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();

    let (candidate, unresolved) = edit.build_candidate_with_warnings().unwrap();
    assert_eq!(
        unresolved,
        vec!["sonnet"],
        "原本就不可解析的档位只给非阻断警告"
    );
    assert_eq!(
        candidate.config.profiles.sonnet.context_window, None,
        "Login 不得静默补 contextWindow"
    );
    assert_profiles_unchanged(&candidate, &cfg);
}

#[test]
fn responses_to_non_responses_clears_auth_but_keeps_profiles() {
    let mut cfg = responses_config();
    cfg.config.providers[0].api_key.clear();
    cfg.config.providers[0].auth = Some(ProviderAuthConfig::ChatGptPlan {
        store: std::env::temp_dir().join("peri-login-edit-state"),
        account: "work".into(),
        subject: "subject".into(),
        client_id: "client".into(),
    });
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    assert!(edit.plan_reference().is_some());
    edit.kind = Some(ProviderKind::Anthropic);

    let candidate = edit.build_candidate().unwrap();
    let provider = candidate
        .config
        .providers
        .iter()
        .find(|provider| provider.id == "resp")
        .unwrap();
    assert_eq!(provider.auth, None, "最终 candidate 清除 Responses auth");
    assert_profiles_unchanged(&candidate, &cfg);

    edit.kind = Some(ProviderKind::OpenAiResponses);
    let restored = edit.build_candidate().unwrap();
    let provider = restored
        .config
        .providers
        .iter()
        .find(|provider| provider.id == "resp")
        .unwrap();
    assert!(matches!(
        provider.auth,
        Some(ProviderAuthConfig::ChatGptPlan { .. })
    ));
    assert!(provider.api_key.is_empty());
    assert_profiles_unchanged(&restored, &cfg);
}

#[test]
fn untouched_existing_plan_reference_survives_editing_other_fields() {
    let mut cfg = responses_config();
    cfg.config.providers[0].api_key.clear();
    cfg.config.providers[0].auth = Some(ProviderAuthConfig::ChatGptPlan {
        store: std::env::temp_dir().join("peri-login-edit-state"),
        account: "work".into(),
        subject: "subject".into(),
        client_id: "client".into(),
    });
    cfg.config.providers[0].base_url = "https://example.invalid/v1/responses".into();
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    edit.provider_id = "resp-renamed".into();
    edit.models.opus = "opus-next".into();

    let candidate = edit.build_candidate().unwrap();
    let provider = candidate
        .config
        .providers
        .iter()
        .find(|provider| provider.id == "resp-renamed")
        .unwrap();
    assert_eq!(
        provider.auth, cfg.config.providers[0].auth,
        "未触碰的 plan auth 不得被清空"
    );
    assert!(provider.api_key.is_empty());
    assert!(
        provider.base_url.is_empty(),
        "plan 引用只允许 core 官方 endpoint（空值走默认）"
    );
    assert_profiles_unchanged_after_rename(&candidate, &cfg, "resp", "resp-renamed");
}

fn assert_profiles_unchanged_after_rename(
    candidate: &PeriConfig,
    baseline: &PeriConfig,
    old: &str,
    new: &str,
) {
    for alias in Profiles::ALL {
        let mut expected = baseline.config.profiles.get(alias).unwrap().clone();
        if expected.provider == old {
            expected.provider = new.to_string();
        }
        assert_eq!(
            candidate.config.profiles.get(alias),
            Some(&expected),
            "{alias} keeps parameters with only the reference renamed"
        );
    }
}

#[test]
fn api_key_mode_is_fully_written_for_new_responses_provider() {
    let cfg = responses_config();
    let mut edit = new_responses_edit(&cfg);
    edit.base_url = "https://example.test/v1/responses".into();

    let candidate = edit.build_candidate().unwrap();
    let provider = candidate
        .config
        .providers
        .iter()
        .find(|provider| provider.id == "new-responses")
        .unwrap();
    assert_eq!(provider.api_key, "new-key");
    assert_eq!(provider.base_url, "https://example.test/v1/responses");
    assert_eq!(provider.auth, Some(ProviderAuthConfig::ApiKey));
}

#[test]
fn unknown_saved_type_blocks_save_instead_of_defaulting_to_anthropic() {
    let mut cfg = responses_config();
    cfg.config.providers[0].provider_type = "future_responses".into();
    let edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    assert_eq!(edit.kind, None);
    let error = edit.build_candidate().unwrap_err();
    assert_eq!(error.field, LoginField::ProviderType);
    assert_eq!(error.message, i18n::tr("login-error-unknown-type"));
}

#[test]
fn provider_level_validation_still_runs_for_new_responses_provider() {
    let cfg = config(json!({"config": {
        "active_alias": "opus",
        "providers": [{"id":"base", "type":"anthropic", "apiKey":"key"}],
        "profiles": {"opus":{"provider":"base"}}
    }}));
    let mut edit = LoginEditState::new_provider(&cfg, None);
    edit.kind = Some(ProviderKind::OpenAiResponses);
    edit.provider_id = "new".into();
    edit.api_key = "key".into();
    edit.base_url = "not a url".into();

    let error = edit.build_candidate().unwrap_err();
    assert_eq!(error.field, LoginField::BaseUrl);
    assert!(error.message.contains("baseUrl"));
}

#[test]
fn provider_name_and_unknown_extras_survive_login_edits() {
    let mut cfg = responses_config();
    cfg.config.providers[0].name = Some("Display Name".into());
    cfg.config.providers[0]
        .extra
        .insert("customFlag".into(), serde_json::json!(true));
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    edit.provider_id = "resp-renamed".into();
    edit.models.opus = "opus-next".into();

    let candidate = edit.build_candidate().unwrap();
    let provider = candidate
        .config
        .providers
        .iter()
        .find(|provider| provider.id == "resp-renamed")
        .unwrap();
    assert_eq!(provider.name.as_deref(), Some("Display Name"));
    assert_eq!(
        provider.extra.get("customFlag"),
        Some(&serde_json::json!(true))
    );
    assert_eq!(provider.models.opus, "opus-next");
}

#[test]
fn existing_api_key_modes_preserve_their_auth_shape() {
    let cfg = responses_config();
    assert_eq!(cfg.config.providers[0].auth, None);
    let edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    let candidate = edit.build_candidate().unwrap();
    assert_eq!(
        candidate.config.providers[0].auth, None,
        "未触碰的 None auth 保持 None"
    );

    let mut explicit = cfg;
    explicit.config.providers[0].auth = Some(ProviderAuthConfig::ApiKey);
    let edit = LoginEditState::edit_provider(&explicit, "resp", None).unwrap();
    let candidate = edit.build_candidate().unwrap();
    assert_eq!(
        candidate.config.providers[0].auth,
        Some(ProviderAuthConfig::ApiKey)
    );
}

// ── 新建 plan Provider 的默认模型预填 ─────────────────────────────────────────

fn plan_provider(id: &str, models: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "type": "openai_responses",
        "apiKey": "",
        "auth": {
            "kind": "chatgpt_plan",
            "store": "/tmp/peri-login-edit-state",
            "account": "work",
            "subject": "subject",
            "clientId": "client"
        },
        "models": models
    })
}

#[test]
fn new_plan_provider_prefills_models_from_saved_plan_provider() {
    let cfg = config(json!({"config": {
        "providers": [
            plan_provider("saved", json!({
                "fable": "plan-fable", "opus": "plan-opus",
                "sonnet": "plan-sonnet", "haiku": "plan-haiku"
            })),
            {"id":"plain", "type":"openai_responses", "apiKey":"key",
             "models":{"fable":"other-fable", "opus":"other-opus",
                       "sonnet":"other-sonnet", "haiku":"other-haiku"}}
        ]
    }}));
    let mut edit = LoginEditState::new_provider(&cfg, None);
    edit.kind = Some(ProviderKind::OpenAiResponses);
    edit.cycle_auth_mode();

    assert_eq!(edit.models.fable, "plan-fable");
    assert_eq!(edit.models.opus, "plan-opus");
    assert_eq!(edit.models.sonnet, "plan-sonnet");
    assert_eq!(edit.models.haiku, "plan-haiku");
    assert!(!edit.model_defaults_hint_visible());
}

#[test]
fn new_plan_provider_falls_back_to_other_responses_mapping() {
    let cfg = config(json!({"config": {
        "providers": [
            {"id":"plain", "type":"openai_responses", "apiKey":"key",
             "models":{"fable":"plain-fable", "opus":"plain-opus",
                       "sonnet":"plain-sonnet", "haiku":"plain-haiku"}}
        ]
    }}));
    let mut edit = LoginEditState::new_provider(&cfg, None);
    edit.kind = Some(ProviderKind::OpenAiResponses);
    edit.cycle_auth_mode();

    assert_eq!(edit.models.opus, "plain-opus");
    assert_eq!(edit.models.haiku, "plain-haiku");
}

#[test]
fn manual_default_model_survives_auto_fill() {
    let cfg = config(json!({"config": {
        "providers": [plan_provider("saved", json!({
            "fable": "plan-fable", "opus": "plan-opus",
            "sonnet": "plan-sonnet", "haiku": "plan-haiku"
        }))]
    }}));
    let mut edit = LoginEditState::new_provider(&cfg, None);
    edit.kind = Some(ProviderKind::OpenAiResponses);
    edit.models.opus = "manual-opus".into();
    edit.cycle_auth_mode();

    assert_eq!(edit.models.opus, "manual-opus", "手动输入优先");
    assert_eq!(edit.models.sonnet, "plan-sonnet", "仅空字段被预填");

    // 再次切换认证方式会重复触发预填，但已填字段（含手动值）不得被覆盖。
    edit.cycle_auth_mode();
    edit.cycle_auth_mode();
    assert_eq!(edit.models.opus, "manual-opus");
    assert_eq!(edit.models.sonnet, "plan-sonnet");
}

#[test]
fn new_plan_provider_without_source_keeps_empty_models_and_shows_hint() {
    let cfg = PeriConfig::default();
    let mut edit = LoginEditState::new_provider(&cfg, None);
    edit.kind = Some(ProviderKind::OpenAiResponses);
    edit.cycle_auth_mode();

    assert_eq!(edit.models, ProviderModels::default());
    assert!(
        edit.model_defaults_hint_visible(),
        "没有可靠来源时不得伪造模型 ID，必须给出可操作提示"
    );
}

#[test]
fn existing_plan_provider_never_auto_fills_empty_models() {
    let cfg = config(json!({"config": {
        "providers": [
            plan_provider("saved", json!({
                "fable": "plan-fable", "opus": "plan-opus",
                "sonnet": "plan-sonnet", "haiku": "plan-haiku"
            })),
            plan_provider("target", json!({}))
        ]
    }}));
    let mut edit = LoginEditState::edit_provider(&cfg, "target", None).unwrap();
    edit.ensure_plan_defaults(Some(PathBuf::from("/tmp/peri-login-edit-state")));

    assert_eq!(
        edit.models,
        ProviderModels::default(),
        "编辑已有 provider 不得静默改写其默认模型"
    );
    assert!(!edit.model_defaults_hint_visible());
}
