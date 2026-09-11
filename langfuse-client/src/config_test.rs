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

#[test]
fn test_batcher_invalid_config_is_rejected_before_worker_spawn() {
    use crate::{Batcher, LangfuseClient, LangfuseError};
    let invalid = [
        BatcherConfig {
            max_events: 0,
            ..Default::default()
        },
        BatcherConfig {
            max_events: tokio::sync::Semaphore::MAX_PERMITS + 1,
            ..Default::default()
        },
        BatcherConfig {
            flush_interval: Duration::ZERO,
            ..Default::default()
        },
    ];
    // 故意无 Tokio runtime：必须先拒绝配置，不能进入 spawn 后才报错。
    for config in invalid {
        let make_client = || LangfuseClient::new("pk-test", "sk-test", "http://127.0.0.1:1", 0);
        assert!(matches!(
            Batcher::try_new(make_client(), config.clone()),
            Err(LangfuseError::Config(_))
        ));
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Batcher::new(make_client(), config);
        }))
        .expect_err("兼容 new 必须立即 panic，不能返回假就绪对象");
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap();
        assert!(message.contains("invalid batcher configuration"));
    }
}

#[test]
fn test_batcher_config_validation_keeps_supported_bounds_and_values() {
    for max_events in [1, tokio::sync::Semaphore::MAX_PERMITS] {
        let config = BatcherConfig {
            max_events,
            flush_interval: Duration::from_nanos(1),
            backpressure: BackpressurePolicy::DropOldest,
            max_retries: 99,
        };
        config.validate().unwrap();
        assert_eq!(config.max_events, max_events);
        assert_eq!(config.flush_interval, Duration::from_nanos(1));
        assert_eq!(config.max_retries, 99, "兼容字段不应被校验或重写");
    }
}

#[tokio::test]
async fn test_batcher_maximum_valid_capacity_constructs_and_shuts_down_without_preallocation() {
    let client = crate::LangfuseClient::new("pk-test", "sk-test", "http://127.0.0.1:1", 0);
    let batcher = crate::Batcher::try_new(
        client,
        BatcherConfig {
            max_events: tokio::sync::Semaphore::MAX_PERMITS,
            ..Default::default()
        },
    )
    .expect("有效上限应只设容量约束，不预分配事件数组");
    tokio::time::timeout(Duration::from_secs(5), batcher.shutdown())
        .await
        .expect("空队列必须及时关闭")
        .expect("正常 join 不应有发送失败");
}
