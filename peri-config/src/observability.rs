use std::collections::BTreeMap;

use thiserror::Error;

#[path = "observability_spool.rs"]
mod spool;

#[cfg(test)]
#[path = "observability_test.rs"]
mod tests;

#[path = "observability/langfuse_config.rs"]
mod langfuse_config;
pub use langfuse_config::resolve;
pub use langfuse_config::ConfigValueSource;
pub use langfuse_config::LangfuseCompression;
pub use langfuse_config::LangfuseConfig;
pub use langfuse_config::LangfuseConfigError;
pub use langfuse_config::ENVIRONMENT_KEYS;
