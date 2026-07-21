//! WriteSandbox 工具——只允许写入 frontmatter 声明的沙箱目录白名单。
//!
//! 用于 readonly subagent（如 plan），给它们一个能力最小化的写入通道：
//! 只能写沙箱目录内的文件，不能碰项目代码。路径安全通过词法校验 +
//! canonicalize 前缀匹配实现 symlink 逃逸防护。

use peri_agent::tools::BaseTool;
use serde_json::Value;
use std::path::{Path, PathBuf};

const WRITE_SANDBOX_DESC_PREFIX: &str = "Write a file ONLY into your sandbox directories: ";

const WRITE_SANDBOX_DESC_SUFFIX: &str = r#"
 Paths are relative to the project root. Overwriting is allowed.
 Absolute paths and '..' are rejected.
 Do NOT use this tool for files outside the sandbox directories listed above."#;

/// 沙箱写工具——只能写入构造时指定的目录白名单。
pub struct WriteSandboxTool {
    /// 工作目录（项目根）
    pub cwd: String,
    /// canonicalized 沙箱根路径列表（构造时已校验合法性）
    sandbox_roots: Vec<PathBuf>,
    /// 原始相对路径列表（用于错误消息展示，避免绝对路径与工具要求矛盾）
    allowed_dirs: Vec<String>,
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
            allowed_dirs,
            description,
        })
    }

    /// 格式化允许的沙箱目录列表，用于错误信息。
    /// 使用原始相对路径（如 `.peri/plans/`），而非 canonicalized 绝对路径，
    /// 因为工具只接受相对路径，展示绝对路径会与约束矛盾。
    fn allowed_dirs_display(&self) -> String {
        format!("允许的目录: {:?}", self.allowed_dirs)
    }

    /// 全路径安全校验链。
    ///
    /// 返回 canonicalized 目标路径，或错误描述。
    fn validate_path(&self, path: &str) -> Result<PathBuf, String> {
        // ① 词法拒绝绝对路径
        if Path::new(path).is_absolute() {
            return Err(format!(
                "WriteSandbox: 拒绝绝对路径 '{}'。请使用基于项目根的相对路径。{}",
                path,
                self.allowed_dirs_display()
            ));
        }
        // ② 词法拒绝路径穿越（含 ../、..\\ 等变体）
        let normalized = path.replace('\\', "/");
        for segment in normalized.split('/') {
            if segment == ".." {
                return Err(format!(
                    "WriteSandbox: 拒绝路径穿越 '{}'（含 '..'）。请使用沙箱目录内的相对路径。{}",
                    path,
                    self.allowed_dirs_display()
                ));
            }
        }

        let raw = Path::new(&self.cwd).join(path);

        // ③ 寻找最长存在祖先并 canonicalize + 沙箱校验，防止 create_dir_all
        //    跟随 symlink 在沙箱外创建目录（副作用逃逸）
        // ④ 然后创建剩余父目录 + canonicalize 目标（防 symlink 逃逸）
        let canonical_target = if raw.exists() {
            // 文件已存在：直接 canonicalize 目标本身
            raw.canonicalize().map_err(|e| {
                format!(
                    "WriteSandbox: canonicalize 失败 '{}': {}。{}",
                    path,
                    e,
                    self.allowed_dirs_display()
                )
            })?
        } else {
            // 文件不存在：找到最长存在的祖先路径
            let ancestor = {
                let mut p = raw.as_path();
                loop {
                    match p.parent() {
                        Some(parent) if !parent.exists() => p = parent,
                        Some(parent) => break parent.to_path_buf(),
                        None => break p.to_path_buf(),
                    }
                }
            };
            // canonicalize 已有祖先 + 校验沙箱前缀
            let canon_ancestor = ancestor.canonicalize().map_err(|e| {
                format!(
                    "WriteSandbox: 无法 canonicalize 路径 '{}': {}。{}",
                    ancestor.display(),
                    e,
                    self.allowed_dirs_display()
                )
            })?;
            let is_ancestor_in_sandbox = self
                .sandbox_roots
                .iter()
                .any(|root| canon_ancestor.starts_with(root));
            if !is_ancestor_in_sandbox {
                return Err(format!(
                    "WriteSandbox: 路径 '{}' 的已有祖先不在沙箱目录内。{}",
                    path,
                    self.allowed_dirs_display()
                ));
            }
            // 创建剩余父目录（祖先已校验，新创建的目录在祖先子树内 = 安全）
            if let Some(parent) = raw.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    format!(
                        "WriteSandbox: 创建父目录失败 '{}': {}。{}",
                        path,
                        e,
                        self.allowed_dirs_display()
                    )
                })?;
            }
            // 再 canonicalize 父目录 + 文件名
            if let (Some(parent), Some(file_name)) = (raw.parent(), raw.file_name()) {
                match parent.canonicalize() {
                    Ok(canon_parent) => canon_parent.join(file_name),
                    Err(e) => {
                        return Err(format!(
                            "WriteSandbox: 无法 canonicalize 父目录 '{}': {}。{}",
                            parent.display(),
                            e,
                            self.allowed_dirs_display()
                        ));
                    }
                }
            } else {
                return Err(format!(
                    "WriteSandbox: 无法解析路径 '{}'——缺少父目录或文件名。{}",
                    path,
                    self.allowed_dirs_display()
                ));
            }
        };

        // ⑤ 最终以沙箱根为前缀校验
        let is_in_sandbox = self
            .sandbox_roots
            .iter()
            .any(|root| canonical_target.starts_with(root));
        if !is_in_sandbox {
            return Err(format!(
                "WriteSandbox: 路径 '{}' 不在沙箱目录内。允许的目录: {:?}",
                path,
                self.sandbox_roots
                    .iter()
                    .map(|r| r.display().to_string())
                    .collect::<Vec<_>>()
            ));
        }

        Ok(canonical_target)
    }
}

#[async_trait::async_trait]
impl BaseTool for WriteSandboxTool {
    fn name(&self) -> &str {
        "SandboxWrite"
    }

    fn aliases(&self) -> &[&str] {
        &["WriteSandbox"]
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
#[path = "write_sandbox_test.rs"]
mod tests;
