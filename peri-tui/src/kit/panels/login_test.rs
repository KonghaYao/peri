//! Tests for LoginPanel component.

use super::config_store::apply_login_edit;
use super::*;
use crate::config::PeriConfig;

fn openai_provider_config(api: Option<ApiProtocol>) -> ProviderConfig {
    ProviderConfig {
        id: "p1".into(),
        provider_type: "openai".into(),
        api_key: "sk-test".into(),
        base_url: "https://api.example.com/v1".into(),
        api,
        ..Default::default()
    }
}

#[test]
fn login_edit_state_reads_api_protocol_from_config() {
    // 显式 responses：编辑面板初始化必须显示实际选择
    let state =
        LoginEditState::from_provider_config(&openai_provider_config(Some(ApiProtocol::Responses)));
    assert_eq!(state.api, ApiProtocol::Responses);
    assert_eq!(state.effective_api(), Some(ApiProtocol::Responses));

    // 未声明（旧配置）：缺省 chat_completions，落盘保持显式值
    let state = LoginEditState::from_provider_config(&openai_provider_config(None));
    assert_eq!(state.api, ApiProtocol::ChatCompletions);
    assert_eq!(state.effective_api(), Some(ApiProtocol::ChatCompletions));
}

#[test]
fn login_api_cycle_only_applies_to_openai() {
    let mut state = LoginEditState::from_provider_config(&openai_provider_config(None));
    state.cycle_api_protocol();
    assert_eq!(state.api, ApiProtocol::Responses);
    state.cycle_api_protocol();
    assert_eq!(state.api, ApiProtocol::ChatCompletions);

    let mut anthropic = LoginEditState::from_provider_config(&ProviderConfig {
        provider_type: "anthropic".into(),
        ..openai_provider_config(None)
    });
    anthropic.cycle_api_protocol();
    assert_eq!(
        anthropic.api,
        ApiProtocol::ChatCompletions,
        "anthropic 下切换协议不应产生不可落盘的值"
    );
}

#[test]
fn login_edit_clears_api_when_switching_to_anthropic() {
    let mut state =
        LoginEditState::from_provider_config(&openai_provider_config(Some(ApiProtocol::Responses)));
    state.toggle_provider_type();
    assert_eq!(state.provider_type, "anthropic");
    assert_eq!(state.api, ApiProtocol::ChatCompletions);
    assert_eq!(state.effective_api(), None);

    // 残留非法组合（anthropic + 显式 api）落盘一律写 None
    state.api = ApiProtocol::Responses;
    let mut cfg = PeriConfig::default();
    cfg.config
        .providers
        .push(openai_provider_config(Some(ApiProtocol::Responses)));
    assert!(apply_login_edit(&mut cfg, &state));
    assert_eq!(
        cfg.config.providers[0].api, None,
        "anthropic provider 不得携带 api 字段"
    );
}

#[test]
fn login_save_writes_selected_protocol_and_reopen_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");

    let mut cfg = PeriConfig::default();
    cfg.config.active_alias = "opus".into();

    // New 路径：新建 openai provider 并选择 Responses
    let mut state = LoginEditState::default_empty();
    state.provider_type = "openai".into();
    state.provider_id = "p1".into();
    state.api_key = "sk-test".into();
    state.base_url = "https://api.example.com/v1".into();
    state.opus_model = "gpt-5.6".into();
    state.api = ApiProtocol::Responses;

    assert!(apply_login_edit(&mut cfg, &state), "保存变换必须成功");
    crate::config::save_to(&cfg, &path).unwrap();

    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(
        raw.contains("\"api\": \"responses\""),
        "落盘必须写入强类型 api 字段：{raw}"
    );

    // 重开：解析回同一协议，且不落入 extra
    let reopened = crate::config::load_from(&path).unwrap();
    let provider = reopened
        .config
        .providers
        .iter()
        .find(|p| p.id == "p1")
        .expect("保存后的 provider 必须可重开");
    assert_eq!(provider.api, Some(ApiProtocol::Responses));
    assert!(provider.extra.get("api").is_none(), "api 不得被 extra 吸收");

    // 编辑面板重开保留选择
    let re_edit = LoginEditState::from_provider_config(provider);
    assert_eq!(re_edit.api, ApiProtocol::Responses);
    assert_eq!(re_edit.effective_api(), Some(ApiProtocol::Responses));

    // 保存结果直接进入 provider 工厂：协议为 Responses（用户可用路径闭环）
    let factory_provider = crate::app::agent::LlmProvider::from_config(&reopened)
        .expect("保存后的配置必须可构造 provider");
    assert_eq!(
        factory_provider.api_protocol(),
        Some(ApiProtocol::Responses)
    );
    assert_eq!(factory_provider.protocol_key(), "responses");
}

#[test]
fn login_edit_preserves_other_providers_protocol() {
    // 编辑一个 provider 不得影响另一个已选协议的 provider（切换/快速切换保留选择）
    let mut cfg = PeriConfig::default();
    cfg.config.active_alias = "opus".into();
    cfg.config
        .providers
        .push(openai_provider_config(Some(ApiProtocol::Responses)));
    cfg.config.providers.push(ProviderConfig {
        id: "p2".into(),
        provider_type: "openai".into(),
        api_key: "sk-test-2".into(),
        base_url: "https://api.example.com/v1".into(),
        api: Some(ApiProtocol::ChatCompletions),
        ..Default::default()
    });

    let mut state =
        LoginEditState::from_provider_config(&openai_provider_config(Some(ApiProtocol::Responses)));
    state.api = ApiProtocol::ChatCompletions;
    assert!(apply_login_edit(&mut cfg, &state));

    assert_eq!(
        cfg.config.providers[0].api,
        Some(ApiProtocol::ChatCompletions)
    );
    assert_eq!(
        cfg.config.providers[1].api,
        Some(ApiProtocol::ChatCompletions),
        "未编辑的 provider 协议选择保持不变"
    );
}
