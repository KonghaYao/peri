use std::path::Path;

use peri_agent::tools::BaseTool;
use serde_json::{json, Value};

use super::artifact_client::ArtifactClient;

const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10MB
const ALLOWED_EXTENSIONS: &[&str] = &["html", "htm"];

/// Artifact 上传工具——将本地 HTML 文件上传到 CCB Artifacts 服务，返回公开 URL。
///
/// 延迟加载（deferred tool）：LLM 通过 SearchExtraTools → ExecuteExtraTool 两步调用。
pub struct ArtifactTool {
    cwd: String,
    client: ArtifactClient,
}

impl ArtifactTool {
    pub fn new(cwd: String) -> Self {
        Self {
            cwd,
            client: ArtifactClient::from_env_or_default(),
        }
    }

    fn resolve_path(&self, file_path: &str) -> Result<std::path::PathBuf, String> {
        let path = Path::new(file_path);
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            Path::new(&self.cwd).join(path)
        };
        Ok(resolved)
    }

    fn validate_file(&self, path: &Path) -> Result<(), String> {
        if !path.exists() {
            return Err(format!("File not found: {}", path.display()));
        }
        if !path.is_file() {
            return Err(format!("Not a file: {}", path.display()));
        }

        // 检查扩展名
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());
        match ext {
            Some(ref e) if ALLOWED_EXTENSIONS.contains(&e.as_str()) => {}
            _ => {
                return Err(format!(
                    "Only HTML files are supported (allowed: {}). Got: {}",
                    ALLOWED_EXTENSIONS.join(", "),
                    path.display()
                ));
            }
        }

        // 检查大小
        let size = match std::fs::metadata(path) {
            Ok(m) => m.len(),
            Err(e) => return Err(format!("Cannot read file metadata: {}", e)),
        };
        if size > MAX_FILE_SIZE {
            return Err(format!(
                "File too large: {} bytes (max: {} bytes / 10MB)",
                size, MAX_FILE_SIZE
            ));
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl BaseTool for ArtifactTool {
    fn name(&self) -> &str {
        "artifact"
    }

    fn description(&self) -> &str {
        "Upload an HTML file to a public URL with automatic expiry. \
         The file will be accessible via a shareable link for 7 days (default) or 30 days. \
         Use this after generating HTML content (dashboards, reports, prototypes) that you want to share. \
         Returns a clickable URL that can be opened in any browser."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to the HTML file to upload (relative or absolute)"
                },
                "ttl": {
                    "type": "string",
                    "enum": ["7d", "30d"],
                    "description": "Time-to-live. Use '7d' for 7-day expiry (default), '30d' for 30-day expiry."
                }
            },
            "required": ["file_path"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let file_path = input["file_path"]
            .as_str()
            .ok_or("Missing required parameter: file_path")?;
        let ttl = input["ttl"].as_str().unwrap_or("7d");

        let resolved = self.resolve_path(file_path)?;
        self.validate_file(&resolved)?;

        let content = std::fs::read_to_string(&resolved)
            .map_err(|e| format!("Failed to read file {}: {}", resolved.display(), e))?;

        let output = self.client.upload(&content, ttl).await;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("artifact_tool_test.rs");
}
