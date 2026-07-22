//! Tests for config_lfc

use std::time::Duration;

use super::*;

#[test]
fn test_batcher_config_default() {
    let config = BatcherConfig::default();
    assert_eq!(config.max_events, 50);
    assert_eq!(config.flush_interval, Duration::from_secs(10));
    assert_eq!(config.backpressure, BackpressurePolicy::DropNew);
    assert_eq!(config.max_retries, 3);
}

#[test]
fn test_backpressure_default() {
    assert_eq!(BackpressurePolicy::default(), BackpressurePolicy::DropNew);
}

#[test]
fn test_client_config_from_env() {
    temp_env::with_vars(
        [
            ("LANGFUSE_PUBLIC_KEY", Some("pk-test")),
            ("LANGFUSE_SECRET_KEY", Some("sk-test")),
            ("LANGFUSE_BASE_URL", Some("https://custom.langfuse.com")),
        ],
        || {
            let config = ClientConfig::from_env().unwrap();
            assert_eq!(config.public_key, "pk-test");
            assert_eq!(config.secret_key, "sk-test");
            assert_eq!(config.base_url, "https://custom.langfuse.com");
        },
    );
}

#[test]
fn test_client_config_from_env_missing_key() {
    temp_env::with_vars_unset(["LANGFUSE_PUBLIC_KEY", "LANGFUSE_SECRET_KEY"], || {
        let result = ClientConfig::from_env();
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("LANGFUSE_PUBLIC_KEY not set"), "got: {}", msg);
    });
}

#[test]
fn test_client_config_default_base_url() {
    temp_env::with_vars(
        [
            ("LANGFUSE_PUBLIC_KEY", Some("pk")),
            ("LANGFUSE_SECRET_KEY", Some("sk")),
            ("LANGFUSE_BASE_URL", None),
        ],
        || {
            let config = ClientConfig::from_env().unwrap();
            assert_eq!(config.base_url, "https://cloud.langfuse.com");
        },
    );
}

#[test]
fn test_client_config_new_fields_default() {
    let cfg = ClientConfig {
        public_key: "pk".into(),
        secret_key: "sk".into(),
        base_url: "https://cloud.langfuse.com".into(),
        trace_sampling: 0.1,
        error_span_always: true,
        batch_max_events: 50,
        batch_flush_interval_secs: 10,
        batch_backpressure: BackpressurePolicy::DropNew,
    };
    assert_eq!(cfg.trace_sampling, 0.1);
    assert!(cfg.error_span_always);
    assert_eq!(cfg.batch_max_events, 50);
    assert_eq!(cfg.batch_flush_interval_secs, 10);
}

#[test]
fn test_backpressure_policy_drop_oldest_exists() {
    let p = BackpressurePolicy::DropOldest;
    assert_eq!(format!("{:?}", p), "DropOldest");
}

#[test]
fn test_batcher_config_from_client() {
    let client_cfg = ClientConfig {
        public_key: "pk".into(),
        secret_key: "sk".into(),
        base_url: "https://cloud.langfuse.com".into(),
        trace_sampling: 1.0,
        error_span_always: true,
        batch_max_events: 100,
        batch_flush_interval_secs: 5,
        batch_backpressure: BackpressurePolicy::Block,
    };
    let batcher_cfg = BatcherConfig::from_client(&client_cfg);
    assert_eq!(batcher_cfg.max_events, 100);
    assert_eq!(batcher_cfg.flush_interval, Duration::from_secs(5));
    assert_eq!(batcher_cfg.backpressure, BackpressurePolicy::Block);
    assert_eq!(batcher_cfg.max_retries, 3);
}
