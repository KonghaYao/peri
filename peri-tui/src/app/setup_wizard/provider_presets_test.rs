//! Tests for provider_presets
use super::*;

#[test]
fn test_presets_are_complete_and_free_service_is_last() {
    assert_eq!(PROVIDER_PRESETS.len(), 9, "预设表条目数");
    assert_eq!(
        PROVIDER_PRESETS.last().unwrap().name,
        "Peri Code 免费服务",
        "免密钥网关固定在末位"
    );
}

#[test]
fn test_preset_identity_fields_are_filled() {
    for preset in PROVIDER_PRESETS {
        let name = preset.name;
        assert!(!name.trim().is_empty(), "预设缺少名称");
        assert!(!preset.summary.trim().is_empty(), "{name} 缺少摘要");
        assert!(
            !preset.provider_id.trim().is_empty(),
            "{name} 缺少 provider id"
        );
        assert!(
            preset.base_url.starts_with("https://"),
            "{name} 的 base_url 必须是 https 端点"
        );
    }
}

#[test]
fn test_preset_model_tiers_are_filled() {
    for preset in PROVIDER_PRESETS {
        for (tier, model) in ["fable", "opus", "sonnet", "haiku"]
            .iter()
            .zip(preset.models)
        {
            assert!(
                !model.trim().is_empty(),
                "{} 的 {tier} 档模型为空",
                preset.name
            );
        }
    }
}

#[test]
fn test_preset_provider_ids_are_unique() {
    let mut ids = PROVIDER_PRESETS
        .iter()
        .map(|preset| preset.provider_id)
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), PROVIDER_PRESETS.len(), "provider id 必须唯一");
}

#[test]
fn test_preset_base_url_matches_provider_type() {
    for preset in PROVIDER_PRESETS {
        let url = preset.base_url;
        match preset.provider_type {
            ProviderType::Anthropic => assert!(
                !url.ends_with("/v1/messages"),
                "{} 的 Anthropic 端点不应带 /v1/messages",
                preset.name
            ),
            ProviderType::OpenAiCompatible => assert!(
                url.ends_with("/v1"),
                "{} 的 OpenAI 兼容端点必须带 /v1 后缀",
                preset.name
            ),
        }
    }
}

#[test]
fn test_to_migrated_keeps_preset_values() {
    for preset in PROVIDER_PRESETS {
        let mp = preset.to_migrated();
        assert_eq!(mp.provider_type, preset.provider_type);
        assert_eq!(mp.provider_id, preset.provider_id);
        assert_eq!(mp.base_url, preset.base_url);
        assert_eq!(mp.api_key, preset.api_key);
        assert_eq!(mp.aliases, preset.models.map(String::from));
        assert_eq!(mp.fixed_tiers, preset.fixed_tiers);
        assert!(mp.selected, "预设 provider 应默认选中");
    }
}

#[test]
fn test_only_free_gateway_declares_fixed_tiers() {
    let fixed = PROVIDER_PRESETS
        .iter()
        .filter(|preset| preset.fixed_tiers)
        .collect::<Vec<_>>();
    assert_eq!(fixed.len(), 1, "仅免密钥网关声明固定档位");
    assert_eq!(fixed[0].name, "Peri Code 免费服务");
    assert!(!fixed[0].api_key.is_empty(), "固定档位网关自带 API Key");
}
