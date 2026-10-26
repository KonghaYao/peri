//! Model 上下文窗口内联 editor 的纯逻辑回归：单档隔离与数值边界。

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
