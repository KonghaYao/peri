use serde::{Deserialize, Serialize};

// ─── OTLP (OpenTelemetry Protocol) Types ───────────────────────────
// These types represent the OTLP HTTP/JSON payload for trace ingestion.
// Endpoint: POST /api/public/otel/v1/traces
// Spec: https://opentelemetry.io/docs/specs/otlp/

/// OTLP trace export request body
///
/// 以下 resource/scope/request 包装类型只由 `types::conversion::ingestion_events_to_otel`
/// 物化回归使用（契约 §3.5）：非测试构建不构造，故与物化入口同一 `cfg(test)` 门控。
#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtelTraceExportRequest {
    #[serde(rename = "resourceSpans")]
    pub resource_spans: Vec<OtelResourceSpan>,
}

/// A collection of spans from a single resource
#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OtelResourceSpan {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<OtelResource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_spans: Option<Vec<OtelScopeSpan>>,
}

/// Resource attributes identifying the source of telemetry
#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtelResource {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Vec<OtelAttribute>>,
}

/// Collection of spans from a single instrumentation scope
#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OtelScopeSpan {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<OtelScope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spans: Option<Vec<OtelSpan>>,
}

/// Instrumentation scope information
#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtelScope {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Vec<OtelAttribute>>,
}

/// Individual OTLP span representing a unit of work
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OtelSpan {
    /// Trace ID — 16 bytes hex-encoded (32 chars), must NOT contain hyphens
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Span ID — 8 bytes hex-encoded (16 chars)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
    /// Parent span ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
    /// Span name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Span kind: 1=INTERNAL, 2=SERVER, 3=CLIENT, 4=PRODUCER, 5=CONSUMER
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<i32>,
    /// Start time in nanoseconds since Unix epoch (string representation)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time_unix_nano: Option<String>,
    /// End time in nanoseconds since Unix epoch (string representation)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time_unix_nano: Option<String>,
    /// Span attributes (langfuse.* namespace for Langfuse-specific mapping)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<Vec<OtelAttribute>>,
    /// Span status
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<OtelStatus>,
}

/// Span status
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OtelStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Key-value attribute pair
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtelAttribute {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<OtelAttributeValue>,
}

/// Attribute value wrapper supporting different value types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OtelAttributeValue {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub string_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub int_value: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub double_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bool_value: Option<bool>,
}
