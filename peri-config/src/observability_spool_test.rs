//! 新遥测键（spool/compression）的解析契约测试（契约 §6、§8 P1）。
//!
//! 覆盖 11 个新键的全部默认值、settings 与 env 优先级、布尔/编码规范化形式、整数边界与
//! 非法胜出值；非法胜出值必须从**配置发布入口**被拒，且不发布新快照。旧键的忽略/clamp
//! 语义与总环境键表仍由 `observability_test.rs` 覆盖，本文件不重复。

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::observability::{
    resolve, ConfigValueSource, LangfuseCompression, LangfuseConfig, LangfuseConfigError,
};
use crate::source::ConfigurationSource;
use crate::{
    ConfigurationError, ConfigurationInputs, ConfigurationScope, ConfigurationSystem,
    SourceIdentity,
};

fn resolve_ok(
    settings: &serde_json::Value,
    environment: &BTreeMap<String, String>,
) -> LangfuseConfig {
    resolve(settings, environment).expect("测试配置必须解析成功")
}

#[test]
fn spool_defaults_and_env_precedence() {
    let defaults = LangfuseConfig::default();
    assert!(defaults.spool_enabled);
    assert_eq!(defaults.compression, LangfuseCompression::Gzip);
    assert_eq!(defaults.input_spool_threshold_bytes, 256 * 1024);
    assert_eq!(defaults.payload_memory_threshold_bytes, 256 * 1024);
    assert_eq!(defaults.memory_budget_bytes, 32 * 1024 * 1024);
    assert_eq!(defaults.prepare_max_in_flight, 1);
    assert_eq!(defaults.stream_chunk_bytes, 64 * 1024);
    assert_eq!(defaults.spool_quota_bytes, 1024 * 1024 * 1024);
    assert_eq!(defaults.spool_ttl_secs, 86_400);
    assert_eq!(defaults.max_wire_bytes, 4 * 1024 * 1024);
    assert_eq!(defaults.shutdown_grace_secs, 60);

    let settings = json!({"langfuse": {
        "spool_enabled": false,
        "compression": "none",
        "input_spool_threshold_bytes": 1,
        "payload_memory_threshold_bytes": 2,
        "memory_budget_bytes": 3,
        "prepare_max_in_flight": 4,
        "stream_chunk_bytes": 5,
        "spool_quota_bytes": 6,
        "spool_ttl_secs": 7,
        "max_wire_bytes": 8,
        "shutdown_grace_secs": 9
    }});
    let from_settings = resolve_ok(&settings, &BTreeMap::new());
    assert!(!from_settings.spool_enabled);
    assert_eq!(from_settings.compression, LangfuseCompression::None);
    assert_eq!(from_settings.input_spool_threshold_bytes, 1);
    assert_eq!(from_settings.payload_memory_threshold_bytes, 2);
    assert_eq!(from_settings.memory_budget_bytes, 3);
    assert_eq!(from_settings.prepare_max_in_flight, 4);
    assert_eq!(from_settings.stream_chunk_bytes, 5);
    assert_eq!(from_settings.spool_quota_bytes, 6);
    assert_eq!(from_settings.spool_ttl_secs, 7);
    assert_eq!(from_settings.max_wire_bytes, 8);
    assert_eq!(from_settings.shutdown_grace_secs, 9);

    let environment = BTreeMap::from([
        ("LANGFUSE_SPOOL_ENABLED".to_string(), "on".to_string()),
        ("LANGFUSE_COMPRESSION".to_string(), " GZip ".to_string()),
        (
            "LANGFUSE_INPUT_SPOOL_THRESHOLD_BYTES".to_string(),
            "11".to_string(),
        ),
        (
            "LANGFUSE_PAYLOAD_MEMORY_THRESHOLD_BYTES".to_string(),
            "12".to_string(),
        ),
        ("LANGFUSE_MEMORY_BUDGET_BYTES".to_string(), "13".to_string()),
        (
            "LANGFUSE_PREPARE_MAX_IN_FLIGHT".to_string(),
            "14".to_string(),
        ),
        ("LANGFUSE_STREAM_CHUNK_BYTES".to_string(), "15".to_string()),
        ("LANGFUSE_SPOOL_QUOTA_BYTES".to_string(), "16".to_string()),
        ("LANGFUSE_SPOOL_TTL_SECS".to_string(), "17".to_string()),
        ("LANGFUSE_MAX_WIRE_BYTES".to_string(), "18".to_string()),
        ("LANGFUSE_SHUTDOWN_GRACE_SECS".to_string(), "19".to_string()),
    ]);
    let from_environment = resolve_ok(&settings, &environment);
    assert!(from_environment.spool_enabled);
    assert_eq!(from_environment.compression, LangfuseCompression::Gzip);
    assert_eq!(from_environment.input_spool_threshold_bytes, 11);
    assert_eq!(from_environment.payload_memory_threshold_bytes, 12);
    assert_eq!(from_environment.memory_budget_bytes, 13);
    assert_eq!(from_environment.prepare_max_in_flight, 14);
    assert_eq!(from_environment.stream_chunk_bytes, 15);
    assert_eq!(from_environment.spool_quota_bytes, 16);
    assert_eq!(from_environment.spool_ttl_secs, 17);
    assert_eq!(from_environment.max_wire_bytes, 18);
    assert_eq!(from_environment.shutdown_grace_secs, 19);
}

#[test]
fn new_boolean_and_compression_forms_are_normalized() {
    for (text, expected) in [
        ("true", true),
        ("TRUE", true),
        (" 1 ", true),
        ("On", true),
        ("false", false),
        ("False", false),
        ("0", false),
        ("off", false),
    ] {
        let environment = BTreeMap::from([("LANGFUSE_SPOOL_ENABLED".to_string(), text.into())]);
        assert_eq!(
            resolve_ok(&json!({}), &environment).spool_enabled,
            expected,
            "{text} 必须解析为 {expected}"
        );
    }
    for (text, expected) in [
        ("gzip", LangfuseCompression::Gzip),
        (" NONE ", LangfuseCompression::None),
    ] {
        let environment = BTreeMap::from([("LANGFUSE_COMPRESSION".to_string(), text.into())]);
        assert_eq!(resolve_ok(&json!({}), &environment).compression, expected);
        let settings = json!({"langfuse": {"compression": text.trim()}});
        assert_eq!(
            resolve_ok(&settings, &BTreeMap::new()).compression,
            expected
        );
    }
}

#[test]
fn new_integer_keys_reject_zero_negative_float_overflow_and_blank() {
    // 环境来源：空白、负号、小数点、非数字与超出 u64 的值全部拒绝。
    for text in [
        "0",
        "-1",
        "1.5",
        "",
        "   ",
        "1 2",
        "abc",
        "18446744073709551616",
    ] {
        let environment = BTreeMap::from([("LANGFUSE_SPOOL_TTL_SECS".to_string(), text.into())]);
        let error = resolve(&json!({}), &environment).expect_err("非法整数不得回退默认值");
        assert!(
            matches!(
                error,
                LangfuseConfigError::Invalid {
                    key: "LANGFUSE_SPOOL_TTL_SECS",
                    origin: ConfigValueSource::Environment,
                    ..
                }
            ),
            "{text} 必须按环境来源拒绝：{error}"
        );
    }
    // settings 来源：JSON 类型必须是无符号整数，0 同样拒绝。
    for value in [
        json!(0),
        json!(-1),
        json!(1.5),
        json!("30"),
        json!(null),
        json!(true),
    ] {
        let settings = json!({"langfuse": {"stream_chunk_bytes": value}});
        let error = resolve(&settings, &BTreeMap::new()).expect_err("非法 settings 整数必须拒绝");
        assert!(
            matches!(
                error,
                LangfuseConfigError::Invalid {
                    key: "stream_chunk_bytes",
                    origin: ConfigValueSource::Settings,
                    ..
                }
            ),
            "{value} 必须按 settings 来源拒绝：{error}"
        );
    }
    // 合法边界：1 与 u64 上限（usize 装不下时按平台范围拒绝）。
    let environment = BTreeMap::from([
        ("LANGFUSE_MEMORY_BUDGET_BYTES".to_string(), "1".to_string()),
        (
            "LANGFUSE_SPOOL_QUOTA_BYTES".to_string(),
            u64::MAX.to_string(),
        ),
    ]);
    let config = resolve_ok(&json!({}), &environment);
    assert_eq!(config.memory_budget_bytes, 1);
    assert_eq!(config.spool_quota_bytes, u64::MAX);
}

#[test]
fn invalid_winning_value_rejects_every_new_key() {
    for (key, value) in [
        ("LANGFUSE_SPOOL_ENABLED", "maybe"),
        ("LANGFUSE_COMPRESSION", "zstd"),
        ("LANGFUSE_INPUT_SPOOL_THRESHOLD_BYTES", "zero"),
        ("LANGFUSE_PAYLOAD_MEMORY_THRESHOLD_BYTES", "-2"),
        ("LANGFUSE_MEMORY_BUDGET_BYTES", "0"),
        ("LANGFUSE_PREPARE_MAX_IN_FLIGHT", "1.0"),
        ("LANGFUSE_STREAM_CHUNK_BYTES", "many"),
        ("LANGFUSE_SPOOL_QUOTA_BYTES", "0"),
        ("LANGFUSE_SPOOL_TTL_SECS", ""),
        ("LANGFUSE_MAX_WIRE_BYTES", "0"),
        ("LANGFUSE_SHUTDOWN_GRACE_SECS", "60s"),
    ] {
        let environment = BTreeMap::from([(key.to_string(), value.to_string())]);
        let error = resolve(&json!({}), &environment).expect_err("胜出非法值必须拒绝");
        assert!(
            matches!(
                error,
                LangfuseConfigError::Invalid { key: reported, .. } if reported == key
            ),
            "{key}={value} 必须报告自身键名：{error}"
        );
    }
    // 错误只由静态原因构成，不携带凭据或原配置值。
    let error = resolve(
        &json!({"langfuse": {"spool_enabled": "settings-secret-token"}}),
        &BTreeMap::new(),
    )
    .unwrap_err();
    assert!(!format!("{error}").contains("settings-secret-token"));
    let error = resolve(
        &json!({}),
        &BTreeMap::from([(
            "LANGFUSE_COMPRESSION".to_string(),
            "environment-secret-token".to_string(),
        )]),
    )
    .unwrap_err();
    assert!(!format!("{error}").contains("environment-secret-token"));
    // settings 的类型错误同样拒绝，不回退到默认值。
    for (key, value) in [
        ("spool_enabled", json!("true")),
        ("compression", json!(1)),
        ("input_spool_threshold_bytes", json!(-1)),
        ("memory_budget_bytes", json!("1024")),
    ] {
        let settings = json!({"langfuse": {key: value}});
        let error = resolve(&settings, &BTreeMap::new()).expect_err("settings 非法值必须拒绝");
        assert!(
            matches!(
                error,
                LangfuseConfigError::Invalid { key: reported, .. } if reported == key
            ),
            "{key}={value} 必须报告 settings 键名：{error}"
        );
    }
}

/// 关闭 spool 只关闭新磁盘路径的开关；旧键解析、clamp 与忽略语义不变，也不产生新配置面。
#[test]
fn disabled_spool_preserves_old_files() {
    let environment = BTreeMap::from([
        ("LANGFUSE_SPOOL_ENABLED".to_string(), "false".to_string()),
        ("LANGFUSE_TRACE_SAMPLING".to_string(), "2.0".to_string()),
        ("LANGFUSE_BATCH_MAX_EVENTS".to_string(), "bad".to_string()),
        ("LANGFUSE_BATCH_MAX_BYTES".to_string(), "8192".to_string()),
    ]);
    let config = resolve_ok(
        &json!({"langfuse": {"batch_max_events": 100, "batch_max_bytes": 4096}}),
        &environment,
    );
    assert!(!config.spool_enabled);
    assert_eq!(config.trace_sampling, 1.0);
    assert_eq!(config.batch_max_events, 100);
    assert_eq!(config.batch_max_bytes, 8192);
    assert_eq!(
        config.input_spool_threshold_bytes,
        LangfuseConfig::default().input_spool_threshold_bytes
    );
}

/// 胜出的非法新值必须从发布入口拒绝整快照：环境与 settings 两种来源都不得改写当前快照，
/// 也**不得发布**新快照（只断言 `resolve` 返回 `Err` 不足以证明这一点）。
#[test]
fn invalid_winning_value_rejects_snapshot() {
    let temp = tempfile::tempdir().expect("临时目录");
    let scope = ConfigurationScope::new(
        temp.path().join("project"),
        temp.path().join("global/settings.json"),
    )
    .expect("绝对路径 scope");
    let source = FixedSource::new(ConfigurationInputs {
        global: Some(r#"{"langfuse":{"compression":"none","memory_budget_bytes":4096}}"#.into()),
        environment: BTreeMap::from([("LANGFUSE_SPOOL_ENABLED".into(), "on".into())]),
        ..Default::default()
    });
    let system = ConfigurationSystem::new(source.clone());
    let published = system.resolve(scope.clone()).expect("首次快照发布成功");
    assert_eq!(
        published.observability().compression,
        LangfuseCompression::None
    );
    assert!(published.observability().spool_enabled);

    // 环境来源非法：按胜出变量定位失败，当前快照仍是同一个 Arc，值也不变。
    source.mutate(|inputs| {
        inputs
            .environment
            .insert("LANGFUSE_COMPRESSION".into(), "zstd".into());
    });
    let error = system
        .resolve(scope.clone())
        .expect_err("非法胜出值必须拒绝");
    assert!(matches!(
        &error,
        ConfigurationError::InvalidInput {
            source_identity: SourceIdentity::Environment(key),
            domain: "observability",
            ..
        } if key == "LANGFUSE_COMPRESSION"
    ));
    assert!(
        Arc::ptr_eq(&published, &system.current(&scope).expect("旧快照仍在位")),
        "非法环境值不得发布新快照"
    );
    assert_eq!(
        system.current(&scope).unwrap().observability().compression,
        LangfuseCompression::None
    );

    // settings 来源非法：同样只保留旧快照。
    source.mutate(|inputs| {
        inputs.environment.clear();
        inputs.global = Some(r#"{"langfuse":{"spool_enabled":"maybe"}}"#.into());
    });
    let error = system
        .resolve(scope.clone())
        .expect_err("非法 settings 值必须拒绝");
    assert!(matches!(
        &error,
        ConfigurationError::InvalidInput {
            source_identity: SourceIdentity::GlobalFile,
            domain: "observability",
            ..
        }
    ));
    assert!(Arc::ptr_eq(
        &published,
        &system.current(&scope).expect("旧快照仍在位")
    ));

    // 修正后必须真的发布新快照：证明上面的 `Arc::ptr_eq` 判的是「未变」而不是「从不写入」。
    source.mutate(|inputs| {
        inputs.global = Some(r#"{"langfuse":{"compression":"gzip","spool_enabled":false}}"#.into());
    });
    let corrected = system.resolve(scope.clone()).expect("修正后发布成功");
    assert!(!corrected.observability().spool_enabled);
    assert_eq!(
        corrected.observability().compression,
        LangfuseCompression::Gzip
    );
    assert!(!Arc::ptr_eq(&published, &corrected));
    assert!(Arc::ptr_eq(
        &corrected,
        &system.current(&scope).expect("新快照已发布")
    ));
}

/// 局部 fixture：固定返回同一份输入，不做真实 IO；只在失败与修正路径上改写输入。
#[derive(Default)]
struct FixedSource {
    inputs: Mutex<ConfigurationInputs>,
}

impl FixedSource {
    fn new(inputs: ConfigurationInputs) -> Arc<Self> {
        Arc::new(Self {
            inputs: Mutex::new(inputs),
        })
    }

    fn mutate(&self, mutate: impl FnOnce(&mut ConfigurationInputs)) {
        mutate(&mut self.inputs.lock().unwrap());
    }
}

impl ConfigurationSource for FixedSource {
    fn collect(&self, _scope: &ConfigurationScope) -> io::Result<ConfigurationInputs> {
        Ok(self.inputs.lock().unwrap().clone())
    }

    fn write_if_unchanged(
        &self,
        _path: &Path,
        _expected: Option<&str>,
        _content: &str,
    ) -> io::Result<bool> {
        Ok(false)
    }
}
