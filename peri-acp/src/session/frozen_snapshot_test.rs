use super::*;

fn make_frozen() -> FrozenSessionData {
    let mut section_overrides = HashMap::new();
    section_overrides.insert("persona".to_string(), Arc::<str>::from("custom persona"));
    let mut disabled_middlewares = HashSet::new();
    disabled_middlewares.insert("WebMiddleware".to_string());
    let context = peri_agent::session::FrozenContext {
        system_prompt: Arc::from("system-v1"),
        claude_md: Arc::from("claude-v1"),
        skill_summary: Arc::from("skills-v1"),
        date: Arc::from("2026-09-01"),
        language: Some(Arc::from("zh-CN")),
        meta_harness: peri_acp_types::meta_harness::MetaHarnessState {
            section_overrides,
            disabled_middlewares,
            built_in_subagents_enabled: false,
        },
        // H3：冻结运行环境随快照持久化。
        runtime_env: Some(peri_acp_types::frozen::FrozenRuntimeEnv {
            platform: "macos".to_string(),
            os_version: "macOS 26.5.1".to_string(),
            is_git_repo: true,
        }),
    };
    FrozenSessionData::from_frozen_parts(context, Some(Arc::from("local-v1")))
}

#[test]
fn test_frozen_snapshot_roundtrip_preserves_all_fields() {
    // Arrange
    let original = make_frozen();
    // Act
    let raw = encode_frozen_snapshot(&original).unwrap();
    let restored = decode_frozen_snapshot(&raw).unwrap();
    // Assert
    assert_eq!(restored.system_prompt(), original.system_prompt());
    assert_eq!(restored.claude_md(), original.claude_md());
    assert_eq!(restored.claude_local_md(), original.claude_local_md());
    assert_eq!(restored.skill_summary(), original.skill_summary());
    assert_eq!(restored.date(), original.date());
    assert_eq!(restored.language(), original.language());
    assert_eq!(restored.meta_harness(), original.meta_harness());
    // H3：运行环境快照逐字段往返（platform / os_version / is_git_repo）。
    assert_eq!(restored.runtime_env(), original.runtime_env());
}

/// H3 旧数据策略：V1 旧 blob（无 `runtime_env` 键）仍可解码，结构化环境值
/// 标记 unavailable（`None`），不得重探本地值冒充。
#[test]
fn test_frozen_snapshot_v1_without_runtime_env_decodes_as_unavailable() {
    let raw = r#"{"version":1,"data":{
        "system_prompt":"system-v1",
        "claude_md":"claude-v1",
        "claude_local_md":null,
        "skill_summary":"skills-v1",
        "date":"2026-09-01",
        "language":null,
        "meta_harness":{"section_overrides":{},"disabled_middlewares":[],"built_in_subagents_enabled":true}
    }}"#;
    let restored = decode_frozen_snapshot(raw).expect("旧 V1 blob 必须可读");
    assert_eq!(
        restored.runtime_env(),
        None,
        "缺少结构化环境值 = unavailable"
    );
    assert_eq!(restored.date(), "2026-09-01");
    assert_eq!(restored.system_prompt(), "system-v1");
}

#[test]
fn test_frozen_snapshot_future_version_fails_closed() {
    // Arrange
    let raw = r#"{"version":2,"data":{}}"#;
    // Act
    let error = decode_frozen_snapshot(raw)
        .err()
        .expect("future versions must fail closed");
    // Assert
    assert!(matches!(error, FrozenSnapshotError::UnsupportedVersion(2)));
}

#[test]
fn test_frozen_snapshot_missing_version_is_invalid() {
    // Arrange
    let raw = r#"{"data":{}}"#;
    // Act
    let error = decode_frozen_snapshot(raw)
        .err()
        .expect("missing versions must fail closed");
    // Assert
    assert!(matches!(error, FrozenSnapshotError::Invalid(_)));
    assert!(error.to_string().contains("missing unsigned version"));
}
