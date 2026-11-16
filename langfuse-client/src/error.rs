use thiserror::Error;

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;

#[path = "error/langfuse_error.rs"]
mod langfuse_error;
pub use langfuse_error::LangfuseError;
