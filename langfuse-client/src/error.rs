use thiserror::Error;

#[derive(Debug, Error)]
pub enum LangfuseError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON serialization failed: {0}")]
    JsonSerialize(#[from] serde_json::Error),

    #[error("Ingestion API returned errors: {0}")]
    IngestionApi(String),

    #[error("Batch sender dropped, batcher is shut down")]
    ChannelClosed,

    #[error("Invalid configuration: {0}")]
    Config(String),
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
