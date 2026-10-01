use super::LangfuseConfig;

#[test]
fn test_reexported_config_uses_core_defaults() {
    let config = LangfuseConfig::default();
    assert!(config.public_key.is_none());
    assert!(config.secret_key.is_none());
    assert_eq!(config.host, "https://cloud.langfuse.com");
    assert_eq!(config.trace_sampling, 1.0);
    assert_eq!(config.batch_max_events, 50);
}
