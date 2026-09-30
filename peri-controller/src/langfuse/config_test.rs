//! Tests for config_lf

use super::*;
use serial_test::serial;

#[test]
#[serial]
fn test_batch_budgets_load_from_settings_with_environment_precedence() {
    clear_langfuse_env();
    let settings = serde_json::json!({"langfuse": {
        "batch_queue_capacity": 200,
        "batch_max_in_flight": 4,
        "batch_max_event_bytes": 1000,
        "batch_max_bytes": 4000,
        "batch_max_queue_bytes": 16000
    }});
    let configured = LangfuseConfig::load_with_settings(&settings);
    assert_eq!(configured.batch_queue_capacity, 200);
    assert_eq!(configured.batch_max_in_flight, 4);
    assert_eq!(configured.batch_max_event_bytes, 1000);
    assert_eq!(configured.batch_max_bytes, 4000);
    assert_eq!(configured.batch_max_queue_bytes, 16000);
    std::env::set_var("LANGFUSE_BATCH_QUEUE_CAPACITY", "300");
    std::env::set_var("LANGFUSE_BATCH_MAX_IN_FLIGHT", "3");
    let overridden = LangfuseConfig::load_with_settings(&settings);
    assert_eq!(overridden.batch_queue_capacity, 300);
    assert_eq!(overridden.batch_max_in_flight, 3);
    clear_langfuse_env();
}

fn clear_langfuse_env() {
    std::env::remove_var("LANGFUSE_PUBLIC_KEY");
    std::env::remove_var("LANGFUSE_SECRET_KEY");
    std::env::remove_var("LANGFUSE_BASE_URL");
    std::env::remove_var("LANGFUSE_TRACE_SAMPLING");
    std::env::remove_var("LANGFUSE_ERROR_SPAN_ALWAYS");
    std::env::remove_var("LANGFUSE_BATCH_MAX_EVENTS");
    std::env::remove_var("LANGFUSE_BATCH_QUEUE_CAPACITY");
    std::env::remove_var("LANGFUSE_BATCH_MAX_IN_FLIGHT");
    std::env::remove_var("LANGFUSE_BATCH_MAX_EVENT_BYTES");
    std::env::remove_var("LANGFUSE_BATCH_MAX_BYTES");
    std::env::remove_var("LANGFUSE_BATCH_MAX_QUEUE_BYTES");
    std::env::remove_var("LANGFUSE_BATCH_FLUSH_INTERVAL");
    std::env::remove_var("LANGFUSE_USER_ID");
}

#[test]
fn test_default_config() {
    let cfg = LangfuseConfig::default();
    assert!(cfg.public_key.is_none());
    assert!(cfg.secret_key.is_none());
    assert_eq!(cfg.host, "https://cloud.langfuse.com");
    assert!((cfg.trace_sampling - 1.0).abs() < 1e-10);
    assert!(cfg.error_span_always);
    assert_eq!(cfg.batch_max_events, 50);
    assert_eq!(cfg.batch_queue_capacity, 1024);
    assert_eq!(cfg.batch_max_in_flight, 2);
    assert_eq!(cfg.batch_flush_interval_secs, 10);
}

#[test]
#[serial]
fn test_load_with_settings_defaults() {
    clear_langfuse_env();
    let cfg = LangfuseConfig::load_with_settings(&serde_json::json!({}));
    assert!(cfg.public_key.is_none());
    assert!((cfg.trace_sampling - 1.0).abs() < 1e-10);
}

#[test]
#[serial]
fn test_load_with_settings_langfuse_fields() {
    clear_langfuse_env();
    let cfg = LangfuseConfig::load_with_settings(&serde_json::json!({
        "langfuse": {
            "trace_sampling": 0.3,
            "error_span_always": false,
            "batch_max_events": 100,
            "batch_flush_interval_secs": 30
        }
    }));
    assert!((cfg.trace_sampling - 0.3).abs() < 1e-10);
    assert!(!cfg.error_span_always);
    assert_eq!(cfg.batch_max_events, 100);
    assert_eq!(cfg.batch_flush_interval_secs, 30);
}

#[test]
#[serial]
fn test_load_with_settings_env_override() {
    clear_langfuse_env();
    // 设置环境变量后，settings.json 的值被覆盖
    std::env::set_var("LANGFUSE_TRACE_SAMPLING", "0.7");
    std::env::set_var("LANGFUSE_ERROR_SPAN_ALWAYS", "true");
    let cfg = LangfuseConfig::load_with_settings(&serde_json::json!({
        "langfuse": {
            "trace_sampling": 0.3,
            "error_span_always": false
        }
    }));
    assert!((cfg.trace_sampling - 0.7).abs() < 1e-10);
    assert!(cfg.error_span_always);
    // 清理环境变量
    clear_langfuse_env();
}

#[test]
#[serial]
fn test_load_with_settings_clamp_sampling() {
    clear_langfuse_env();
    let cfg = LangfuseConfig::load_with_settings(&serde_json::json!({
        "langfuse": { "trace_sampling": 2.5 }
    }));
    assert!((cfg.trace_sampling - 1.0).abs() < 1e-10);
}

#[test]
fn test_default_user_id_none() {
    let cfg = LangfuseConfig::default();
    assert!(cfg.user_id.is_none());
}

#[test]
#[serial]
fn test_user_id_from_env() {
    clear_langfuse_env();
    std::env::set_var("LANGFUSE_USER_ID", "env-user");
    let cfg = LangfuseConfig::load_with_settings(&serde_json::json!({}));
    assert_eq!(cfg.user_id.as_deref(), Some("env-user"));
    clear_langfuse_env();
}
