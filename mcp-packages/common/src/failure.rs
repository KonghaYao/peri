//! Tool failures retain diagnostic detail alongside actionable recovery guidance.

#[path = "failure/tool_failure.rs"]
mod tool_failure;
pub use tool_failure::ToolFailure;
