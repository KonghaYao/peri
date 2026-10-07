use peri_acp_types::tools::ToolContext;
use rmcp::model::RequestMetaObject;

pub(crate) const INVOCATION_META_KEY: &str = "peri.invocation";

#[derive(Debug, thiserror::Error)]
pub(crate) enum InvocationError {
    #[error("MCP invocation metadata conflicts with trusted context")]
    IdentityConflict,
}

pub(crate) fn request_meta(
    context: &ToolContext<'_>,
    existing: Option<RequestMetaObject>,
) -> Result<RequestMetaObject, InvocationError> {
    let mut metadata = existing.unwrap_or_default();
    if metadata.0 .0.contains_key(INVOCATION_META_KEY) {
        return Err(InvocationError::IdentityConflict);
    }
    metadata.0 .0.insert(
        INVOCATION_META_KEY.into(),
        serde_json::json!({
            "version": 2,
            "initiatorSessionId": context.session_id,
            "invocationId": context.invocation_id,
            "toolCallId": context.tool_call_id,
        }),
    );
    Ok(metadata)
}

#[cfg(test)]
#[path = "invocation_test.rs"]
mod tests;
