use crate::session::frozen_snapshot::FrozenSnapshotError;
use peri_agent::session::exec::executor::FrozenSessionData;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

/// 快照往返测试用的合成 flag id（与注册表解耦）。
const SNAPSHOT_FLAG: &str = "snapshot-flag";

fn make_frozen() -> FrozenSessionData {
    let mut section_overrides = HashMap::new();
    section_overrides.insert("persona".to_string(), Arc::<str>::from("custom persona"));
    let mut disabled_middlewares = HashSet::new();
    disabled_middlewares.insert("web".to_string());
    let context = peri_agent::session::FrozenContext {
        system_prompt: Arc::from("system-v1"),
        external_instructions: None,
        environment_prompt: Some(Arc::from("Working directory: /frozen")),
        legacy_embedded_instructions: false,
        legacy_embedded_environment: false,
        claude_md: Arc::from("claude-v1"),
        skill_summary: Arc::from("skills-v1"),
        date: Arc::from("2026-09-01"),
        language: Some(Arc::from("zh-CN")),
        meta_harness: peri_acp_types::meta_harness::MetaHarnessState {
            section_overrides,
            disabled_middlewares,
            built_in_subagents_enabled: false,
        },
        // beta flag 冻结值：随快照持久化，恢复后不重读配置（用合成 id 锁往返语义）。
        beta_flags: peri_acp_types::beta_flags::BetaFlags::from_values([(
            SNAPSHOT_FLAG.to_string(),
            peri_acp_types::beta_flags::BetaFlagValue {
                enabled: true,
                origin: peri_acp_types::beta_flags::BetaFlagOrigin::Workspace,
            },
        )]),
        builtin_assembly_mode: peri_acp_types::builtin_mcp::BuiltinAssemblyMode::LegacyInstances,
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
    let raw = crate::session::frozen_snapshot::encode_frozen_snapshot(&original).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&raw).unwrap()["version"],
        4
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(&raw).unwrap()["data"]
            .get("external_instructions")
            .is_some()
    );
    let restored = crate::session::frozen_snapshot::decode_frozen_snapshot(&raw).unwrap();
    // Assert
    assert_eq!(restored.system_prompt(), original.system_prompt());
    assert_eq!(restored.claude_md(), original.claude_md());
    assert_eq!(restored.claude_local_md(), original.claude_local_md());
    assert_eq!(restored.skill_summary(), original.skill_summary());
    assert_eq!(restored.date(), original.date());
    assert_eq!(restored.language(), original.language());
    assert_eq!(restored.meta_harness(), original.meta_harness());
    assert_eq!(
        restored.v2_frozen().builtin_assembly_mode,
        original.v2_frozen().builtin_assembly_mode
    );
    // beta flag 冻结值逐条往返（含来源层）：恢复路径不按当前配置重建。
    assert_eq!(
        restored.v2_frozen().beta_flags,
        original.v2_frozen().beta_flags
    );
    assert!(restored.v2_frozen().beta_flags.is_enabled(SNAPSHOT_FLAG));
    // H3：运行环境快照逐字段往返（platform / os_version / is_git_repo）。
    assert_eq!(restored.runtime_env(), original.runtime_env());
    assert_eq!(
        restored.v2_frozen().environment_prompt,
        original.v2_frozen().environment_prompt
    );
}

/// [回归测试] 客户端扩展必须作为 V2 独立字段逐字往返，不混入内部 prompt。
#[test]
fn test_frozen_snapshot_v2_external_instructions_roundtrip_and_required_field() {
    let mut frozen = make_frozen();
    let mut context = frozen.v2_frozen().clone();
    context.external_instructions = Some(Arc::from("  {{date}}\r\n外部指令  "));
    frozen = FrozenSessionData::from_frozen_parts(context, None);
    let raw = crate::session::frozen_snapshot::encode_frozen_snapshot(&frozen).unwrap();
    let restored = crate::session::frozen_snapshot::decode_frozen_snapshot(&raw).unwrap();
    assert_eq!(restored.system_prompt(), "system-v1");
    assert_eq!(
        restored.v2_frozen().external_instructions.as_deref(),
        Some("  {{date}}\r\n外部指令  ")
    );
    let mut damaged: serde_json::Value = serde_json::from_str(&raw).unwrap();
    damaged["data"]
        .as_object_mut()
        .unwrap()
        .remove("external_instructions");
    assert!(matches!(
        crate::session::frozen_snapshot::decode_frozen_snapshot(&damaged.to_string()),
        Err(FrozenSnapshotError::Invalid(_))
    ));
    let mut damaged: serde_json::Value = serde_json::from_str(&raw).unwrap();
    damaged["data"]["external_instructions"] =
        serde_json::json!(peri_agent::agent::model_bridge::SYSTEM_PROMPT_DYNAMIC_BOUNDARY);
    assert!(matches!(
        crate::session::frozen_snapshot::decode_frozen_snapshot(&damaged.to_string()),
        Err(FrozenSnapshotError::Invalid(_))
    ));
}

#[test]
fn test_frozen_snapshot_v1_embedded_instructions_keep_legacy_provenance() {
    let mut value: serde_json::Value = serde_json::from_str(
        &crate::session::frozen_snapshot::encode_frozen_snapshot(&make_frozen()).unwrap(),
    )
    .unwrap();
    value["version"] = serde_json::json!(1);
    value["data"]
        .as_object_mut()
        .unwrap()
        .remove("external_instructions");
    value["data"]["system_prompt"] =
        serde_json::json!("BASE\n<agent_instructions>\nlegacy\n</agent_instructions>");
    let restored =
        crate::session::frozen_snapshot::decode_frozen_snapshot(&value.to_string()).unwrap();
    assert!(restored.v2_frozen().legacy_embedded_instructions);
    assert_eq!(restored.v2_frozen().external_instructions, None);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            &crate::session::frozen_snapshot::encode_frozen_snapshot(&restored).unwrap()
        )
        .unwrap()["version"],
        1
    );
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
    let restored =
        crate::session::frozen_snapshot::decode_frozen_snapshot(raw).expect("旧 V1 blob 必须可读");
    assert_eq!(
        restored.runtime_env(),
        None,
        "缺少结构化环境值 = unavailable"
    );
    assert_eq!(restored.date(), "2026-09-01");
    assert_eq!(restored.system_prompt(), "system-v1");
    // V1 旧 blob 缺 beta_flags 键 → 空投影（一切按 false），不按当前配置重建。
    assert!(restored.v2_frozen().beta_flags.is_empty());
    assert!(!restored.v2_frozen().beta_flags.is_enabled(SNAPSHOT_FLAG));
}

#[test]
fn test_frozen_snapshot_future_version_fails_closed() {
    // Arrange
    let raw = r#"{"version":5,"data":{}}"#;
    // Act
    let Err(error) = crate::session::frozen_snapshot::decode_frozen_snapshot(raw) else {
        panic!("future versions must fail closed")
    };
    // Assert
    assert!(matches!(error, FrozenSnapshotError::UnsupportedVersion(5)));
}

#[test]
fn test_frozen_snapshot_missing_version_is_invalid() {
    // Arrange
    let raw = r#"{"data":{}}"#;
    // Act
    let Err(error) = crate::session::frozen_snapshot::decode_frozen_snapshot(raw) else {
        panic!("missing versions must fail closed")
    };
    // Assert
    assert!(matches!(error, FrozenSnapshotError::Invalid(_)));
    assert!(error.to_string().contains("missing unsigned version"));
}

/// D5：旧快照的覆盖集合可能只满足单段预算而超出**总**预算——按「只诊断、
/// 不改写」处理：审计命中，原正文与持久字节逐字保持（不回写、不裁剪、不改
/// 语义），旧会话仍可读。
#[test]
fn snapshot_over_total_override_budget_is_diagnosed_without_rewrite() {
    use crate::prompt::section_validation;
    use crate::prompt::section_validation::MAX_SECTION_OVERRIDE_BYTES;
    use crate::prompt::section_validation::MAX_TOTAL_OVERRIDE_BYTES;

    let chunk = "x".repeat(MAX_SECTION_OVERRIDE_BYTES);
    let mut overrides = serde_json::Map::new();
    for id in ["s1", "s2", "s3", "s4", "s5"] {
        overrides.insert(id.to_string(), serde_json::Value::String(chunk.clone()));
    }
    let raw = serde_json::json!({
        "version": 1,
        "data": {
            "system_prompt": "system-v1",
            "claude_md": "",
            "claude_local_md": null,
            "skill_summary": "",
            "date": "2026-09-01",
            "language": null,
            "meta_harness": {
                "section_overrides": overrides,
                "disabled_middlewares": [],
                "built_in_subagents_enabled": true,
            },
        }
    })
    .to_string();

    let decoded =
        crate::session::frozen_snapshot::decode_frozen_snapshot(&raw).expect("旧快照必须仍可读");
    let audit = section_validation::audit_total_override_budget(decoded.meta_harness())
        .expect("累计超总预算必须被诊断（只诊断，不改写）");
    assert_eq!(audit.sections, 5);
    assert!(audit.total_bytes > MAX_TOTAL_OVERRIDE_BYTES);

    // 不改写：正文逐字保留（不裁剪、不落空、不重编码为新语义）
    assert_eq!(decoded.meta_harness().section_overrides.len(), 5);
    for value in decoded.meta_harness().section_overrides.values() {
        assert_eq!(value.len(), MAX_SECTION_OVERRIDE_BYTES);
        assert!(value.chars().all(|c| c == 'x'));
    }
    let re_encoded =
        crate::session::frozen_snapshot::encode_frozen_snapshot(&decoded).expect("re-encode");
    assert!(
        re_encoded.contains(&chunk),
        "再编码仍逐字保留原覆盖正文（只诊断不改写）"
    );
}

#[test]
fn test_frozen_snapshot_environment_required_in_v3_and_absent_in_v2() {
    let mut value: serde_json::Value = serde_json::from_str(
        &crate::session::frozen_snapshot::encode_frozen_snapshot(&make_frozen()).unwrap(),
    )
    .unwrap();
    value["data"]
        .as_object_mut()
        .unwrap()
        .remove("environment_prompt");
    assert!(matches!(
        crate::session::frozen_snapshot::decode_frozen_snapshot(&value.to_string()),
        Err(FrozenSnapshotError::Invalid(_))
    ));
    value["version"] = serde_json::json!(2);
    let restored =
        crate::session::frozen_snapshot::decode_frozen_snapshot(&value.to_string()).unwrap();
    assert!(restored.v2_frozen().environment_prompt.is_none());
    assert_eq!(restored.system_prompt(), "system-v1");
    value["version"] = serde_json::json!(3);
    value["data"]["environment_prompt"] =
        serde_json::json!(peri_agent::agent::model_bridge::SYSTEM_PROMPT_DYNAMIC_BOUNDARY);
    assert!(matches!(
        crate::session::frozen_snapshot::decode_frozen_snapshot(&value.to_string()),
        Err(FrozenSnapshotError::Invalid(_))
    ));
}

#[test]
fn frozen_assembly_mode_survives_restore_and_old_snapshots_keep_v4_identity() {
    use peri_acp_types::builtin_mcp::BuiltinAssemblyMode;

    let mut context = make_frozen().v2_frozen().clone();
    context.builtin_assembly_mode = BuiltinAssemblyMode::SingleExit;
    let frozen = FrozenSessionData::from_frozen_parts(context, None);
    let mut value: serde_json::Value = serde_json::from_str(
        &crate::session::frozen_snapshot::encode_frozen_snapshot(&frozen).unwrap(),
    )
    .unwrap();
    let restored =
        crate::session::frozen_snapshot::decode_frozen_snapshot(&value.to_string()).unwrap();
    assert_eq!(
        restored.v2_frozen().builtin_assembly_mode,
        BuiltinAssemblyMode::SingleExit
    );

    value["version"] = serde_json::json!(3);
    value["data"]
        .as_object_mut()
        .unwrap()
        .remove("builtin_assembly_mode");
    let old = crate::session::frozen_snapshot::decode_frozen_snapshot(&value.to_string()).unwrap();
    assert_eq!(
        old.v2_frozen().builtin_assembly_mode,
        BuiltinAssemblyMode::LegacyInstances
    );

    value["version"] = serde_json::json!(4);
    assert!(matches!(
        crate::session::frozen_snapshot::decode_frozen_snapshot(&value.to_string()),
        Err(FrozenSnapshotError::Invalid(_))
    ));
}
