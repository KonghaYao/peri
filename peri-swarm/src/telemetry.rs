//! OpenTelemetry traces, metrics and logs for the gateway (OTLP/HTTP).
//!
//! The gateway follows the standard OTel environment surface
//! (`OTEL_EXPORTER_OTLP_*`, `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES`,
//! `OTEL_TRACES_SAMPLER`). Export is **off by default**: with no endpoint (or
//! `OTEL_SDK_DISABLED=true`) [`init`] returns a no-op guard that builds no
//! provider, opens no connection and starts no background task.
//!
//! Providers are local to this process and handed to the `tracing` layer
//! explicitly; the OTel global tracer/meter providers are never installed, so
//! a second telemetry stack cannot observe the gateway by accident. The guard
//! owns the three providers and flushes them on drop with a bounded wait;
//! `main` keeps it alive until `run()` has drained the gateway and closed both
//! stores.
//!
//! Access logs are emitted directly through the local OTel logger (see
//! [`HttpTelemetry::log_access`]); the `tracing` layer only carries spans from
//! the `peri_swarm::telemetry` target, so unrelated `tracing` events never
//! become OTel log records.
//!
//! The 0.28 logs SDK is used as-is. It is marked experimental in the upstream
//! specification, but its batch log processor is complete enough here and
//! keeps the OTLP protobuf wire format shared with traces and metrics; the
//! alternative JSON sender was therefore not needed.

use std::sync::Arc;
use std::time::Duration;

use axum::http::{HeaderName, HeaderValue};

use opentelemetry::logs::{AnyValue, LogRecord, Logger, LoggerProvider, Severity};
use opentelemetry::metrics::{Counter, Histogram, Meter, MeterProvider};
use opentelemetry::trace::{Span as _, SpanContext, SpanKind, Tracer as _, TracerProvider as _};
use opentelemetry::{Context, KeyValue};
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::logs::log_processor_with_async_runtime::BatchLogProcessor;
use opentelemetry_sdk::logs::{BatchConfigBuilder, SdkLogger, SdkLoggerProvider};
use opentelemetry_sdk::metrics::periodic_reader_with_async_runtime::PeriodicReader;
use opentelemetry_sdk::metrics::{SdkMeterProvider, Temporality};
use opentelemetry_sdk::resource::Resource;
use opentelemetry_sdk::runtime::Tokio;
use opentelemetry_sdk::trace::span_processor_with_async_runtime::BatchSpanProcessor;
use opentelemetry_sdk::trace::{SdkTracer, SdkTracerProvider};

/// Default `service.name` when neither `OTEL_SERVICE_NAME` nor a resource
/// attribute names one.
pub const DEFAULT_SERVICE_NAME: &str = "peri-swarm";
/// Default `RUST_LOG` filter, unchanged from the pre-telemetry entry point.
pub const DEFAULT_LOG_FILTER: &str = "peri_swarm=info,warn";
/// Targets that may become OTel spans through the `tracing` layer.
pub const TELEMETRY_TARGET_PREFIX: &str = "peri_swarm::telemetry";
/// Target used by the optional turn span.
pub const TURN_SPAN_TARGET: &str = "peri_swarm::telemetry::turn";
/// Access-log event name and record target.
pub const ACCESS_LOG_EVENT: &str = "peri_swarm.http.access";
pub const ACCESS_LOG_TARGET: &str = "peri_swarm.access";
/// Metric names exported for HTTP server traffic.
pub const HTTP_REQUESTS_METRIC: &str = "peri_swarm.http.server.requests";
pub const HTTP_DURATION_METRIC: &str = "peri_swarm.http.server.duration";

/// Explicit bucket boundaries in seconds for [`HTTP_DURATION_METRIC`].
///
/// The SDK default boundaries are the OpenTelemetry millisecond defaults
/// (`0, 5, 10, 25, ...`). Recording seconds into them puts every sub-5-second
/// request into one bucket, so a p95 computed from the histogram is meaningless.
/// These Prometheus-style second boundaries keep sub-second resolution.
pub const HTTP_DURATION_BOUNDARIES_SECONDS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

pub const ENV_SDK_DISABLED: &str = "OTEL_SDK_DISABLED";
pub const ENV_ENDPOINT: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";
pub const ENV_HEADERS: &str = "OTEL_EXPORTER_OTLP_HEADERS";
pub const ENV_TRACES_ENDPOINT: &str = "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT";
pub const ENV_TRACES_HEADERS: &str = "OTEL_EXPORTER_OTLP_TRACES_HEADERS";
pub const ENV_METRICS_ENDPOINT: &str = "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT";
pub const ENV_METRICS_HEADERS: &str = "OTEL_EXPORTER_OTLP_METRICS_HEADERS";
pub const ENV_LOGS_ENDPOINT: &str = "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT";
pub const ENV_LOGS_HEADERS: &str = "OTEL_EXPORTER_OTLP_LOGS_HEADERS";
pub const ENV_SERVICE_NAME: &str = "OTEL_SERVICE_NAME";
pub const ENV_RESOURCE_ATTRIBUTES: &str = "OTEL_RESOURCE_ATTRIBUTES";
pub const ENV_TRACES_SAMPLER: &str = "OTEL_TRACES_SAMPLER";
pub const ENV_TRACES_SAMPLER_ARG: &str = "OTEL_TRACES_SAMPLER_ARG";
pub const ENV_BSP_SCHEDULE_DELAY: &str = "OTEL_BSP_SCHEDULE_DELAY";
pub const ENV_BLRP_SCHEDULE_DELAY: &str = "OTEL_BLRP_SCHEDULE_DELAY";
pub const ENV_METRIC_EXPORT_INTERVAL: &str = "OTEL_METRIC_EXPORT_INTERVAL";
/// Not an OTel variable: the machine host name is used only for the local
/// `service.instance.id`. `PERI_SWARM_MACHINE_ID` is deliberately never read:
/// both gateway replicas share it, so it cannot identify one process.
pub const ENV_HOSTNAME: &str = "HOSTNAME";

const EXPORT_TIMEOUT: Duration = Duration::from_secs(10);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_BATCH_DELAY: Duration = Duration::from_secs(5);
const DEFAULT_METRIC_INTERVAL: Duration = Duration::from_secs(60);
const INSTRUMENTATION_SCOPE: &str = "peri-swarm";

/// One signal's OTLP/HTTP destination.
#[derive(Clone, Default)]
pub struct SignalConfig {
    /// Full OTLP/HTTP URL including the signal path for a generic endpoint, or
    /// exactly the signal-specific endpoint when one was configured.
    pub endpoint: Option<String>,
    pub headers: Vec<(String, String)>,
}

impl std::fmt::Debug for SignalConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Endpoints may carry userinfo and header values are credentials.
        formatter
            .debug_struct("SignalConfig")
            .field("configured", &self.endpoint.is_some())
            .field("header_count", &self.headers.len())
            .finish()
    }
}

/// Trace sampler selected by `OTEL_TRACES_SAMPLER`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum SamplerConfig {
    #[default]
    ParentBasedAlwaysOn,
    ParentBasedAlwaysOff,
    AlwaysOn,
    AlwaysOff,
    TraceIdRatio(f64),
    ParentBasedTraceIdRatio(f64),
}

/// Parsed telemetry configuration. Values are resolved once at process start;
/// the exporter builders are then given explicit endpoints so tests can inject
/// a local collector without mutating the process environment.
#[derive(Clone)]
pub struct TelemetryConfig {
    pub disabled: bool,
    /// `OTEL_SERVICE_NAME`; resource attributes are the fallback.
    pub service_name: Option<String>,
    pub resource_attributes: Vec<(String, String)>,
    pub traces: SignalConfig,
    pub metrics: SignalConfig,
    pub logs: SignalConfig,
    pub sampler: SamplerConfig,
    pub trace_batch_delay: Duration,
    pub log_batch_delay: Duration,
    pub metric_export_interval: Duration,
    /// Host component of `service.instance.id`; `None` renders `unknown`.
    pub hostname: Option<String>,
    /// Process component of `service.instance.id`.
    pub pid: u32,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            disabled: false,
            service_name: None,
            resource_attributes: Vec::new(),
            traces: SignalConfig::default(),
            metrics: SignalConfig::default(),
            logs: SignalConfig::default(),
            sampler: SamplerConfig::default(),
            trace_batch_delay: DEFAULT_BATCH_DELAY,
            log_batch_delay: DEFAULT_BATCH_DELAY,
            metric_export_interval: DEFAULT_METRIC_INTERVAL,
            hostname: std::env::var(ENV_HOSTNAME)
                .ok()
                .filter(|value| !value.trim().is_empty()),
            pid: std::process::id(),
        }
    }
}

impl std::fmt::Debug for TelemetryConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TelemetryConfig")
            .field("disabled", &self.disabled)
            .field("service_name_configured", &self.service_name.is_some())
            .field("resource_attribute_count", &self.resource_attributes.len())
            .field("traces", &self.traces)
            .field("metrics", &self.metrics)
            .field("logs", &self.logs)
            .field("sampler", &self.sampler)
            .field("trace_batch_delay", &self.trace_batch_delay)
            .field("log_batch_delay", &self.log_batch_delay)
            .field("metric_export_interval", &self.metric_export_interval)
            .field("hostname_configured", &self.hostname.is_some())
            .finish()
    }
}

impl TelemetryConfig {
    /// Parse the process environment.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_env_map(|name| std::env::var(name).ok())
    }

    /// Parse from an arbitrary lookup so tests and CLI overrides share one
    /// path. An empty value is treated as unset.
    #[must_use]
    pub fn from_env_map<F>(lookup: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let disabled =
            lookup(ENV_SDK_DISABLED).is_some_and(|value| value.trim().eq_ignore_ascii_case("true"));
        let service_name = non_empty(lookup(ENV_SERVICE_NAME));
        let resource_attributes = parse_key_value_list(lookup(ENV_RESOURCE_ATTRIBUTES).as_deref());
        let sampler = parse_sampler(
            lookup(ENV_TRACES_SAMPLER).as_deref(),
            lookup(ENV_TRACES_SAMPLER_ARG).as_deref(),
        );
        let trace_batch_delay = parse_millis(
            lookup(ENV_BSP_SCHEDULE_DELAY).as_deref(),
            DEFAULT_BATCH_DELAY,
        );
        let log_batch_delay = parse_millis(
            lookup(ENV_BLRP_SCHEDULE_DELAY).as_deref(),
            DEFAULT_BATCH_DELAY,
        );
        let metric_export_interval = parse_millis(
            lookup(ENV_METRIC_EXPORT_INTERVAL).as_deref(),
            DEFAULT_METRIC_INTERVAL,
        );

        let (traces, metrics, logs) = if disabled {
            // Disabled means disabled: never build an exporter from the rest
            // of the environment.
            (
                SignalConfig::default(),
                SignalConfig::default(),
                SignalConfig::default(),
            )
        } else {
            let endpoint = non_empty(lookup(ENV_ENDPOINT));
            let headers = parse_key_value_list(lookup(ENV_HEADERS).as_deref());
            (
                signal_config(
                    endpoint.as_deref(),
                    lookup(ENV_TRACES_ENDPOINT).as_deref(),
                    "traces",
                    &headers,
                    lookup(ENV_TRACES_HEADERS).as_deref(),
                ),
                signal_config(
                    endpoint.as_deref(),
                    lookup(ENV_METRICS_ENDPOINT).as_deref(),
                    "metrics",
                    &headers,
                    lookup(ENV_METRICS_HEADERS).as_deref(),
                ),
                signal_config(
                    endpoint.as_deref(),
                    lookup(ENV_LOGS_ENDPOINT).as_deref(),
                    "logs",
                    &headers,
                    lookup(ENV_LOGS_HEADERS).as_deref(),
                ),
            )
        };
        Self {
            disabled,
            service_name,
            resource_attributes,
            traces,
            metrics,
            logs,
            sampler,
            trace_batch_delay,
            log_batch_delay,
            metric_export_interval,
            hostname: lookup(ENV_HOSTNAME).filter(|value| !value.trim().is_empty()),
            pid: std::process::id(),
        }
    }

    /// True when at least one signal has an export endpoint.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        !self.disabled
            && (self.traces.endpoint.is_some()
                || self.metrics.endpoint.is_some()
                || self.logs.endpoint.is_some())
    }

    /// Build the resource shared by all three providers.
    ///
    /// `service.instance.id` is always `HOSTNAME:pid` (host name `unknown`
    /// when unset). A machine-wide deployment id is never used: two replicas of
    /// the gateway share it and would be indistinguishable.
    #[must_use]
    pub fn resource(&self) -> Resource {
        let mut service_name = None;
        let mut service_version = None;
        let mut attributes = Vec::new();
        for (key, value) in &self.resource_attributes {
            match key.as_str() {
                "service.name" => service_name = Some(value.clone()),
                "service.version" => service_version = Some(value.clone()),
                // Always local; ignore a shared value from the environment.
                "service.instance.id" => {}
                _ => attributes.push(KeyValue::new(key.clone(), value.clone())),
            }
        }
        let service_name = self
            .service_name
            .clone()
            .filter(|value| !value.trim().is_empty())
            .or(service_name)
            .unwrap_or_else(|| DEFAULT_SERVICE_NAME.to_owned());
        let service_version =
            service_version.unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned());
        let hostname = self
            .hostname
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("unknown");
        Resource::builder_empty()
            .with_attributes(attributes)
            .with_service_name(service_name)
            .with_attribute(KeyValue::new("service.version", service_version))
            .with_attribute(KeyValue::new(
                "service.instance.id",
                format!("{hostname}:{}", self.pid),
            ))
            .build()
    }
}

/// Owns the three local providers and flushes them on drop.
pub struct TelemetryGuard {
    inner: Option<TelemetryInner>,
    warnings: Vec<String>,
}

struct TelemetryInner {
    tracer: Option<SdkTracer>,
    tracer_provider: Option<SdkTracerProvider>,
    meter_provider: Option<SdkMeterProvider>,
    logger_provider: Option<SdkLoggerProvider>,
    http: Option<HttpTelemetry>,
}

impl TelemetryGuard {
    /// Build providers from an explicit configuration. Must run inside a Tokio
    /// runtime: the async-runtime processors spawn their workers there.
    #[must_use]
    pub fn start(config: TelemetryConfig) -> Self {
        if !config.is_enabled() {
            return Self {
                inner: None,
                warnings: Vec::new(),
            };
        }

        let resource = config.resource();
        let mut warnings = Vec::new();

        let mut tracer = None;
        let mut tracer_provider = None;
        if let Some(endpoint) = config.traces.endpoint.as_deref() {
            match build_tracer_provider(&config, endpoint, &resource) {
                Ok(provider) => {
                    tracer = Some(provider.tracer(INSTRUMENTATION_SCOPE));
                    tracer_provider = Some(provider);
                }
                Err(()) => warnings.push(
                    "traces export disabled: the OTLP trace exporter could not be initialized"
                        .to_owned(),
                ),
            }
        }

        let mut meter = None;
        let mut meter_provider = None;
        if let Some(endpoint) = config.metrics.endpoint.as_deref() {
            match build_meter_provider(&config, endpoint, &resource) {
                Ok(provider) => {
                    meter = Some(provider.meter(INSTRUMENTATION_SCOPE));
                    meter_provider = Some(provider);
                }
                Err(()) => warnings.push(
                    "metrics export disabled: the OTLP metric exporter could not be initialized"
                        .to_owned(),
                ),
            }
        }

        let mut logger = None;
        let mut logger_provider = None;
        if let Some(endpoint) = config.logs.endpoint.as_deref() {
            match build_logger_provider(&config, endpoint, &resource) {
                Ok(provider) => {
                    logger = Some(provider.logger(INSTRUMENTATION_SCOPE));
                    logger_provider = Some(provider);
                }
                Err(()) => warnings.push(
                    "logs export disabled: the OTLP log exporter could not be initialized"
                        .to_owned(),
                ),
            }
        }

        let http = HttpTelemetry::new(tracer.clone(), meter, logger);
        let inner =
            if tracer_provider.is_some() || meter_provider.is_some() || logger_provider.is_some() {
                Some(TelemetryInner {
                    tracer,
                    tracer_provider,
                    meter_provider,
                    logger_provider,
                    http,
                })
            } else {
                None
            };
        Self { inner, warnings }
    }

    /// Span tracer for the `tracing` layer, when trace export is enabled.
    #[must_use]
    pub fn tracer(&self) -> Option<SdkTracer> {
        self.inner.as_ref()?.tracer.clone()
    }

    /// Handle used by the HTTP middleware.
    #[must_use]
    pub fn http_telemetry(&self) -> Option<HttpTelemetry> {
        self.inner.as_ref()?.http.clone()
    }

    /// Flush all providers. Best effort: exporter errors are not logged (their
    /// text may contain the collector URL or response body).
    pub fn force_flush(&self) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        if let Some(provider) = inner.tracer_provider.as_ref() {
            let _ = provider.force_flush();
        }
        if let Some(provider) = inner.meter_provider.as_ref() {
            let _ = provider.force_flush();
        }
        if let Some(provider) = inner.logger_provider.as_ref() {
            let _ = provider.force_flush();
        }
    }

    /// Non-secret build warnings (for example an invalid endpoint).
    #[must_use]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Print non-secret build warnings through `tracing` (after the subscriber
    /// exists, so a bad endpoint is still diagnosable).
    pub fn emit_warnings(&self) {
        for warning in &self.warnings {
            tracing::warn!(target: TELEMETRY_TARGET_PREFIX, "{warning}");
        }
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        let Some(inner) = self.inner.take() else {
            return;
        };
        // Provider shutdown blocks on the exporter; run it off the async thread
        // and bound the wait so a dead collector cannot hold the process open.
        let (sender, receiver) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("otel-shutdown".to_owned())
            .spawn(move || {
                inner.shutdown();
                let _ = sender.send(());
            });
        if spawned.is_ok() {
            let _ = receiver.recv_timeout(SHUTDOWN_TIMEOUT);
        }
    }
}

impl TelemetryInner {
    fn shutdown(self) {
        // Order is not significant; each provider flushes its pending batch.
        if let Some(provider) = self.tracer_provider {
            let _ = provider.shutdown();
        }
        if let Some(provider) = self.meter_provider {
            let _ = provider.shutdown();
        }
        if let Some(provider) = self.logger_provider {
            let _ = provider.shutdown();
        }
    }
}

/// Install the process subscriber. With telemetry enabled the `tracing` OTel
/// layer only sees spans under [`TELEMETRY_TARGET_PREFIX`]; the fmt layer keeps
/// the existing `EnvFilter` behaviour. With telemetry disabled the exact
/// pre-stage-3 `fmt` subscriber is installed.
pub fn init() -> TelemetryGuard {
    let guard = TelemetryGuard::start(TelemetryConfig::from_env());
    init_subscriber(guard.tracer());
    guard.emit_warnings();
    guard
}

fn init_subscriber(tracer: Option<SdkTracer>) {
    use tracing_subscriber::filter::filter_fn;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::Layer;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(DEFAULT_LOG_FILTER));
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_filter(filter);
    match tracer {
        Some(tracer) => {
            let telemetry_layer = tracing_opentelemetry::layer()
                .with_tracer(tracer)
                .with_filter(filter_fn(|metadata| {
                    metadata.target().starts_with(TELEMETRY_TARGET_PREFIX)
                }));
            let _ = tracing_subscriber::registry()
                .with(fmt_layer)
                .with(telemetry_layer)
                .try_init();
        }
        None => {
            let _ = tracing_subscriber::registry().with(fmt_layer).try_init();
        }
    }
}

/// Recording handle for the HTTP middleware. Construction is cheap; a no-op
/// build yields `None` and the router never installs the middleware.
#[derive(Clone)]
pub struct HttpTelemetry {
    inner: Arc<HttpTelemetryInner>,
}

struct HttpTelemetryInner {
    tracer: Option<SdkTracer>,
    requests: Option<Counter<u64>>,
    duration: Option<Histogram<f64>>,
    logger: Option<SdkLogger>,
}

impl HttpTelemetry {
    fn new(
        tracer: Option<SdkTracer>,
        meter: Option<Meter>,
        logger: Option<SdkLogger>,
    ) -> Option<Self> {
        let (requests, duration) = match meter {
            Some(meter) => (
                Some(
                    meter
                        .u64_counter(HTTP_REQUESTS_METRIC)
                        .with_unit("{request}")
                        .with_description("HTTP server requests handled by the gateway")
                        .build(),
                ),
                Some(
                    meter
                        .f64_histogram(HTTP_DURATION_METRIC)
                        .with_unit("s")
                        .with_description("HTTP server request duration in seconds")
                        .with_boundaries(HTTP_DURATION_BOUNDARIES_SECONDS.to_vec())
                        .build(),
                ),
            ),
            None => (None, None),
        };
        if tracer.is_none() && requests.is_none() && logger.is_none() {
            return None;
        }
        Some(Self {
            inner: Arc::new(HttpTelemetryInner {
                tracer,
                requests,
                duration,
                logger,
            }),
        })
    }

    pub(crate) fn start_span(
        &self,
        method: &str,
        route: &str,
        session_id: Option<&str>,
    ) -> Option<HttpSpan> {
        let tracer = self.inner.tracer.as_ref()?;
        let mut attributes = vec![
            KeyValue::new("http.request.method", method.to_owned()),
            KeyValue::new("http.route", route.to_owned()),
        ];
        if let Some(session_id) = session_id {
            attributes.push(KeyValue::new("session.id", session_id.to_owned()));
        }
        let builder = tracer
            .span_builder(format!("{method} {route}"))
            .with_kind(SpanKind::Server)
            .with_attributes(attributes);
        let span = tracer.build_with_context(builder, &Context::new());
        if !span.span_context().is_valid() {
            return None;
        }
        Some(HttpSpan {
            context: span.span_context().clone(),
            span,
        })
    }

    pub(crate) fn record_request(
        &self,
        method: &str,
        route: &str,
        status: u16,
        elapsed: Duration,
        record_duration: bool,
    ) {
        let attributes = [
            KeyValue::new("method", method.to_owned()),
            KeyValue::new("route", route.to_owned()),
            KeyValue::new("status", i64::from(status)),
        ];
        if let Some(counter) = self.inner.requests.as_ref() {
            counter.add(1, &attributes);
        }
        if record_duration {
            if let Some(histogram) = self.inner.duration.as_ref() {
                histogram.record(elapsed.as_secs_f64(), &attributes);
            }
        }
    }

    pub(crate) fn log_access(&self, entry: &AccessLog<'_>, trace: Option<&SpanContext>) {
        let Some(logger) = self.inner.logger.as_ref() else {
            return;
        };
        let mut record = logger.create_log_record();
        record.set_event_name(ACCESS_LOG_EVENT);
        record.set_target(ACCESS_LOG_TARGET);
        record.set_severity_number(Severity::Info);
        record.set_severity_text("INFO");
        record.set_body(AnyValue::String("http request completed".into()));
        record.add_attribute("method", entry.method.to_owned());
        record.add_attribute("route", entry.route.to_owned());
        record.add_attribute("status", i64::from(entry.status));
        record.add_attribute(
            "duration_ms",
            i64::try_from(entry.duration_ms).unwrap_or(i64::MAX),
        );
        if let Some(session_id) = entry.session_id {
            record.add_attribute("session_id", session_id.to_owned());
        }
        record.add_attribute("request_id", entry.request_id.to_owned());
        if let Some(trace) = trace {
            record.set_trace_context(trace.trace_id(), trace.span_id(), Some(trace.trace_flags()));
        }
        logger.emit(record);
    }
}

/// Span plus its context; the middleware ends it once response headers exist.
pub(crate) struct HttpSpan {
    span: opentelemetry_sdk::trace::Span,
    context: SpanContext,
}

impl HttpSpan {
    pub(crate) fn context(&self) -> &SpanContext {
        &self.context
    }

    pub(crate) fn end(mut self, status: u16) {
        self.span.set_attribute(KeyValue::new(
            "http.response.status_code",
            i64::from(status),
        ));
        self.span.end();
    }
}

/// One access-log line. Only these fields are recorded: no headers, query
/// strings, bodies, authorization, cookies or idempotency keys.
pub(crate) struct AccessLog<'a> {
    pub method: &'a str,
    pub route: &'a str,
    pub status: u16,
    pub duration_ms: u64,
    pub session_id: Option<&'a str>,
    pub request_id: &'a str,
}

fn build_tracer_provider(
    config: &TelemetryConfig,
    endpoint: &str,
    resource: &Resource,
) -> Result<SdkTracerProvider, ()> {
    let endpoint = validate_endpoint(endpoint)?;
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_http_client(TelemetryHttpClient::new(&config.traces.headers))
        .with_endpoint(endpoint)
        .with_timeout(EXPORT_TIMEOUT)
        .build()
        .map_err(|_| ())?;
    let batch = BatchSpanProcessor::builder(exporter, Tokio)
        .with_batch_config(
            opentelemetry_sdk::trace::BatchConfigBuilder::default()
                .with_scheduled_delay(config.trace_batch_delay)
                .build(),
        )
        .build();
    Ok(SdkTracerProvider::builder()
        .with_resource(resource.clone())
        .with_sampler(config.sampler.to_sdk())
        .with_span_processor(batch)
        .build())
}

fn build_meter_provider(
    config: &TelemetryConfig,
    endpoint: &str,
    resource: &Resource,
) -> Result<SdkMeterProvider, ()> {
    let endpoint = validate_endpoint(endpoint)?;
    let exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_http_client(TelemetryHttpClient::new(&config.metrics.headers))
        .with_temporality(Temporality::Cumulative)
        .with_endpoint(endpoint)
        .with_timeout(EXPORT_TIMEOUT)
        .build()
        .map_err(|_| ())?;
    let reader = PeriodicReader::builder(exporter, Tokio)
        .with_interval(config.metric_export_interval)
        .build();
    Ok(SdkMeterProvider::builder()
        .with_resource(resource.clone())
        .with_reader(reader)
        .build())
}

fn build_logger_provider(
    config: &TelemetryConfig,
    endpoint: &str,
    resource: &Resource,
) -> Result<SdkLoggerProvider, ()> {
    let endpoint = validate_endpoint(endpoint)?;
    let exporter = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_http_client(TelemetryHttpClient::new(&config.logs.headers))
        .with_endpoint(endpoint)
        .with_timeout(EXPORT_TIMEOUT)
        .build()
        .map_err(|_| ())?;
    let batch = BatchLogProcessor::builder(exporter, Tokio)
        .with_batch_config(
            BatchConfigBuilder::default()
                .with_scheduled_delay(config.log_batch_delay)
                .build(),
        )
        .build();
    Ok(SdkLoggerProvider::builder()
        .with_resource(resource.clone())
        .with_log_processor(batch)
        .build())
}

/// Async HTTP client shared by the three exporters.
///
/// A custom client is passed explicitly for two reasons: (1) 0.28's built-in
/// selection compiles neither the async nor the blocking branch when another
/// crate in the build enables `reqwest-blocking-client` alongside
/// `reqwest-client`; (2) its `with_headers` applies only the first explicit
/// header, so the parsed header list is injected on every request here instead.
/// Invalid header names/values are dropped, never rendered.
#[derive(Clone, Debug)]
struct TelemetryHttpClient {
    client: reqwest::Client,
    headers: Arc<[(HeaderName, HeaderValue)]>,
}

impl TelemetryHttpClient {
    fn new(headers: &[(String, String)]) -> Self {
        let headers: Vec<(HeaderName, HeaderValue)> = headers
            .iter()
            .filter_map(|(name, value)| {
                Some((
                    name.parse::<HeaderName>().ok()?,
                    value.parse::<HeaderValue>().ok()?,
                ))
            })
            .collect();
        Self {
            client: reqwest::Client::builder()
                .timeout(EXPORT_TIMEOUT)
                .build()
                .unwrap_or_default(),
            headers: Arc::from(headers),
        }
    }
}

#[async_trait::async_trait]
impl opentelemetry_http::HttpClient for TelemetryHttpClient {
    async fn send_bytes(
        &self,
        mut request: opentelemetry_http::Request<opentelemetry_http::Bytes>,
    ) -> Result<
        opentelemetry_http::Response<opentelemetry_http::Bytes>,
        opentelemetry_http::HttpError,
    > {
        for (name, value) in self.headers.iter() {
            request.headers_mut().insert(name.clone(), value.clone());
        }
        let mut response = self
            .client
            .execute(request.try_into()?)
            .await?
            .error_for_status()?;
        let headers = std::mem::take(response.headers_mut());
        let mut http_response = opentelemetry_http::Response::builder()
            .status(response.status())
            .body(response.bytes().await?)?;
        *http_response.headers_mut() = headers;
        Ok(http_response)
    }
}

/// Only plain HTTP(S) origins with a host. Userinfo is allowed (some
/// collectors use basic auth) but is never logged.
fn validate_endpoint(raw: &str) -> Result<String, ()> {
    let url = url::Url::parse(raw).map_err(|_| ())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(());
    }
    Ok(raw.to_owned())
}

impl SamplerConfig {
    fn to_sdk(self) -> opentelemetry_sdk::trace::Sampler {
        use opentelemetry_sdk::trace::Sampler;
        match self {
            Self::ParentBasedAlwaysOn => Sampler::ParentBased(Box::new(Sampler::AlwaysOn)),
            Self::ParentBasedAlwaysOff => Sampler::ParentBased(Box::new(Sampler::AlwaysOff)),
            Self::AlwaysOn => Sampler::AlwaysOn,
            Self::AlwaysOff => Sampler::AlwaysOff,
            Self::TraceIdRatio(ratio) => Sampler::TraceIdRatioBased(ratio),
            Self::ParentBasedTraceIdRatio(ratio) => {
                Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(ratio)))
            }
        }
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn parse_millis(value: Option<&str>, fallback: Duration) -> Duration {
    value
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map(Duration::from_millis)
        .unwrap_or(fallback)
}

fn parse_sampler(name: Option<&str>, argument: Option<&str>) -> SamplerConfig {
    let ratio = argument
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| (0.0..=1.0).contains(value))
        .unwrap_or(1.0);
    match name.map(str::trim) {
        Some("always_on") => SamplerConfig::AlwaysOn,
        Some("always_off") => SamplerConfig::AlwaysOff,
        Some("traceidratio") => SamplerConfig::TraceIdRatio(ratio),
        Some("parentbased_always_off") => SamplerConfig::ParentBasedAlwaysOff,
        Some("parentbased_traceidratio") => SamplerConfig::ParentBasedTraceIdRatio(ratio),
        // Unset or an unsupported sampler keeps the SDK default.
        _ => SamplerConfig::ParentBasedAlwaysOn,
    }
}

fn signal_config(
    generic_endpoint: Option<&str>,
    specific_endpoint: Option<&str>,
    signal_path: &str,
    generic_headers: &[(String, String)],
    specific_headers: Option<&str>,
) -> SignalConfig {
    let endpoint = non_empty(specific_endpoint.map(str::to_owned)).or_else(|| {
        generic_endpoint
            .map(|endpoint| format!("{}/v1/{signal_path}", endpoint.trim_end_matches('/')))
    });
    let mut headers = generic_headers.to_vec();
    merge_headers(&mut headers, parse_key_value_list(specific_headers));
    SignalConfig { endpoint, headers }
}

fn merge_headers(headers: &mut Vec<(String, String)>, extra: Vec<(String, String)>) {
    for (key, value) in extra {
        if let Some(existing) = headers
            .iter_mut()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(&key))
        {
            existing.1 = value;
        } else {
            headers.push((key, value));
        }
    }
}

/// Parse an OTel `key=value,key=value` list. Values are percent-decoded, as the
/// specification requires; an entry without `=` or with an empty key/value is
/// ignored.
fn parse_key_value_list(value: Option<&str>) -> Vec<(String, String)> {
    let Some(value) = value else {
        return Vec::new();
    };
    value
        .split(',')
        .filter_map(|entry| {
            let (key, value) = entry.split_once('=')?;
            let key = key.trim();
            let value = value.trim();
            if key.is_empty() || value.is_empty() {
                return None;
            }
            Some((key.to_owned(), percent_decode(value)))
        })
        .collect()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
            {
                decoded.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(decoded).unwrap_or_else(|_| value.to_owned())
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
#[path = "telemetry_test.rs"]
mod tests;
