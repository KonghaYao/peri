//! Deterministic telemetry configuration tests.
//!
//! Everything here goes through [`TelemetryConfig::from_env_map`] or an
//! explicitly mutated config: no test mutates the process environment, so the
//! default parallel test runner cannot race.

use std::collections::HashMap;
use std::time::Duration;

use opentelemetry::Key;

use super::{
    parse_key_value_list, validate_endpoint, SamplerConfig, SignalConfig, TelemetryConfig,
    DEFAULT_LOG_FILTER, DEFAULT_SERVICE_NAME,
};

const PID: u32 = 4242;

fn config_from(entries: &[(&str, &str)]) -> TelemetryConfig {
    let env: HashMap<String, String> = entries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    TelemetryConfig::from_env_map(|name| env.get(name).cloned())
}

fn attr(config: &TelemetryConfig, name: &str) -> String {
    config
        .resource()
        .get(&Key::new(name.to_owned()))
        .unwrap_or_else(|| panic!("missing resource attribute {name}"))
        .to_string()
}

fn header(config: &SignalConfig, name: &str) -> Option<String> {
    config
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

#[test]
fn no_endpoint_is_a_no_op_configuration() {
    let config = config_from(&[]);
    assert!(!config.disabled);
    assert!(!config.is_enabled());
    assert!(config.traces.endpoint.is_none());
    assert!(config.metrics.endpoint.is_none());
    assert!(config.logs.endpoint.is_none());
    assert!(config.traces.headers.is_empty());
    assert_eq!(config.trace_batch_delay, Duration::from_secs(5));
    assert_eq!(config.log_batch_delay, Duration::from_secs(5));
    assert_eq!(config.metric_export_interval, Duration::from_secs(60));
}

#[test]
fn sdk_disabled_ignores_configured_endpoints() {
    let config = config_from(&[
        ("OTEL_SDK_DISABLED", "true"),
        ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4318"),
    ]);
    assert!(config.disabled);
    assert!(!config.is_enabled());
    assert!(config.traces.endpoint.is_none());
    assert!(config.metrics.endpoint.is_none());
    assert!(config.logs.endpoint.is_none());
}

#[test]
fn generic_endpoint_appends_each_signal_path() {
    let config = config_from(&[("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4318/")]);
    assert!(config.is_enabled());
    assert_eq!(
        config.traces.endpoint.as_deref(),
        Some("http://collector:4318/v1/traces")
    );
    assert_eq!(
        config.metrics.endpoint.as_deref(),
        Some("http://collector:4318/v1/metrics")
    );
    assert_eq!(
        config.logs.endpoint.as_deref(),
        Some("http://collector:4318/v1/logs")
    );

    let config = config_from(&[("OTEL_EXPORTER_OTLP_ENDPOINT", "http://host/api/default")]);
    assert_eq!(
        config.traces.endpoint.as_deref(),
        Some("http://host/api/default/v1/traces")
    );
}

#[test]
fn signal_endpoint_overrides_the_generic_endpoint_unchanged() {
    let config = config_from(&[
        ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4318"),
        (
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            "http://trace:4318/custom",
        ),
    ]);
    assert_eq!(
        config.traces.endpoint.as_deref(),
        Some("http://trace:4318/custom")
    );
    assert_eq!(
        config.metrics.endpoint.as_deref(),
        Some("http://collector:4318/v1/metrics")
    );
}

#[test]
fn a_signal_endpoint_alone_enables_only_that_signal() {
    let config = config_from(&[(
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
        "http://trace:4318/v1/traces",
    )]);
    assert!(config.is_enabled());
    assert_eq!(
        config.traces.endpoint.as_deref(),
        Some("http://trace:4318/v1/traces")
    );
    assert!(config.metrics.endpoint.is_none());
    assert!(config.logs.endpoint.is_none());
}

#[test]
fn signal_headers_override_generic_headers() {
    let config = config_from(&[
        ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4318"),
        (
            "OTEL_EXPORTER_OTLP_HEADERS",
            "Authorization=Basic%20abc,stream-name=peri_swarm_logs",
        ),
        (
            "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
            "authorization=Bearer%20trace,extra=1",
        ),
    ]);
    assert_eq!(
        header(&config.traces, "authorization").as_deref(),
        Some("Bearer trace")
    );
    assert_eq!(
        header(&config.traces, "stream-name").as_deref(),
        Some("peri_swarm_logs")
    );
    assert_eq!(header(&config.traces, "extra").as_deref(), Some("1"));
    assert_eq!(
        header(&config.metrics, "authorization").as_deref(),
        Some("Basic abc")
    );
    assert_eq!(config.traces.headers.len(), 3);
}

#[test]
fn key_value_parsing_decodes_percent_escapes_and_drops_invalid_entries() {
    let parsed = parse_key_value_list(Some("a=1, b=two%20words , broken, =empty, c="));
    assert_eq!(
        parsed,
        vec![
            ("a".to_owned(), "1".to_owned()),
            ("b".to_owned(), "two words".to_owned()),
        ]
    );
    assert_eq!(
        parse_key_value_list(Some("Authorization=Basic%20Zm9v%3D")),
        vec![("Authorization".to_owned(), "Basic Zm9v=".to_owned())]
    );
    assert!(parse_key_value_list(None).is_empty());
}

#[test]
fn resource_defaults_to_the_gateway_identity() {
    let config = config_from(&[("HOSTNAME", "host-a")]);
    assert_eq!(attr(&config, "service.name"), DEFAULT_SERVICE_NAME);
    assert_eq!(attr(&config, "service.version"), env!("CARGO_PKG_VERSION"));
    assert_eq!(
        attr(&config, "service.instance.id"),
        format!("host-a:{}", config.pid)
    );
}

#[test]
fn resource_service_name_and_version_precedence() {
    let config = config_from(&[
        ("OTEL_SERVICE_NAME", "gateway-from-env"),
        (
            "OTEL_RESOURCE_ATTRIBUTES",
            "service.name=gateway-from-resource,service.version=9.9.9,deployment=blue",
        ),
    ]);
    assert_eq!(attr(&config, "service.name"), "gateway-from-env");
    assert_eq!(attr(&config, "service.version"), "9.9.9");
    assert_eq!(attr(&config, "deployment"), "blue");

    let config = config_from(&[("OTEL_RESOURCE_ATTRIBUTES", "service.name=resource-only")]);
    assert_eq!(attr(&config, "service.name"), "resource-only");
}

#[test]
fn instance_id_is_local_and_never_the_shared_machine_id() {
    let mut config = config_from(&[
        ("HOSTNAME", "host-a"),
        ("PERI_SWARM_MACHINE_ID", "shared-machine"),
        (
            "OTEL_RESOURCE_ATTRIBUTES",
            "service.instance.id=shared-machine,PERI_SWARM_MACHINE_ID=shared-machine",
        ),
    ]);
    config.pid = PID;
    let instance = attr(&config, "service.instance.id");
    assert_eq!(instance, format!("host-a:{PID}"));
    assert_ne!(instance, "shared-machine");

    let mut other = config.clone();
    other.pid = PID + 1;
    assert_ne!(
        attr(&config, "service.instance.id"),
        attr(&other, "service.instance.id")
    );
}

#[test]
fn instance_id_falls_back_to_unknown_without_hostname() {
    let mut config = config_from(&[]);
    config.hostname = None;
    config.pid = PID;
    assert_eq!(
        attr(&config, "service.instance.id"),
        format!("unknown:{PID}")
    );
}

#[test]
fn sampler_configuration_follows_the_standard_names() {
    assert_eq!(config_from(&[]).sampler, SamplerConfig::ParentBasedAlwaysOn);
    assert_eq!(
        config_from(&[("OTEL_TRACES_SAMPLER", "always_on")]).sampler,
        SamplerConfig::AlwaysOn
    );
    assert_eq!(
        config_from(&[("OTEL_TRACES_SAMPLER", "always_off")]).sampler,
        SamplerConfig::AlwaysOff
    );
    assert_eq!(
        config_from(&[
            ("OTEL_TRACES_SAMPLER", "traceidratio"),
            ("OTEL_TRACES_SAMPLER_ARG", "0.25"),
        ])
        .sampler,
        SamplerConfig::TraceIdRatio(0.25)
    );
    assert_eq!(
        config_from(&[
            ("OTEL_TRACES_SAMPLER", "parentbased_traceidratio"),
            ("OTEL_TRACES_SAMPLER_ARG", "not-a-number"),
        ])
        .sampler,
        SamplerConfig::ParentBasedTraceIdRatio(1.0)
    );
    assert_eq!(
        config_from(&[("OTEL_TRACES_SAMPLER", "parentbased_always_off")]).sampler,
        SamplerConfig::ParentBasedAlwaysOff
    );
    assert_eq!(
        config_from(&[("OTEL_TRACES_SAMPLER", "unknown")]).sampler,
        SamplerConfig::ParentBasedAlwaysOn
    );
}

#[test]
fn export_intervals_are_parsed_in_milliseconds() {
    let config = config_from(&[
        ("OTEL_BSP_SCHEDULE_DELAY", "1000"),
        ("OTEL_BLRP_SCHEDULE_DELAY", "2000"),
        ("OTEL_METRIC_EXPORT_INTERVAL", "3000"),
    ]);
    assert_eq!(config.trace_batch_delay, Duration::from_millis(1000));
    assert_eq!(config.log_batch_delay, Duration::from_millis(2000));
    assert_eq!(config.metric_export_interval, Duration::from_millis(3000));

    let config = config_from(&[
        ("OTEL_BSP_SCHEDULE_DELAY", "not-a-number"),
        ("OTEL_BLRP_SCHEDULE_DELAY", "0"),
        ("OTEL_METRIC_EXPORT_INTERVAL", "-1"),
    ]);
    assert_eq!(config.trace_batch_delay, Duration::from_secs(5));
    assert_eq!(config.log_batch_delay, Duration::from_secs(5));
    assert_eq!(config.metric_export_interval, Duration::from_secs(60));
}

#[test]
fn endpoints_must_be_plain_http_origins() {
    assert_eq!(
        validate_endpoint("http://collector:4318/v1/traces"),
        Ok("http://collector:4318/v1/traces".to_owned())
    );
    assert!(validate_endpoint("https://collector.example/v1/logs").is_ok());
    assert!(validate_endpoint("ftp://collector/v1/traces").is_err());
    assert!(validate_endpoint("http://collector/v1/traces?token=secret").is_err());
    assert!(validate_endpoint("http://collector/v1/traces#fragment").is_err());
    assert!(validate_endpoint("not a url").is_err());
}

#[test]
fn debug_output_never_contains_header_or_endpoint_values() {
    let config = config_from(&[
        (
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "http://user:endpoint-secret@collector:4318",
        ),
        (
            "OTEL_EXPORTER_OTLP_HEADERS",
            "Authorization=Basic%20header-secret",
        ),
    ]);
    let debug = format!("{config:?}");
    assert!(!debug.contains("header-secret"));
    assert!(!debug.contains("endpoint-secret"));
    assert!(!debug.contains("Basic"));
    assert!(debug.contains("header_count"));
}

#[test]
fn default_log_filter_is_unchanged() {
    assert_eq!(DEFAULT_LOG_FILTER, "peri_swarm=info,warn");
}

#[test]
fn duration_histogram_uses_second_scale_boundaries() {
    let bounds = super::HTTP_DURATION_BOUNDARIES_SECONDS;
    assert!(bounds.iter().all(|bound| bound.is_finite() && *bound > 0.0));
    assert!(bounds.windows(2).all(|pair| pair[0] < pair[1]));
    // Millisecond-scale request latency must land in a finite bucket instead
    // of the SDK default (0, 5] seconds bucket.
    assert!(bounds.iter().any(|bound| *bound <= 0.01));
    assert!(bounds.last().is_some_and(|bound| *bound >= 10.0));
}
