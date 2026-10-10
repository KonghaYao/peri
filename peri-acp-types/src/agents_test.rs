//! `agents` 模块的 typed 模型档位契约测试（M2）。

use super::*;

/// `MODEL_TIERS` 与 `ModelTier::ALL` 必须逐项对应（`as_str` 依赖该同序映射）。
#[test]
fn model_tier_all_matches_model_tiers_constant() {
    assert_eq!(ModelTier::ALL.len(), MODEL_TIERS.len());
    for (tier, name) in ModelTier::ALL.into_iter().zip(MODEL_TIERS) {
        assert_eq!(tier.as_str(), name, "档位枚举与常量必须同序同值");
    }
}

/// 解析：大小写不敏感、首尾空白忽略；未指定/`inherit` 归一；未知值 typed 拒绝。
#[test]
fn parse_distinguishes_unspecified_inherit_and_valid_tiers() {
    assert_eq!(
        AgentModelSelection::parse(None).unwrap(),
        AgentModelSelection::Unspecified
    );
    assert_eq!(
        AgentModelSelection::parse(Some("   ")).unwrap(),
        AgentModelSelection::Unspecified
    );
    assert_eq!(
        AgentModelSelection::parse(Some("InHerit")).unwrap(),
        AgentModelSelection::Inherit
    );
    for (raw, tier) in [
        ("haiku", ModelTier::Haiku),
        (" SoNnEt ", ModelTier::Sonnet),
        ("OPUS", ModelTier::Opus),
        ("fable", ModelTier::Fable),
        // 尾随换行是 YAML 常见形态：trim 后按合法档位归一（注入形状仍被拒绝）。
        ("haiku\n", ModelTier::Haiku),
    ] {
        assert_eq!(
            AgentModelSelection::parse(Some(raw)).unwrap(),
            AgentModelSelection::Tier(tier),
            "{raw:?} 应按大小写不敏感归一"
        );
    }
}

/// 未知档位与注入形状一律 typed 拒绝（不构造值、不回显可疑输入）。
#[test]
fn parse_rejects_unknown_and_injection_shapes() {
    for raw in ["turbo", "sonnet]\n- evil [opus", "gpt-4", "inherit extra"] {
        assert_eq!(
            AgentModelSelection::parse(Some(raw)).unwrap_err(),
            InvalidModelTier,
            "{raw:?} 必须被拒绝"
        );
    }
    // 错误文本不回显原始值。
    let message = InvalidModelTier.to_string();
    assert!(
        !message.contains("turbo") && !message.contains("evil"),
        "{message}"
    );
}

/// 渲染/执行投影：catalog_label 只能是静态档位名或 inherit；
/// tier_alias 只在合法档位时给出别名。
#[test]
fn selection_projections_are_static_and_lossless() {
    assert_eq!(AgentModelSelection::Unspecified.catalog_label(), "inherit");
    assert_eq!(AgentModelSelection::Inherit.catalog_label(), "inherit");
    assert_eq!(
        AgentModelSelection::Tier(ModelTier::Fable).catalog_label(),
        "fable"
    );
    assert_eq!(AgentModelSelection::Unspecified.tier_alias(), None);
    assert_eq!(AgentModelSelection::Inherit.tier_alias(), None);
    assert_eq!(
        AgentModelSelection::Tier(ModelTier::Opus).tier_alias(),
        Some("opus")
    );
    assert_eq!(AgentModelSelection::Unspecified.normalized_value(), None);
    assert_eq!(
        AgentModelSelection::Inherit.normalized_value(),
        Some("inherit")
    );
    assert_eq!(
        AgentModelSelection::Tier(ModelTier::Haiku).normalized_value(),
        Some("haiku")
    );
}
