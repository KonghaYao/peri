//! Responses 未完成绑定与保存边界（截图回归）。
//!
//! 从 `settings_test.rs` 拆出的独立测试模块，保持覆盖不变。

use super::super::ConfigSource;
use super::support::{make_source, revision, write_settings};

fn write_responses_fixture(tmp: &tempfile::TempDir) {
    let providers = r#"[
        {"id":"cursor","type":"openai","apiKey":"cursor-key",
         "models":{"fable":"grok-4.6","opus":"grok-4.6","sonnet":"composer-2.5","haiku":"composer-2.5"}},
        {"id":"codex","type":"openai_responses","apiKey":"codex-key",
         "models":{"fable":"gpt-6-astra","opus":"gpt-6-astra","sonnet":"gpt-6.1-sol","haiku":"gpt-6-luna"}}
    ]"#;
    write_settings(
        &tmp.path().join("global"),
        &format!(
            r#"{{"config":{{"active_alias":"opus","providers":{providers},
            "profiles":{{
                "opus":{{"provider":"cursor","model":"grok-4.6","effort":"high"}},
                "sonnet":{{"provider":"codex"}}
            }}}}}}"#
        ),
    );
    write_settings(
        &tmp.path().join("ws"),
        &format!(
            r#"{{"config":{{"active_alias":"opus","providers":{providers},
            "profiles":{{
                "fable":{{"provider":"cursor"}},
                "opus":{{"provider":"cursor","model":"grok-4.6","effort":"high"}}
            }}}}}}"#
        ),
    );
}

/// 截图场景：生效档位 `opus · cursor`，辅助档位 `sonnet · codex` 缺 contextWindow。
/// 保存生效档位的普通字段必须成功，且不得补写/修改辅助绑定。
#[test]
fn workspace_save_tolerates_auxiliary_responses_capacity_gap() {
    let tmp = tempfile::tempdir().unwrap();
    write_responses_fixture(&tmp);
    let source = make_source(&tmp, Some("ws"));
    let mut candidate = source.loaded_merged();
    candidate.config.profiles.opus.effort = "max".into();

    let accepted = source
        .save(revision(&source), &candidate)
        .expect("辅助档位缺 contextWindow 不得阻断生效档位保存");
    assert_eq!(accepted.settings().config.profiles.opus.effort, "max");
    assert_eq!(
        accepted.settings().config.profiles.sonnet.context_window,
        None
    );
    assert_eq!(accepted.settings().config.profiles.sonnet.provider, "codex");
    assert!(
        matches!(
            accepted.provider(),
            Some(crate::provider::ResolvedProvider::OpenAi { .. })
        ),
        "生效档位仍按 cursor 解析"
    );
}

/// 截图根因：Model 面板把生效档位切到缺 contextWindow 的 Responses provider 时，
/// 配置文档仍应保存（未完成绑定 → 无生效 provider），而不是整份保存失败。
#[test]
fn workspace_save_persists_unfinished_effective_responses_binding() {
    let tmp = tempfile::tempdir().unwrap();
    write_responses_fixture(&tmp);
    let source = make_source(&tmp, Some("ws"));
    let mut candidate = source.loaded_merged();
    candidate.config.profiles.opus.provider = "codex".into();
    candidate.config.profiles.opus.model = Some("gpt-6-astra".into());

    let accepted = source
        .save(revision(&source), &candidate)
        .expect("未完成绑定必须可保存");
    assert_eq!(accepted.settings().config.profiles.opus.provider, "codex");
    assert_eq!(
        accepted.settings().config.profiles.opus.context_window,
        None
    );
    assert!(
        accepted.provider().is_none(),
        "未完成绑定只让 provider 不可用，不得伪造容量"
    );

    let reloaded = ConfigSource::load_at(source.cwd(), source.global_path().to_owned()).unwrap();
    assert_eq!(
        reloaded.loaded_merged().config.profiles.opus.provider,
        "codex",
        "绑定必须已落盘"
    );
}

/// 保存边界不因此放松 provider 级校验：缺 API key 的 Responses 绑定仍拒绝写入。
#[test]
fn workspace_save_rejects_provider_level_error_on_responses_binding() {
    let tmp = tempfile::tempdir().unwrap();
    write_responses_fixture(&tmp);
    let source = make_source(&tmp, Some("ws"));
    let workspace_path = source.workspace_path().unwrap().to_owned();
    let before = std::fs::read_to_string(&workspace_path).unwrap();
    let mut candidate = source.loaded_merged();
    candidate.config.profiles.opus.provider = "codex".into();
    candidate.config.profiles.opus.model = Some("gpt-6-astra".into());
    candidate.config.providers[1].api_key.clear();

    assert!(
        source.save(revision(&source), &candidate).is_err(),
        "缺 API key 是 provider 级硬错误"
    );
    assert_eq!(
        std::fs::read_to_string(&workspace_path).unwrap(),
        before,
        "拒绝保存不得写盘"
    );
}

/// 加载边界同一规则：磁盘上生效档位是未完成的 Responses 绑定时，配置源仍可装载，
/// 只是不产生生效 provider（否则 TUI 无法启动到可修复状态）。
#[test]
fn load_tolerates_unfinished_effective_responses_binding() {
    let tmp = tempfile::tempdir().unwrap();
    write_responses_fixture(&tmp);
    let ws_path = tmp.path().join("ws").join(".peri").join("settings.json");
    let content = std::fs::read_to_string(&ws_path).unwrap().replace(
        r#""opus":{"provider":"cursor","model":"grok-4.6","effort":"high"}"#,
        r#""opus":{"provider":"codex","model":"gpt-6-astra","effort":"high"}"#,
    );
    std::fs::write(&ws_path, content).unwrap();

    let source = ConfigSource::load_at(
        &tmp.path().join("ws"),
        tmp.path()
            .join("global")
            .join(".peri")
            .join("settings.json"),
    )
    .expect("未完成绑定不得让配置源装载失败");
    assert!(source.snapshot().unwrap().provider().is_none());
    assert_eq!(
        source.loaded_merged().config.profiles.opus.provider,
        "codex"
    );
}
