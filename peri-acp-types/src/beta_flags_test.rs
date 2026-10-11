//! `beta_flags` 模块测试：注册表约束与投影语义。

use super::*;

/// id 必须唯一且为 kebab-case（发布后不得改名，settings 键身份）。
#[test]
fn test_registry_ids_are_unique_kebab_case() {
    let mut seen = std::collections::HashSet::new();
    for flag in BETA_FLAGS {
        assert!(
            seen.insert(flag.id),
            "flag id 重复：{}（settings 键身份必须唯一）",
            flag.id
        );
        assert!(!flag.id.is_empty(), "flag id 不得为空");
        assert!(
            flag.id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "flag id 必须是 kebab-case：{}",
            flag.id
        );
        assert!(
            !flag.id.starts_with('-') && !flag.id.ends_with('-'),
            "flag id 不得以 '-' 开头或结尾：{}",
            flag.id
        );
        assert!(
            !flag.description.trim().is_empty(),
            "flag {} 必须有 canonical 描述（面板回退文本）",
            flag.id
        );
    }
}

/// 清单顺序稳定：首个条目是 `full-async-tools`（设计 §首个 flag）。
#[test]
fn test_registry_declares_full_async_tools_first() {
    assert_eq!(
        BETA_FLAGS.first().map(|flag| flag.id),
        Some(FULL_ASYNC_TOOLS)
    );
}

/// 查找命中/未命中。
#[test]
fn test_find_hit_and_miss() {
    let found = find(FULL_ASYNC_TOOLS).expect("注册表条目必须可查");
    assert_eq!(found.id, FULL_ASYNC_TOOLS);
    assert_eq!(found.description, BETA_FLAGS[0].description);
    assert!(find("not-a-flag").is_none());
    assert!(find("").is_none());
}

/// 未覆盖与未知 id 一律 false（快照缺失/配置面不可用不得意外开启能力）。
#[test]
fn test_uncovered_and_unknown_ids_default_false() {
    let empty = BetaFlags::default();
    assert!(empty.is_empty());
    assert!(!empty.is_enabled(FULL_ASYNC_TOOLS));
    assert!(empty.value(FULL_ASYNC_TOOLS).is_none());

    let flags = BetaFlags::from_values([(
        FULL_ASYNC_TOOLS.to_string(),
        BetaFlagValue {
            enabled: false,
            origin: BetaFlagOrigin::Workspace,
        },
    )]);
    assert!(
        !flags.is_enabled(FULL_ASYNC_TOOLS),
        "显式 false 有效值为 false"
    );
    assert!(
        flags.value(FULL_ASYNC_TOOLS).is_some(),
        "显式 false 与未覆盖必须可区分（面板显示语义）"
    );
    assert!(!flags.is_enabled("unknown-flag"));
    assert_eq!(flags.enabled().count(), 0);
}

/// 生效条目按 id 稳定顺序列出，并保留来源层。
#[test]
fn test_enabled_entries_report_origin() {
    let flags = BetaFlags::from_values([
        (
            "zz-last".to_string(),
            BetaFlagValue {
                enabled: true,
                origin: BetaFlagOrigin::Workspace,
            },
        ),
        (
            FULL_ASYNC_TOOLS.to_string(),
            BetaFlagValue {
                enabled: true,
                origin: BetaFlagOrigin::Global,
            },
        ),
    ]);
    let enabled: Vec<_> = flags.enabled().collect();
    assert_eq!(
        enabled,
        vec![
            (FULL_ASYNC_TOOLS, BetaFlagOrigin::Global),
            ("zz-last", BetaFlagOrigin::Workspace)
        ]
    );
    assert!(flags.is_enabled(FULL_ASYNC_TOOLS));
}
