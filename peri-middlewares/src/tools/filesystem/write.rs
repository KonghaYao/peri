use peri_agent::tools::BaseTool;
use serde_json::Value;
use std::time::Duration;

use super::resolve_path;

const WRITE_FILE_DESCRIPTION: &str = r#"Writes a file to the local filesystem.

Usage:
- This tool will overwrite the existing file if there is one at the provided path
- If this is an existing file, you MUST use the Read tool first to read the file's contents. This tool will fail if you did not read the file first
- ALWAYS prefer editing existing files in the codebase. DO NOT create new files unless explicitly required
- The file_path parameter must be an absolute path, not a relative path
- Parent directories are created automatically if they do not exist

Notes:
- Uses atomic write (write to temp file then rename) to prevent data loss on crash
- NEVER create documentation files (*.md) or README files unless explicitly requested by the User
- Only use emojis if the User explicitly requests it. Avoid writing emojis to files unless asked
- For files longer than 200 lines, consider writing in chunks: use Write for the first chunk, then Write with append=true for subsequent chunks. This reduces context window consumption significantly"#;

/// Write tool - 与 TypeScript write_tool 对齐
pub struct WriteFileTool {
    pub cwd: String,
}

impl WriteFileTool {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self { cwd: cwd.into() }
    }
}

#[async_trait::async_trait]
impl BaseTool for WriteFileTool {
    fn name(&self) -> &str {
        "Write"
    }

    fn description(&self) -> &str {
        WRITE_FILE_DESCRIPTION
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute path to the file to write (must be absolute, not relative)"
                },
                "content": {
                    "type": "string",
                    "description": "The full content to write to the file"
                },
                "append": {
                    "type": "boolean",
                    "description": "If true, append content to the end of the file instead of overwriting. Use this for writing large files in chunks: first call Write without append to create the file with the initial content, then call Write with append=true to add more content. This avoids sending the entire file content in a single tool call, saving context window space.",
                    "default": false
                }
            },
            "required": ["file_path", "content"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let file_path = input["file_path"]
            .as_str()
            .ok_or("The 'file_path' parameter is required for the Write tool.")?;
        let content = input["content"]
            .as_str()
            .ok_or("The 'content' parameter is required for the Write tool.")?;

        let append = input["append"].as_bool().unwrap_or(false);

        let result = tokio::time::timeout(Duration::from_secs(120), async {
            let resolved = resolve_path(&self.cwd, file_path);
            let line_count = content.lines().count();

            if let Some(parent) = resolved.parent() {
                if !parent.exists() {
                    std::fs::create_dir_all(parent)?;
                }
            }

            if append {
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&resolved)
                    .map_err(|e| format!("Error opening file for append: {e}"))?;
                file.write_all(content.as_bytes())
                    .map_err(|e| format!("Error appending to file: {e}"))?;
                drop(file); // 确保句柄关闭后再读取文件

                let total_lines = std::fs::read_to_string(&resolved)
                    .map(|s| s.lines().count())
                    .unwrap_or(line_count);

                let rel = resolved
                    .strip_prefix(&self.cwd)
                    .unwrap_or(&resolved)
                    .display()
                    .to_string();
                let lines_label = if line_count == 1 { "line" } else { "lines" };
                Ok::<String, Box<dyn std::error::Error + Send + Sync>>(format!(
                    "Appended {} {} to {} (file total: {} lines)",
                    line_count, lines_label, rel, total_lines
                ))
            } else {
                // 原子写入：先写临时文件再 rename，防止崩溃时丢失数据
                // 使用随机后缀避免并发写入冲突
                let tmp_ext = format!("tmp.{}", uuid::Uuid::now_v7());
                let tmp_path = resolved.with_extension(tmp_ext);
                if let Err(e) = std::fs::write(&tmp_path, content) {
                    return Err(format!("Error writing file: {e}").into());
                }
                match std::fs::rename(&tmp_path, &resolved) {
                    Ok(_) => {
                        let rel = resolved
                            .strip_prefix(&self.cwd)
                            .unwrap_or(&resolved)
                            .display()
                            .to_string();
                        let lines_label = if line_count == 1 { "line" } else { "lines" };
                        Ok(format!("Wrote {} {} {}", line_count, lines_label, rel))
                    }
                    Err(e) => {
                        let _ = std::fs::remove_file(&tmp_path);
                        Err(format!("Error renaming temp file: {e}").into())
                    }
                }
            }
        })
        .await;

        match result {
            Ok(inner) => inner,
            Err(_elapsed) => Err("Write operation timed out (exceeded 2 minutes).\
                  \nFor large files, use the append=true parameter to write in chunks:\
                  \n1. First call Write without append to create the file with the initial content\
                  \n2. Then call Write with append=true to append the remaining content\
                  \nThis avoids timeouts caused by writing too much content in a single call."
                .into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("write_test.rs");
}
