//! Model 档位草稿/内联 editor 的纯逻辑回归：原子候选、单档隔离与数值边界。

use super::*;
use crate::config::PeriConfig;
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
            {"id":"other", "type":"openai", "apiKey":"other-key",
             "models":{"fable":"other-fable", "opus":"other-opus", "sonnet":"other-sonnet", "haiku":"other-haiku"}}
        ],
        "profiles": {
            "fable":{"provider":"resp", "contextWindow":1000},
            "opus":{"provider":"resp", "contextWindow":2000},
            "sonnet":{"provider":"resp", "contextWindow":3000, "max_tokens":4096},
            "haiku":{"provider":"resp", "contextWindow":4000, "effort":"max"}
        }
    }}))
}

#[test]
fn window_editor_patches_only_the_frozen_alias() {
    let cfg = responses_config();
    let mut editor = WindowEditState::enter(&cfg, "haiku", None).expect("Responses 档位可编辑");
    assert_eq!(editor.text, "4000");
    assert_eq!(editor.model, "haiku-model");

    editor.text = "9999".into();
    editor.cursor = 4;
    let candidate = editor.validate_and_patch().unwrap();
    assert_eq!(candidate.config.active_alias, "sonnet");
    assert_eq!(candidate.config.profiles.haiku.context_window, Some(9999));
    assert_eq!(
        candidate.config.profiles.haiku.effort,
        cfg.config.profiles.haiku.effort
    );
    for alias in ["fable", "opus", "sonnet"] {
        assert_eq!(
            candidate.config.profiles.get(alias),
            cfg.config.profiles.get(alias),
            "{alias} 不得被单档 window 编辑覆盖"
        );
    }
}

#[test]
fn window_editor_rejects_non_positive_values_and_non_responses_providers() {
    let cfg = responses_config();
    let mut editor = WindowEditState::enter(&cfg, "sonnet", None).unwrap();
    for invalid in ["", "0", "128k", "-1", "1 000"] {
        editor.text = invalid.into();
        assert!(editor.validate_and_patch().is_err(), "{invalid:?}");
    }
    editor.text = "3000".into();
    assert!(editor.validate_and_patch().is_ok());

    let mut non_responses = cfg;
    non_responses.config.providers[0].provider_type = "openai".into();
    assert!(WindowEditState::enter(&non_responses, "sonnet", None).is_none());
}

#[test]
fn window_editor_only_accepts_ascii_digits() {
    let cfg = responses_config();
    let mut editor = WindowEditState::enter(&cfg, "sonnet", None).unwrap();
    editor.text.clear();
    editor.cursor = 0;
    assert!(editor.insert('1'));
    assert!(editor.insert('2'));
    assert!(!editor.insert('k'));
    assert!(editor.error.is_some());
    assert!(editor.input_blocked, "被拒绝的输入必须阻断提交");
    assert_eq!(editor.text, "12");

    editor.backspace();
    assert!(!editor.input_blocked, "实际编辑后错误生命周期结束");
    assert!(editor.error.is_none());
}

#[test]
fn profile_draft_rejected_input_blocks_until_edited() {
    let cfg = responses_config();
    let mut draft =
        ProfileDraftState::enter(&cfg, "sonnet", None, Some("resp".into()), None).unwrap();
    assert!(!draft.insert('k'));
    assert!(draft.error.is_some());
    assert!(draft.input_blocked, "被拒绝的输入必须阻断提交");

    draft.backspace();
    assert!(!draft.input_blocked, "实际编辑后错误生命周期结束");
    assert!(draft.error.is_none());
}

#[test]
fn profile_draft_saves_provider_model_and_window_atomically() {
    let cfg = responses_config();
    let mut draft = ProfileDraftState::enter(
        &cfg,
        "sonnet",
        None,
        Some("resp".into()),
        Some("sonnet-next".into()),
    )
    .unwrap();
    assert_eq!(draft.text, "", "模型变化后不能复用旧容量");
    assert!(draft.validate_and_patch().is_err(), "Responses 需要新容量");

    draft.text = "128000".into();
    draft.cursor = 6;
    let candidate = draft.validate_and_patch().unwrap();
    assert_eq!(candidate.config.active_alias, "sonnet");
    let profile = candidate.config.profiles.sonnet.clone();
    assert_eq!(profile.provider, "resp");
    assert_eq!(profile.model.as_deref(), Some("sonnet-next"));
    assert_eq!(profile.context_window, Some(128000));
    assert_eq!(profile.max_tokens, 4096, "未编辑字段保持基线");

    for alias in ["fable", "opus", "haiku"] {
        assert_eq!(
            candidate.config.profiles.get(alias),
            cfg.config.profiles.get(alias),
            "{alias} 不得被草稿覆盖"
        );
    }
}

#[test]
fn profile_draft_to_non_responses_preserves_existing_capacity() {
    let cfg = responses_config();
    let mut draft =
        ProfileDraftState::enter(&cfg, "sonnet", None, Some("other".into()), None).unwrap();
    draft.text.clear();
    let candidate = draft.validate_and_patch().unwrap();
    let profile = candidate.config.profiles.sonnet.clone();
    assert_eq!(profile.provider, "other");
    assert_eq!(profile.model.as_deref(), Some("other-sonnet"));
    assert_eq!(
        profile.context_window,
        Some(3000),
        "非 Responses 目标不批量清空既有容量"
    );
}

#[test]
fn cycling_provider_invalidates_capacity_and_restores_it_on_return() {
    let cfg = responses_config();
    let mut draft = ProfileDraftState::enter(&cfg, "sonnet", None, None, None).unwrap();
    assert_eq!(draft.provider_id, "resp");
    assert_eq!(draft.text, "3000");

    draft.cycle_provider(true);
    assert_eq!(draft.provider_id, "other");
    assert_eq!(draft.text, "", "更换 provider 后旧容量不作默认值");

    draft.cycle_provider(false);
    assert_eq!(draft.provider_id, "resp");
    assert_eq!(draft.text, "3000", "回到原 provider/model 组合时恢复原容量");
}

#[test]
fn profile_draft_requires_nonempty_model() {
    let cfg = responses_config();
    let mut draft = ProfileDraftState::enter(&cfg, "sonnet", None, None, None).unwrap();
    draft.model.clear();
    assert_eq!(
        draft.validate_and_patch().unwrap_err(),
        i18n::tr("model-panel-model-required")
    );
}

#[test]
fn shared_context_window_parser_matches_model_editor_contract() {
    use crate::kit::panels::shared::context_window::parse_context_window;
    assert_eq!(parse_context_window("1"), Some(1));
    assert_eq!(parse_context_window(" 42 "), Some(42));
    assert_eq!(parse_context_window(&u32::MAX.to_string()), Some(u32::MAX));
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
    ] {
        assert_eq!(parse_context_window(invalid), None, "{invalid:?}");
    }
}
