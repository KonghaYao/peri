//! Model-facing failure projection preserving diagnostics and recovery guidance.

use crate::failure::ToolFailure;

const MAX_FAILURE_BYTES: usize = 16 * 1024;
const MAX_FAILURE_DEPTH: usize = 16;

pub fn failure_text(tool: &str, error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = format!("tool `{tool}` failed to execute; {error}");
    let mut cause = error.source();
    for _ in 1..MAX_FAILURE_DEPTH {
        let Some(source) = cause else { break };
        text.push_str(&format!("\nCaused by: {source}"));
        cause = source.source();
    }
    if cause.is_some() {
        text.push_str("\n[Cause chain truncated after 16 levels]");
    }
    if let Some(failure) = error.downcast_ref::<ToolFailure>() {
        if !text.contains(&failure.recovery) {
            text.push_str(&format!("\nRecovery: {}", failure.recovery));
        }
    }
    if text.len() > MAX_FAILURE_BYTES {
        let marker = "\n[Diagnostic truncated: exceeds 16 KiB limit]";
        let mut end = MAX_FAILURE_BYTES - marker.len();
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str(marker);
    }
    text
}

#[cfg(test)]
#[path = "result_mapping_test.rs"]
mod tests;
