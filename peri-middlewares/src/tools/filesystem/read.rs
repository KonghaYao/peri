use peri_agent::tools::BaseTool;
use serde_json::Value;

use super::folder::list_folder;
use super::resolve_path;
use crate::tools::output_persist::persist_truncated_output;
use crate::tools::output_truncate::truncate_bytes;

/// Read tool - 与 TypeScript read_tool 对齐
pub struct ReadFileTool {
    pub cwd: String,
}

impl ReadFileTool {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self { cwd: cwd.into() }
    }
}

const MAX_LINES: usize = 2000;
/// 最大允许读取的文件大小（32 MB）
const MAX_FILE_SIZE: u64 = 32 * 1024 * 1024;
/// 单行最大字符数（超过则截断）
const MAX_CHARS_PER_LINE: usize = 65536;
/// 输出最大字节数（超过后截断并落盘）
const MAX_OUTPUT_BYTES: usize = 5_000;

const READ_FILE_DESCRIPTION: &str = include_str!("descriptions/read.md");

/// 解析 1-based 行号/行数参数（offset/limit）。
///
/// 语义与 schema 描述一致：offset 是 1-based 行号（1 = 首行），limit 是行数。
/// 缺省时返回 `default`；显式传入非正整数（0、负数、小数、非数字）一律报错，
/// 避免 `as_u64()` 对浮点静默回退为默认值、导致读到错误位置。
fn parse_line_number(
    value: &Value,
    name: &str,
    default: usize,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    if value.is_null() {
        return Ok(default);
    }
    let n = value
        .as_f64()
        .ok_or_else(|| format!("Error: '{name}' must be a positive integer, got {value}"))?;
    if n.fract() != 0.0 || n < 1.0 {
        return Err(format!(
            "Error: '{name}' must be a positive integer (1-based line number), got {n}"
        )
        .into());
    }
    Ok(n as usize)
}

fn is_binary_extension(ext: &str) -> bool {
    matches!(
        ext,
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "bmp"
            | "ico"
            | "webp"
            | "tiff"
            | "pdf"
            | "doc"
            | "docx"
            | "xls"
            | "xlsx"
            | "ppt"
            | "pptx"
            | "zip"
            | "rar"
            | "7z"
            | "tar"
            | "gz"
            | "mp3"
            | "wav"
            | "ogg"
            | "flac"
            | "mp4"
            | "avi"
            | "mkv"
            | "mov"
            | "exe"
            | "dll"
            | "so"
            | "dylib"
            | "bin"
            | "class"
    )
}

#[async_trait::async_trait]
impl BaseTool for ReadFileTool {
    fn name(&self) -> &str {
        "Read"
    }

    fn is_direct(&self) -> bool {
        true
    }

    /// 演示分组（design v2 §2.5.1 示例：同类工具按 namespace 组织声明段）。
    fn namespace(&self) -> Option<&str> {
        Some("filesystem")
    }

    /// 提示词层声明模板（design v2 §2.5.3 示例语义）。
    ///
    /// title 不覆盖——走 `BaseTool::tool_description` 默认路径由 name 推导
    /// （"Read" → "Read"），验证缺省推导在真实工具上生效。
    /// 全量迁移完成：声明段是工具选择指引的单一事实源，05 段落无对应条目
    /// （守护测试断言渲染输出与 05 剩余内容无逐字重复）。
    fn prompt_declaration(&self) -> Option<String> {
        Some(
            "Read a file → `{{name}}` ({{title}}). Use `{{name}}` for file content, not `cat`/`head`/`tail`."
                .to_string(),
        )
    }

    fn description(&self) -> &str {
        READ_FILE_DESCRIPTION
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute path to the file to read"
                },
                "offset": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional 1-based start line. OMIT by default. NEVER guess or estimate this value, and never use a large offset to probe the end of a file. Set it only to a line number already observed in Read/Grep output or explicitly provided by the user. For continuation, use the last line actually shown plus 1; do not derive it from limit or an assumed file length"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "The number of lines to read. Only provide if the file is too large to read in a single call. Not providing this parameter reads the whole file (recommended)"
                },
                "pages": {
                    "type": "string",
                    "description": "For PDF files, the page range to read, e.g. '1-5', '3', '10-20'. Only applies to PDF files"
                }
            },
            "required": ["file_path"]
        })
    }

    fn aliases(&self) -> &[&str] {
        &["reading"]
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let file_path = input["file_path"]
            .as_str()
            .ok_or("The 'file_path' parameter is required for the Read tool. Provide the absolute path to the file.")?;

        // offset 语义为 1-based 行号（1 = 首行），与 schema 描述、输出行号一致；
        // 缺省 offset=1（读全文起点）、limit=2000。
        let offset = parse_line_number(&input["offset"], "offset", 1)?;
        let limit = parse_line_number(&input["limit"], "limit", MAX_LINES)?;

        let resolved = resolve_path(&self.cwd, file_path);

        let pages = input["pages"].as_str().map(|s| s.to_string());

        // PDF + pages: 返回占位提示
        if let Some(ext) = resolved.extension().and_then(|e| e.to_str()) {
            if ext.eq_ignore_ascii_case("pdf") && pages.is_some() {
                return Ok(format!(
                    "[PDF READING NOT YET SUPPORTED]\n\nFile path: {}\nPDF reading with page selection is not yet implemented. Use the Bash tool with a PDF reader command as a workaround.",
                    resolved.display()
                ));
            }
            // PDF 但未提供 pages → 继续走到下面的二进制检测，返回 BINARY FILE DETECTED
        }

        if let Some(ext) = resolved.extension().and_then(|e| e.to_str()) {
            if is_binary_extension(&ext.to_lowercase()) {
                return Ok(format!(
                    "[BINARY FILE DETECTED]\n\nFile type: .{ext}\nFile path: {}\n\nThis is a binary file and cannot be displayed as text.",
                    resolved.display()
                ));
            }
        }

        let content = match std::fs::metadata(&resolved) {
            Ok(meta) if meta.len() > MAX_FILE_SIZE => {
                return Err(format!(
                    "Error: File too large ({} bytes, max {} bytes). offset/limit cannot bypass the file-size limit; use Grep to locate content or another suitable file-processing tool.",
                    meta.len(),
                    MAX_FILE_SIZE
                ).into());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!("Error: File not found at {file_path}").into());
            }
            Err(e) => return Err(e.into()),
            Ok(meta) if meta.is_dir() => {
                return list_folder(&resolved).map(|listing| {
                    format!(
                        "[DIRECTORY DETECTED]\n\nRead received a directory path and converted it to a directory listing. Use folder_operations with operation=\"list\" for explicit directory operations.\n\n{}",
                        listing
                    )
                });
            }
            Ok(meta) if meta.len() == 0 => {
                return Ok(format!(
                    "[EMPTY FILE]\n\nFile path: {}\nThe file is empty (0 bytes).",
                    resolved.display()
                ));
            }
            _ => match std::fs::read_to_string(&resolved) {
                Ok(c) => c,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(format!("Error: File not found at {file_path}").into());
                }
                Err(e) => return Err(e.into()),
            },
        };

        let lines: Vec<&str> = content.split('\n').collect();
        // 1-based 行号 → 0-based 切片索引
        let start = offset - 1;
        if start >= lines.len() {
            return Err(format!(
                "Error: offset {offset} exceeds file length ({} lines). Valid offsets are 1..={}; omit offset to read from the beginning. Do not guess another offset or use offset to probe the file end.",
                lines.len(),
                lines.len()
            )
            .into());
        }
        let end = (start + limit).min(lines.len());
        let selected = &lines[start..end];

        let mut numbered: Vec<String> = Vec::new();
        for (i, line) in selected.iter().enumerate() {
            let line_num = start + i + 1;
            // 将截断元数据放在内容前，确保后续输出级截断不会隐藏该信息。
            let line_char_count = line.chars().count();
            let content = if line_char_count > MAX_CHARS_PER_LINE {
                format!(
                    "[LINE TRUNCATED: {line_char_count} characters total; retained first {MAX_CHARS_PER_LINE} characters before output-level truncation] {}",
                    line.chars().take(MAX_CHARS_PER_LINE).collect::<String>()
                )
            } else {
                (*line).to_string()
            };
            numbered.push(format!("{:>6}\t{}", line_num, content));
        }

        let mut output = numbered.join("\n");

        // 总输出超过上限时截断并落盘；提示保留原始大小和完整输出路径。
        if output.len() > MAX_OUTPUT_BYTES {
            let original_output_bytes = output.len();
            let persist_hint = persist_truncated_output(&output);
            output = truncate_bytes(&output, MAX_OUTPUT_BYTES);
            output.push_str(&format!(
                "\n[Output truncated: {original_output_bytes} bytes total; showing first {MAX_OUTPUT_BYTES} bytes]{persist_hint}"
            ));
        }

        Ok(output)
    }

    fn prefers_persist(&self) -> bool {
        true
    }
}

#[cfg(test)]
#[path = "read_test.rs"]
mod tests;
