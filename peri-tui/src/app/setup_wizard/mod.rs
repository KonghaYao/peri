//! Setup Wizard —— 首次启动配置检测。
//!
//! (I16-C) 大幅瘦身：原 1673 行的完整表单逻辑（SetupWizardPanel +
//! handle_setup_wizard_key + 多步骤表单 + Claude Code 迁移 + 连通性测试）
//! 已退役——kit 单路径下 `kit/setup_wizard.rs` 是 TODO stub，
//! `wizard_active` atom 永远为 false，整套表单渲染与按键处理无消费者。
//!
//! 本模块仅保留 `needs_setup()` 检测函数，供 launch.rs 启动期判断是否
//! 需要引导用户配置 Provider（日志提示）；未来 kit 实现完整 Setup Wizard
//! 时可在此重建。

/// 检测配置是否需要 Setup 向导。
///
/// 判定规则：
/// 1. providers 为空 → 检查环境变量能否提供有效 provider，无则返回 true
/// 2. 任一 provider 的 id 为空 / api_key 为空（且对应环境变量也未设置）→ true
/// 3. 否则 false
pub fn needs_setup(config: &crate::config::AppConfig) -> bool {
    if config.providers.is_empty() {
        // 没有 providers 条目时，检查是否可通过环境变量提供 provider
        // 避免已有 env 配置但 settings.json 格式不标准时阻塞用户
        return crate::app::agent::LlmProvider::from_env().is_none();
    }
    for provider in &config.providers {
        if provider.id.trim().is_empty() {
            return true;
        }
        if provider.api_key.is_empty() {
            let key_env = match provider.provider_type.as_str() {
                "anthropic" => "ANTHROPIC_API_KEY",
                _ => "OPENAI_API_KEY",
            };
            if std::env::var(key_env).unwrap_or_default().is_empty() {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, ProviderConfig};

    #[test]
    fn test_needs_setup_empty_providers_no_env() {
        let config = AppConfig::default();
        // 无 providers 且显式移除所有已知 API key 环境变量 → 需要 setup
        unsafe {
            std::env::set_var("MODEL_PROVIDER", "__nonexistent__");
        }
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
        }
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
        assert!(
            needs_setup(&config),
            "无 providers 且无有效 env 时应需要 setup"
        );
    }

    #[test]
    fn test_needs_setup_empty_providers_but_env_key() {
        let config = AppConfig::default();
        // 无 providers 但 OPENAI_API_KEY + MODEL_PROVIDER=openai → 不需要 setup
        unsafe {
            std::env::set_var("MODEL_PROVIDER", "openai");
        }
        unsafe {
            std::env::set_var("OPENAI_API_KEY", "sk-fake-test-key");
        }
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
        assert!(
            !needs_setup(&config),
            "env 提供 OPENAI_API_KEY + MODEL_PROVIDER=openai 时不需要 setup"
        );
    }

    #[test]
    fn test_needs_setup_api_key_from_config() {
        let mut config = AppConfig::default();
        config.providers.push(ProviderConfig {
            id: "test".into(),
            provider_type: "openai".into(),
            api_key: "sk-fake-test-key".into(),
            ..Default::default()
        });
        // 已配置 provider + api_key → 不需要 setup
        assert!(!needs_setup(&config));
    }
}
