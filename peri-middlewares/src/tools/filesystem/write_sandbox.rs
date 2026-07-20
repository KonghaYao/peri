//! WriteSandbox 工具——只允许写入 frontmatter 声明的沙箱目录白名单。
//!
//! 用于 readonly subagent（如 plan），给它们一个能力最小化的写入通道：
//! 只能写沙箱目录内的文件，不能碰项目代码。路径安全通过词法校验 +
//! canonicalize 前缀匹配实现 symlink 逃逸防护。

use peri_agent::tools::BaseTool;
use serde_json::Value;
use std::path::{Path, PathBuf};

use super::sandbox_guard::validate_sandbox_path;

const WRITE_SANDBOX_DESC_PREFIX: &str = "Write a file into your sandbox directories: ";

const WRITE_SANDBOX_DESC_SUFFIX: &str = r#"
 Paths are relative to the project root. Overwriting is allowed.
 Absolute paths and '..' are rejected."#;

/// 沙箱写工具——只能写入构造时指定的目录白名单。
#[deprecated(
    since = "0.1.0",
    note = "Use SandboxedWriteTool (name='Write') with allowedWriteDirs instead"
)]
pub struct WriteSandboxTool {
    /// 工作目录（项目根）
    pub cwd: String,
    /// canonicalized 沙箱根路径列表（构造时已校验合法性）
    sandbox_roots: Vec<PathBuf>,
    /// 动态生成的 description
    description: String,
}

impl WriteSandboxTool {
    /// 构造 WriteSandbox 工具。
    ///
    /// `allowed_dirs` 是 frontmatter 声明的相对目录列表（基于 cwd）。
    /// 目录不存在时自动创建，创建失败才报错。
    pub fn new(
        cwd: impl Into<String>,
        allowed_dirs: Vec<String>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let cwd_raw = cwd.into();
        // 构造时 canonicalize cwd，确保 strip_prefix 正确工作
        let cwd = Path::new(&cwd_raw)
            .canonicalize()
            .map(|p| p.display().to_string())
            .unwrap_or(cwd_raw);
        // 构造沙箱根路径：目录不存在则自动创建
        let mut sandbox_roots = Vec::new();
        for dir in &allowed_dirs {
            let raw = Path::new(&cwd).join(dir);
            // 目录不存在则自动创建（避免 subagent 启动时因目录不存在而缺少工具）
            if !raw.exists() {
                std::fs::create_dir_all(&raw)
                    .map_err(|e| format!("WriteSandbox: 无法创建沙箱目录 '{}': {}", dir, e))?;
            }
            let canonical = raw.canonicalize().map_err(|e| {
                format!("WriteSandbox: 无法 canonicalize 沙箱目录 '{}': {}", dir, e)
            })?;
            // 确保是目录
            if !canonical.is_dir() {
                return Err(format!("WriteSandbox: 沙箱路径 '{}' 不是目录", dir).into());
            }
            sandbox_roots.push(canonical);
        }

        // 构造动态 description
        let dirs_display = allowed_dirs.join(", ");
        let description = format!(
            "{}{}{}",
            WRITE_SANDBOX_DESC_PREFIX, dirs_display, WRITE_SANDBOX_DESC_SUFFIX
        );

        Ok(Self {
            cwd,
            sandbox_roots,
            description,
        })
    }

    /// 全路径安全校验链。
    ///
    /// 委托给公共 guard `validate_sandbox_path`。
    /// 返回 canonicalized 目标路径，或错误描述。
    fn validate_path(&self, path: &str) -> Result<PathBuf, String> {
        validate_sandbox_path(&self.cwd, path, &self.sandbox_roots).map_err(|e| e.to_string())
    }
}

#[async_trait::async_trait]
impl BaseTool for WriteSandboxTool {
    fn name(&self) -> &str {
        "WriteSandbox"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The file path relative to the project root (within your sandbox).\
                     Do NOT use absolute paths or '..'. Overwriting is allowed."
                },
                "content": {
                    "type": "string",
                    "description": "The full content to write to the file"
                }
            },
            "required": ["file_path", "content"]
        })
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        None
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let path = input["file_path"]
            .as_str()
            .ok_or("WriteSandbox: 'file_path' 参数必填")?;
        let content = input["content"]
            .as_str()
            .ok_or("WriteSandbox: 'content' 参数必填")?;

        // 安全校验
        let target = self.validate_path(path)?;

        let line_count = content.lines().count();

        // 原子写入：先写临时文件再 rename（复用 WriteFileTool 逻辑）
        let tmp_ext = format!("tmp.{}", uuid::Uuid::now_v7());
        let tmp_path = target.with_extension(tmp_ext);

        let result = tokio::time::timeout(std::time::Duration::from_secs(120), async {
            if let Err(e) = std::fs::write(&tmp_path, content) {
                return Err(format!("WriteSandbox: 写入失败: {}", e).into());
            }

            // 如果目标已存在，保留 Unix 权限位
            if let Ok(metadata) = std::fs::metadata(&target) {
                #[cfg(unix)]
                {
                    let _ = std::fs::set_permissions(&tmp_path, metadata.permissions());
                }
                #[cfg(not(unix))]
                let _ = &metadata;
            }

            match std::fs::rename(&tmp_path, &target) {
                Ok(_) => {
                    let rel = target
                        .strip_prefix(&self.cwd)
                        .unwrap_or(&target)
                        .display()
                        .to_string();
                    let lines_label = if line_count == 1 { "line" } else { "lines" };
                    Ok(format!("Wrote {} {} {}", line_count, lines_label, rel))
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp_path);
                    Err(format!("WriteSandbox: rename 临时文件失败: {}", e).into())
                }
            }
        })
        .await;

        match result {
            Ok(inner) => inner,
            Err(_elapsed) => Err("WriteSandbox: 操作超时（超过 2 分钟）".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    include!("write_sandbox_test.rs");
}
