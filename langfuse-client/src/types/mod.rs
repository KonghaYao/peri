pub(crate) mod conversion;
mod otlp;
pub mod session;

use std::collections::HashMap;

/// 物化回归入口（契约 §3.5）：只由 `conversion_test` / `encoding::stream_test` 使用。
#[cfg(test)]
pub(crate) use conversion::ingestion_events_to_otel;
use serde::{Deserialize, Serialize};

#[cfg(test)]
#[path = "../types_test.rs"]
mod tests;

// session 测试单独 include
#[cfg(test)]
#[path = "session_test.rs"]
mod session_tests;

mod ingestion_event;
pub use ingestion_event::CostDetails;
pub use ingestion_event::EventBody;
pub use ingestion_event::GenerationBody;
pub use ingestion_event::IngestionEvent;
pub use ingestion_event::IngestionUsage;
pub use ingestion_event::ObservationBody;
pub use ingestion_event::ObservationLevel;
pub use ingestion_event::ObservationType;
pub use ingestion_event::ScoreBody;
pub use ingestion_event::ScoreDataType;
pub use ingestion_event::SdkLogBody;
pub use ingestion_event::SpanBody;
pub use ingestion_event::TraceBody;
pub use ingestion_event::Usage;
pub use ingestion_event::UsageDetails;
