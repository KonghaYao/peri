mod conversion;
mod otlp;
pub mod session;

use std::collections::HashMap;

pub(crate) use conversion::ingestion_events_to_otel;
use serde::{Deserialize, Serialize};

// ─── IngestionEvent helpers ─────────────────────────────────────────

impl IngestionEvent {
    /// Get the event envelope timestamp (all variants have a `timestamp` field)
    pub fn event_timestamp(&self) -> &str {
        match self {
            IngestionEvent::TraceCreate { timestamp, .. } => timestamp,
            IngestionEvent::SpanCreate { timestamp, .. } => timestamp,
            IngestionEvent::SpanUpdate { timestamp, .. } => timestamp,
            IngestionEvent::GenerationCreate { timestamp, .. } => timestamp,
            IngestionEvent::GenerationUpdate { timestamp, .. } => timestamp,
            IngestionEvent::EventCreate { timestamp, .. } => timestamp,
            IngestionEvent::ScoreCreate { timestamp, .. } => timestamp,
            IngestionEvent::ObservationCreate { timestamp, .. } => timestamp,
            IngestionEvent::ObservationUpdate { timestamp, .. } => timestamp,
            IngestionEvent::SdkLog { timestamp, .. } => timestamp,
            IngestionEvent::SessionCreate { timestamp, .. } => timestamp,
            IngestionEvent::SessionUpdate { timestamp, .. } => timestamp,
        }
    }
}

/// 观测类型（V4 扩展，含 10 种变体）
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ObservationType {
    #[default]
    Span,
    Generation,
    Event,
    Agent,
    Tool,
    Chain,
    Retriever,
    Evaluator,
    Embedding,
    Guardrail,
}

/// 观测日志级别
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ObservationLevel {
    Debug,
    Default,
    Warning,
    Error,
}

/// 评分数据类型
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ScoreDataType {
    Numeric,
    Boolean,
    Categorical,
    Correction,
}

/// Langfuse Usage（legacy，API required 字段为 input/output/total）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input: i32,
    pub output: i32,
    pub total: i32,
    pub input_cost: Option<f64>,
    pub output_cost: Option<f64>,
    pub total_cost: Option<f64>,
    pub unit: Option<String>,
}

/// UsageDetails — 灵活的 key-value map
pub type UsageDetails = HashMap<String, i32>;

/// CostDetails — 成本详情 map
pub type CostDetails = HashMap<String, f64>;

/// IngestionUsage — 兼容 Usage 和 OpenAIUsage 的灵活格式
pub type IngestionUsage = HashMap<String, serde_json::Value>;

/// Trace 创建/更新的 Body
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct TraceBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// V4 统一观测类型（ObservationCreate/ObservationUpdate 共用）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ObservationBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    pub r#type: ObservationType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_start_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_observation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_parameters: Option<HashMap<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<IngestionUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<ObservationLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Span Body（SpanCreate/SpanUpdate 共用）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct SpanBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<ObservationLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_observation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Generation Body（GenerationCreate/GenerationUpdate 共用）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct GenerationBody {
    // From OptionalObservationBody
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<ObservationLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_observation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    // Generation-specific fields
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_start_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_parameters: Option<HashMap<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<IngestionUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_details: Option<UsageDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_details: Option<CostDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_version: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Event Body（EventCreate 使用）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct EventBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<ObservationLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_observation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
}

/// Score Body（ScoreCreate 使用）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ScoreBody {
    pub name: String,
    pub value: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_type: Option<ScoreDataType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dataset_run_id: Option<String>,
}

/// SDK Log Body（SdkLog 使用）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct SdkLogBody {
    pub log: serde_json::Value,
}

/// Ingestion 事件统一枚举（10 种变体）
/// 通过 serde 内部标签自动序列化 `type` 判别字段
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum IngestionEvent {
    TraceCreate {
        id: String,
        timestamp: String,
        body: TraceBody,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },
    SpanCreate {
        id: String,
        timestamp: String,
        body: SpanBody,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },
    SpanUpdate {
        id: String,
        timestamp: String,
        body: SpanBody,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },
    GenerationCreate {
        id: String,
        timestamp: String,
        body: GenerationBody,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },
    GenerationUpdate {
        id: String,
        timestamp: String,
        body: GenerationBody,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },
    EventCreate {
        id: String,
        timestamp: String,
        body: EventBody,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },
    ScoreCreate {
        id: String,
        timestamp: String,
        body: ScoreBody,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },
    ObservationCreate {
        id: String,
        timestamp: String,
        body: ObservationBody,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },
    ObservationUpdate {
        id: String,
        timestamp: String,
        body: ObservationBody,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },
    SdkLog {
        id: String,
        timestamp: String,
        body: SdkLogBody,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },
    SessionCreate {
        id: String,
        timestamp: String,
        body: session::SessionBody,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },
    SessionUpdate {
        id: String,
        timestamp: String,
        body: session::SessionBody,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },
}

#[cfg(test)]
#[path = "../types_test.rs"]
mod tests;

// session 测试单独 include
#[cfg(test)]
#[path = "session_test.rs"]
mod session_tests;
