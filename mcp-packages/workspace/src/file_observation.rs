use rmcp::{
    model::{CustomRequest, CustomResult},
    ErrorData as McpError,
};

pub(crate) async fn read_text(cwd: &str, request: CustomRequest) -> Result<CustomResult, McpError> {
    let path = request
        .params
        .as_ref()
        .and_then(|params| params.get("path"))
        .and_then(|path| path.as_str())
        .filter(|path| !path.is_empty())
        .ok_or_else(|| McpError::invalid_params("workspace/readText requires path", None))?;
    let path = std::path::Path::new(cwd).join(path);
    let text = tokio::fs::read_to_string(path)
        .await
        .map_err(|_| McpError::internal_error("workspace text read failed", None))?;
    Ok(CustomResult::new(serde_json::json!({ "text": text })))
}

#[cfg(test)]
#[path = "file_observation_test.rs"]
mod tests;
