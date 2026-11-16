use std::time::Duration;

use base64::Engine;
use reqwest::Client;
use tracing::warn;

use crate::config::Compression;
use crate::config::TelemetryConfig;
use crate::encoding::BudgetedWriter;
use crate::encoding::ExportContext;
use crate::error::LangfuseError;
use crate::memory::MemoryBudget;
use crate::memory::GZIP_BILLING_BYTES;
use crate::payload::ExportPayload;
use crate::snapshot::measure_event;
use crate::snapshot::TelemetryEvent;
use crate::spool::route_fingerprint;
use crate::spool::RouteFingerprint;
use crate::types::IngestionEvent;

#[path = "client_body.rs"]
pub(crate) mod body;
#[path = "client_export.rs"]
pub(crate) mod export;
#[path = "client_response.rs"]
mod response;
#[path = "client_retry.rs"]
mod retry;
#[path = "client_send.rs"]
pub(crate) mod send;

pub use export::ExportConfig;
use send::{MemoryCheckpoint, SendOutcome};

#[cfg(test)]
#[path = "client_response_test.rs"]
mod response_tests;
#[cfg(test)]
#[path = "client_retry_test.rs"]
mod retry_tests;
#[cfg(test)]
#[path = "client_test.rs"]
mod tests;

#[path = "client/langfuse_client.rs"]
mod langfuse_client;
pub use langfuse_client::LangfuseClient;

#[cfg(test)]
use langfuse_client::direct_budget;
