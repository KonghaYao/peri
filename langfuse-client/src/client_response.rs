use serde::Deserialize;
use tracing::warn;

use crate::error::LangfuseError;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportResponse {
    #[serde(default, alias = "partial_success")]
    partial_success: Option<PartialSuccess>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PartialSuccess {
    #[serde(default, alias = "rejected_spans")]
    rejected_spans: RejectedSpans,
    #[serde(default, alias = "error_message")]
    error_message: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RejectedSpans {
    Number(u64),
    Text(String),
}

impl Default for RejectedSpans {
    fn default() -> Self {
        Self::Number(0)
    }
}

pub(super) async fn read_success(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<(), LangfuseError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(response_too_large(limit));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| LangfuseError::IngestionApi("OTLP response body read failed".into()))?
    {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(response_too_large(limit));
        }
        let required = bytes.len() + chunk.len();
        if required > bytes.capacity() {
            let capacity = bytes
                .capacity()
                .saturating_mul(2)
                .max(4096)
                .max(required)
                .min(limit);
            bytes.reserve_exact(capacity - bytes.len());
        }
        bytes.extend_from_slice(&chunk);
    }
    parse(&bytes)
}

fn response_too_large(limit: usize) -> LangfuseError {
    LangfuseError::IngestionApi(format!("OTLP response exceeds {} byte limit", limit))
}

pub(super) fn parse(bytes: &[u8]) -> Result<(), LangfuseError> {
    if bytes.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'{') {
        return Err(LangfuseError::IngestionApi(
            "invalid OTLP response JSON object".into(),
        ));
    }
    let response: ExportResponse = serde_json::from_slice(bytes)
        .map_err(|_| LangfuseError::IngestionApi("invalid OTLP response JSON".into()))?;
    if let Some(partial) = response.partial_success {
        let rejected = match partial.rejected_spans {
            RejectedSpans::Number(count) => count,
            RejectedSpans::Text(count) => {
                if count.is_empty() || !count.bytes().all(|digit| digit.is_ascii_digit()) {
                    return Err(LangfuseError::IngestionApi(
                        "invalid OTLP rejectedSpans".into(),
                    ));
                }
                count
                    .parse::<u64>()
                    .map_err(|_| LangfuseError::IngestionApi("invalid OTLP rejectedSpans".into()))?
            }
        };
        if rejected != 0 {
            return Err(LangfuseError::PartialSuccess {
                rejected_spans: rejected,
            });
        }
        if !partial.error_message.is_empty() {
            warn!("OTLP export accepted all spans with a server warning");
        }
    }
    Ok(())
}
