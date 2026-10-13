//! Tests for config_lfc

use std::time::Duration;

use super::*;

#[test]
fn test_batcher_config_default() {
    let config = BatcherConfig::default();
    assert_eq!(config.max_events, 50);
    assert_eq!(config.queue_capacity, 1024);
    assert_eq!(config.max_in_flight, 2);
    assert_eq!(config.max_event_bytes, 4 * 1024 * 1024);
    assert_eq!(config.max_batch_bytes, 4 * 1024 * 1024);
    assert_eq!(config.max_queue_bytes, 16 * 1024 * 1024);
    assert_eq!(config.flush_interval, Duration::from_secs(10));
    assert_eq!(config.backpressure, BackpressurePolicy::DropNew);
}

#[test]
fn telemetry_defaults_match_the_configuration_table() {
    let telemetry = TelemetryConfig::default();
    assert!(telemetry.spool_enabled);
    assert_eq!(telemetry.compression, Compression::Gzip);
    assert_eq!(telemetry.input_spool_threshold_bytes, 256 * 1024);
    assert_eq!(telemetry.payload_memory_threshold_bytes, 256 * 1024);
    assert_eq!(telemetry.memory_budget_bytes, 32 * 1024 * 1024);
    assert_eq!(telemetry.prepare_max_in_flight, 1);
    assert_eq!(telemetry.stream_chunk_bytes, 64 * 1024);
    assert_eq!(telemetry.spool_quota_bytes, 1024 * 1024 * 1024);
    assert_eq!(telemetry.spool_ttl, Duration::from_secs(86_400));
    assert_eq!(telemetry.max_wire_bytes, 4 * 1024 * 1024);
    assert_eq!(telemetry.shutdown_grace, Duration::from_secs(60));
    assert_eq!(Compression::default(), Compression::Gzip);
    telemetry.validate().unwrap();
}

#[test]
fn telemetry_rejects_out_of_range_budgets_and_limits() {
    let invalid = [
        TelemetryConfig {
            prepare_max_in_flight: 0,
            ..Default::default()
        },
        TelemetryConfig {
            prepare_max_in_flight: tokio::sync::Semaphore::MAX_PERMITS + 1,
            ..Default::default()
        },
        TelemetryConfig {
            memory_budget_bytes: 0,
            ..Default::default()
        },
        TelemetryConfig {
            stream_chunk_bytes: 0,
            ..Default::default()
        },
        TelemetryConfig {
            spool_quota_bytes: 0,
            ..Default::default()
        },
        TelemetryConfig {
            max_wire_bytes: 0,
            ..Default::default()
        },
        TelemetryConfig {
            input_spool_threshold_bytes: 32 * 1024 * 1024 + 1,
            ..Default::default()
        },
        TelemetryConfig {
            payload_memory_threshold_bytes: 32 * 1024 * 1024 + 1,
            ..Default::default()
        },
        TelemetryConfig {
            stream_chunk_bytes: 32 * 1024 * 1024 + 1,
            ..Default::default()
        },
        TelemetryConfig {
            spool_ttl: Duration::ZERO,
            ..Default::default()
        },
        TelemetryConfig {
            shutdown_grace: Duration::ZERO,
            ..Default::default()
        },
        TelemetryConfig {
            spool_ttl: Duration::MAX,
            ..Default::default()
        },
        TelemetryConfig {
            shutdown_grace: Duration::MAX,
            ..Default::default()
        },
    ];
    for telemetry in invalid {
        assert!(
            matches!(telemetry.validate(), Err(crate::LangfuseError::Config(_))),
            "非法遥测配置必须被拒绝：{telemetry:?}"
        );
    }
}

#[test]
fn batcher_validation_includes_telemetry() {
    let config = BatcherConfig {
        telemetry: TelemetryConfig {
            max_wire_bytes: 0,
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(matches!(
        config.validate(),
        Err(crate::LangfuseError::Config(_))
    ));
}

#[test]
fn batcher_validation_rejects_budgets_that_cannot_isolate_the_progress_partition() {
    // 8 MiB 足以隔离默认 P，retained 分区仍可用（契约 §4）。
    let workable = BatcherConfig {
        telemetry: TelemetryConfig {
            memory_budget_bytes: 8 * 1024 * 1024,
            ..Default::default()
        },
        ..Default::default()
    };
    workable.validate().expect("8 MiB 应能隔离默认进度分区");

    // 装不下 P 的预算必须在配置期拒绝，而不是等构造期才发现。
    for memory_budget_bytes in [1, 1024, 4 * 1024 * 1024] {
        let tight = BatcherConfig {
            telemetry: TelemetryConfig {
                memory_budget_bytes,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(
            matches!(tight.validate(), Err(crate::LangfuseError::Config(_))),
            "{memory_budget_bytes} 装不下进度分区，必须配置期拒绝"
        );
    }

    // 槽表 checked 乘加溢出同样属于配置错误，不留给运行期。
    let overflowing = BatcherConfig {
        queue_capacity: usize::MAX,
        ..Default::default()
    };
    assert!(matches!(
        overflowing.validate(),
        Err(crate::LangfuseError::Config(_))
    ));
}

#[test]
fn test_batcher_rejects_invalid_independent_capacity_and_byte_budgets() {
    for config in [
        BatcherConfig {
            queue_capacity: 0,
            ..Default::default()
        },
        BatcherConfig {
            max_in_flight: 0,
            ..Default::default()
        },
        BatcherConfig {
            max_event_bytes: 0,
            ..Default::default()
        },
        BatcherConfig {
            max_batch_bytes: 1,
            ..Default::default()
        },
        BatcherConfig {
            max_queue_bytes: 1,
            ..Default::default()
        },
    ] {
        assert!(matches!(
            config.validate(),
            Err(crate::LangfuseError::Config(_))
        ));
    }
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
        telemetry: TelemetryConfig {
            spool_enabled: false,
            ..Default::default()
        },
    };
    assert_eq!(cfg.trace_sampling, 0.1);
    assert!(cfg.error_span_always);
    assert_eq!(cfg.batch_max_events, 50);
    assert_eq!(cfg.batch_flush_interval_secs, 10);
    assert!(!cfg.telemetry.spool_enabled);
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
        telemetry: TelemetryConfig {
            compression: Compression::None,
            prepare_max_in_flight: 2,
            ..Default::default()
        },
    };
    let batcher_cfg = BatcherConfig::from_client(&client_cfg);
    assert_eq!(batcher_cfg.max_events, 100);
    assert_eq!(batcher_cfg.flush_interval, Duration::from_secs(5));
    assert_eq!(batcher_cfg.backpressure, BackpressurePolicy::Block);
    // 遥测配置逐字段复制，不重新解析来源也不静默回退默认值。
    assert_eq!(batcher_cfg.telemetry.compression, Compression::None);
    assert_eq!(batcher_cfg.telemetry.prepare_max_in_flight, 2);
    assert_eq!(
        batcher_cfg.telemetry.memory_budget_bytes,
        client_cfg.telemetry.memory_budget_bytes
    );
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
            Batcher::try_new(make_client(), config.clone(), None),
            Err(LangfuseError::Config(_))
        ));
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Batcher::new(make_client(), config, None);
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

/// 两个公开配置独立构造出内容编码 / wire 上限冲突时返回 `Config`，不任取一个（契约 §3.6）。
#[test]
fn test_batcher_construction_rejects_conflicting_export_and_telemetry_encoding() {
    use crate::{Batcher, LangfuseClient, LangfuseError};

    let conflicts = [
        crate::client::ExportConfig {
            compression: Compression::None,
            ..Default::default()
        },
        crate::client::ExportConfig {
            max_wire_bytes: 1 << 20,
            ..Default::default()
        },
    ];
    // 故意无 Tokio runtime：冲突必须在启动 worker 之前被拒绝。
    for export in conflicts {
        let client = LangfuseClient::new("pk-test", "sk-test", "http://127.0.0.1:1", 0)
            .with_export_config(export)
            .expect("独立构造的导出配置本身有效");
        assert!(matches!(
            Batcher::try_new(client, BatcherConfig::default(), None),
            Err(LangfuseError::Config(_))
        ));
    }
}

#[test]
fn test_batcher_config_validation_keeps_supported_bounds_and_values() {
    // `queue_capacity` 现在同时受槽表预算约束（契约 §3.1），故与 `max_events` 的 Semaphore
    // 上限解耦：本用例继续验证支持范围内的值被接受且不被校验重写。
    for max_events in [1, tokio::sync::Semaphore::MAX_PERMITS] {
        let config = BatcherConfig {
            max_events,
            flush_interval: Duration::from_nanos(1),
            backpressure: BackpressurePolicy::DropOldest,
            queue_capacity: 4,
            max_in_flight: 1,
            ..Default::default()
        };
        config.validate().unwrap();
        assert_eq!(config.max_events, max_events);
        assert_eq!(config.flush_interval, Duration::from_nanos(1));
        assert_eq!(config.queue_capacity, 4, "配置值不应被校验重写");
        assert_eq!(config.backpressure, BackpressurePolicy::DropOldest);
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
        None,
    )
    .expect("有效上限应只设容量约束，不预分配事件数组");
    tokio::time::timeout(Duration::from_secs(5), batcher.shutdown())
        .await
        .expect("空队列必须及时关闭")
        .expect("正常 join 不应有发送失败");
}
