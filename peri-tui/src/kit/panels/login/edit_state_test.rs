//! Login 草稿的纯逻辑回归：三态、四档隔离、回退归属、改名与数值边界。
//!
//! 这些用例不安装全局句柄、不落盘：`build_candidate` 只从进入表单时的基线派生。

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

fn select(edit: &mut LoginEditState, alias: &str) {
    edit.selected_profile = Profiles::ALL
        .iter()
        .position(|candidate| *candidate == alias)
        .unwrap();
}

fn set_window(edit: &mut LoginEditState, window: &str) {
    edit.selected_mut().window = window.into();
    edit.selected_mut().window_dirty = true;
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
fn new_responses_provider_binds_only_active_alias_and_keeps_other_profiles() {
    let cfg = responses_config();
    let mut edit = new_responses_edit(&cfg);
    select(&mut edit, "sonnet");
    set_window(&mut edit, "64000");

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

    let sonnet = candidate.config.profiles.get("sonnet").unwrap();
    assert_eq!(sonnet.provider, "new-responses");
    assert_eq!(sonnet.model.as_deref(), Some("new-sonnet"));
    assert_eq!(sonnet.context_window, Some(64000));

    for alias in ["fable", "opus", "haiku"] {
        assert_eq!(
            candidate.config.profiles.get(alias),
            cfg.config.profiles.get(alias),
            "{alias} 未被显式勾选时整体保持基线"
        );
    }
}

#[test]
fn editing_an_inactive_profile_does_not_switch_active_alias_or_touch_siblings() {
    let cfg = responses_config();
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    select(&mut edit, "haiku");
    set_window(&mut edit, "9999");

    let candidate = edit.build_candidate().unwrap();
    assert_eq!(candidate.config.active_alias, "sonnet");
    assert_eq!(candidate.config.profiles.haiku.context_window, Some(9999));
    assert_eq!(
        candidate.config.profiles.haiku.effort,
        cfg.config.profiles.haiku.effort
    );
    assert_eq!(
        candidate.config.profiles.haiku.max_tokens,
        cfg.config.profiles.haiku.max_tokens
    );
    for alias in ["fable", "opus", "sonnet"] {
        assert_eq!(
            candidate.config.profiles.get(alias),
            cfg.config.profiles.get(alias),
            "{alias} 不应被未选中的编辑覆盖"
        );
    }
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
fn first_provider_fallback_profiles_are_recognized_when_switching_to_responses() {
    let cfg = config(json!({"config": {
        "active_alias": "opus",
        "providers": [{"id":"first", "type":"anthropic", "apiKey":"legacy-key",
            "models":{"fable":"f", "opus":"o", "sonnet":"s", "haiku":"h"}}],
        "profiles": {
            "fable":{}, "opus":{}, "sonnet":{}, "haiku":{}
        }
    }}));
    let mut edit = LoginEditState::edit_provider(&cfg, "first", None).unwrap();
    assert_eq!(
        edit.affected_aliases(),
        vec!["fable", "opus", "sonnet", "haiku"]
    );
    edit.kind = Some(ProviderKind::OpenAiResponses);

    let error = edit.build_candidate().unwrap_err();
    assert_eq!(error.field, LoginField::ProfileWindow);
    assert_eq!(error.alias, Some("fable"), "空引用回退档位必须逐档配置");

    for alias in Profiles::ALL {
        select(&mut edit, alias);
        set_window(&mut edit, "1000");
    }
    let candidate = edit.build_candidate().unwrap();
    for alias in Profiles::ALL {
        let profile = candidate.config.profiles.get(alias).unwrap();
        assert_eq!(profile.provider, "first", "类型转为 Responses 时显式化归属");
        assert_eq!(profile.context_window, Some(1000));
        assert!(
            profile
                .model
                .as_deref()
                .is_some_and(|model| !model.is_empty())
        );
    }
}

#[test]
fn existing_explicit_model_is_not_overwritten_by_provider_mapping() {
    let mut cfg = responses_config();
    cfg.config.profiles.sonnet.model = Some("custom-model".into());
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    edit.models.sonnet = "mapped-model".into();
    edit.sync_profile_models();
    select(&mut edit, "sonnet");

    let candidate = edit.build_candidate().unwrap();
    assert_eq!(
        candidate.config.profiles.sonnet.model.as_deref(),
        Some("custom-model"),
        "未手动编辑的 profile.model 不能被映射变化覆盖"
    );
    assert_eq!(candidate.config.profiles.sonnet.context_window, Some(3000));
}

#[test]
fn mapping_change_requires_window_reconfirmation_for_mapping_only_profiles() {
    let cfg = responses_config();
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    select(&mut edit, "sonnet");
    edit.models.sonnet = "sonnet-model-next".into();
    edit.sync_profile_models();

    let error = edit.build_candidate().unwrap_err();
    assert_eq!(error.field, LoginField::ProfileWindow);
    assert_eq!(error.alias, Some("sonnet"));
    assert_eq!(error.message, i18n::tr("login-profile-reconfirm"));

    set_window(&mut edit, "3000");
    let candidate = edit.build_candidate().unwrap();
    assert_eq!(
        candidate.config.profiles.sonnet.model, None,
        "仅确认容量不强制写出显式 model override"
    );
    assert_eq!(candidate.config.profiles.sonnet.context_window, Some(3000));
}

#[test]
fn switching_profile_selector_keeps_per_profile_window_drafts() {
    let cfg = responses_config();
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    select(&mut edit, "fable");
    set_window(&mut edit, "1111");
    edit.cycle_profile(true);
    edit.cycle_profile(true);
    edit.cycle_profile(false);
    edit.cycle_profile(false);
    assert_eq!(edit.selected().alias, "fable");
    assert_eq!(edit.selected().window, "1111");
    assert!(edit.selected().window_dirty);
}

#[test]
fn responses_to_non_responses_clears_auth_but_keeps_profile_capacity() {
    let mut cfg = responses_config();
    cfg.config.providers[0].api_key.clear();
    cfg.config.providers[0].auth = Some(ProviderAuthConfig::ChatGptPlan {
        store: "/tmp/peri-login-edit-state".into(),
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
    assert_eq!(
        candidate.config.profiles.opus.context_window,
        Some(2000),
        "未编辑的 profile 容量不批量清空"
    );

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
}

#[test]
fn untouched_existing_plan_reference_survives_editing_other_fields() {
    let mut cfg = responses_config();
    cfg.config.providers[0].api_key.clear();
    cfg.config.providers[0].auth = Some(ProviderAuthConfig::ChatGptPlan {
        store: "/tmp/peri-login-edit-state".into(),
        account: "work".into(),
        subject: "subject".into(),
        client_id: "client".into(),
    });
    cfg.config.providers[0].base_url = "https://example.invalid/v1/responses".into();
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    edit.provider_id = "resp-renamed".into();
    edit.models.opus = "opus-next".into();
    edit.sync_profile_models();
    select(&mut edit, "opus");
    set_window(&mut edit, "2000");

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
}

#[test]
fn api_key_mode_is_fully_written_for_new_responses_provider() {
    let cfg = responses_config();
    let mut edit = new_responses_edit(&cfg);
    edit.base_url = "https://example.test/v1/responses".into();
    select(&mut edit, "sonnet");
    set_window(&mut edit, "128000");

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
fn provider_level_validation_runs_before_any_profile_is_bound() {
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
    edit.profiles
        .iter_mut()
        .for_each(|profile| profile.bind = false);
    edit.selected_profile = 1;
    set_window(&mut edit, "1000");

    let error = edit.build_candidate().unwrap_err();
    assert_eq!(error.field, LoginField::BaseUrl);
    assert!(error.message.contains("baseUrl"));
}

#[test]
fn context_window_parser_accepts_only_positive_ascii_integers() {
    assert_eq!(parse_window("1"), Ok(1));
    assert_eq!(parse_window(" 42 "), Ok(42));
    assert_eq!(parse_window(&u32::MAX.to_string()), Ok(u32::MAX));
    for invalid in [
        "",
        "0",
        "-1",
        "+1",
        "1.5",
        "128k",
        "1 2",
        "١٢",
        "4294967296",
        "1\n2",
    ] {
        assert_eq!(parse_window(invalid), Err(()), "{invalid:?}");
    }
}

#[test]
fn new_provider_without_baseline_providers_requires_all_fallback_profiles() {
    let mut cfg = PeriConfig::default();
    cfg.config.active_alias = "opus".into();
    let mut edit = new_responses_edit(&cfg);
    assert_eq!(edit.affected_aliases(), Profiles::ALL.to_vec());
    select(&mut edit, "opus");
    set_window(&mut edit, "1000");

    let error = edit.build_candidate().unwrap_err();
    assert_eq!(error.alias, Some("fable"));
    for alias in Profiles::ALL {
        select(&mut edit, alias);
        set_window(&mut edit, "1000");
    }
    let candidate = edit.build_candidate().unwrap();
    for alias in Profiles::ALL {
        let profile = candidate.config.profiles.get(alias).unwrap();
        if alias == "opus" {
            assert_eq!(
                profile.provider, "new-responses",
                "进入表单时的 active alias 默认显式绑定新 provider"
            );
        } else {
            assert_eq!(profile.provider, "", "首 provider 回退档不写成显式绑定");
        }
        assert_eq!(profile.context_window, Some(1000));
    }
    assert_eq!(candidate.config.active_alias, "opus");
}

#[test]
fn editing_profile_model_requires_window_reconfirmation() {
    let cfg = responses_config();
    let mut edit = LoginEditState::edit_provider(&cfg, "resp", None).unwrap();
    select(&mut edit, "sonnet");
    edit.selected_mut().model = "sonnet-next".into();
    edit.selected_mut().model_dirty = true;

    let error = edit.build_candidate().unwrap_err();
    assert_eq!(error.field, LoginField::ProfileWindow);
    assert_eq!(error.message, i18n::tr("login-profile-reconfirm"));

    set_window(&mut edit, "3000");
    let candidate = edit.build_candidate().unwrap();
    assert_eq!(
        candidate.config.profiles.sonnet.model.as_deref(),
        Some("sonnet-next")
    );
    assert_eq!(candidate.config.profiles.sonnet.context_window, Some(3000));
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
    edit.sync_profile_models();
    select(&mut edit, "opus");
    set_window(&mut edit, "2000");

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
fn context_window_serializes_only_on_profile_not_provider() {
    let cfg = responses_config();
    let mut edit = new_responses_edit(&cfg);
    select(&mut edit, "sonnet");
    set_window(&mut edit, "64000");
    let candidate = edit.build_candidate().unwrap();
    let value = serde_json::to_value(&candidate).unwrap();
    assert_eq!(
        value["config"]["profiles"]["sonnet"]["contextWindow"],
        serde_json::json!(64000)
    );
    let provider = value["config"]["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["id"] == "new-responses")
        .unwrap();
    assert!(provider.get("contextWindow").is_none());
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
