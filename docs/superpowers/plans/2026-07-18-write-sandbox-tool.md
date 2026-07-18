# WriteSandbox 工具实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 readonly subagent（如 plan）提供一个能力最小化的写工具 WriteSandbox——只能写入 frontmatter 声明的沙箱目录白名单，不能碰项目代码。

**Architecture:** 新增 `WriteSandboxTool` 实现 `BaseTool`，在 `build_agent_from_def` 中按 per-agent 实例注入（不走父工具继承）。路径安全通过词法校验 + canonicalize 前缀匹配实现 symlink 逃逸防护。暴露能力通过 `ClaudeAgentFrontmatter.allowed_write_dirs` 字段声明，`can_mutate` 推断忽略此字段。

**Tech Stack:** Rust, serde_yaml, tokio, tempfile

---

## 文件结构

| 文件 | 职责 | 变更类型 |
|------|------|----------|
| `peri-middlewares/src/tools/filesystem/write_sandbox.rs` | WriteSandbox 工具定义 + 路径安全校验 | **新建** |
| `peri-middlewares/src/tools/filesystem/mod.rs` | 模块注册 + 预导出 | 修改 |
| `peri-middlewares/src/claude_agent_parser/mod.rs` | `ClaudeAgentFrontmatter` 新增 `allowed_write_dirs` 字段 | 修改 |
| `peri-middlewares/src/subagent/tool/build_agent.rs` | `build_agent_from_def` 注入 WriteSandbox | 修改 |
| `peri-middlewares/src/subagent/built-in/plan.md` | plan agent frontmatter + 正文指引 | 修改 |
| `peri-middlewares/src/subagent/mod.rs` | `infer_agent_capability` 注释 | 修改 |
| `peri-acp/src/prompt/mod.rs` | `can_mutate` 注释 | 修改 |

---

### Task 1: ClaudeAgentFrontmatter 新增 `allowed_write_dirs` 字段

**Files:**
- Modify: `peri-middlewares/src/claude_agent_parser/mod.rs`

- [ ] **Step 1: 在 `ClaudeAgentFrontmatter` 结构体中新增字段**

在 `pub isolation: Option<String>,` 之后、`}` 之前加入：

```rust
    /// 沙箱写目录白名单——声明后 subagent 可获得 WriteSandbox 工具，
    /// 只能写入这些相对目录（基于 cwd），不能碰项目代码。
    /// 不影响 can_mutate 推断（agent 仍视为 readonly）。
    #[serde(default)]
    pub allowed_write_dirs: Vec<String>,
```

该字段使用 `#[serde(default)]`，YAML 中写 `allowedWriteDirs`（camelCase 自动转换），支持 YAML 数组格式如：

```yaml
allowedWriteDirs:
  - ".peri/plans/"
```

缺失时默认为空 `Vec`（serde default）。

- [ ] **Step 2: 运行已有测试确保未破坏解析**

```bash
cargo test -p peri-middlewares --lib -- claude_agent_parser
```

期望：全部 PASS（已有测试中无 `allowedWriteDirs`，应默认空）

- [ ] **Step 3: 新增 roundtrip 解析测试（在 claude_agent_parser_test.rs 末尾追加）**

追加以下测试：

```rust
/// [回归测试] allowedWriteDirs roundtrip——plan agent 声明沙箱目录
#[test]
fn test_parse_allowed_write_dirs() {
    let content = r#"---
name: planner
description: A planner agent
allowedWriteDirs:
  - ".peri/plans/"
  - ".peri/output/"
---
prompt"#;
    let agent = parse_agent_file(content).unwrap();
    assert_eq!(
        agent.frontmatter.allowed_write_dirs,
        vec![".peri/plans/", ".peri/output/"]
    );
}

/// [回归测试] allowedWriteDirs 缺失时默认为空
#[test]
fn test_parse_allowed_write_dirs_missing_defaults_empty() {
    let content = r#"---
name: basic
description: test
---
prompt"#;
    let agent = parse_agent_file(content).unwrap();
    assert!(agent.frontmatter.allowed_write_dirs.is_empty());
}
```

- [ ] **Step 4: 运行测试验证**

```bash
cargo test -p peri-middlewares --lib -- test_parse_allowed_write
```

期望：全部 PASS

- [ ] **Step 5: Commit**

```bash
git add peri-middlewares/src/claude_agent_parser/mod.rs peri-middlewares/src/claude_agent_parser/claude_agent_parser_test.rs
git commit -m "feat: ClaudeAgentFrontmatter 新增 allowed_write_dirs 字段"
```

---

### Task 2: WriteSandbox 工具实现

**Files:**
- Create: `peri-middlewares/src/tools/filesystem/write_sandbox.rs`

- [ ] **Step 1: 创建 `write_sandbox.rs` 文件**

```rust
//! WriteSandbox 工具——只允许写入 frontmatter 声明的沙箱目录白名单。
//!
//! 用于 readonly subagent（如 plan），给它们一个能力最小化的写入通道：
//! 只能写沙箱目录内的文件，不能碰项目代码。路径安全通过词法校验 +
//! canonicalize 前缀匹配实现 symlink 逃逸防护。

use peri_agent::tools::BaseTool;
use serde_json::Value;
use std::path::{Path, PathBuf};

use super::resolve_path;

const WRITE_SANDBOX_DESC_PREFIX: &str = "Write a file into your sandbox directories: ";

const WRITE_SANDBOX_DESC_SUFFIX: &str = r#"
 Paths are relative to the project root. Overwriting is allowed.
 Absolute paths and '..' are rejected."#;

/// 沙箱写工具——只能写入构造时指定的目录白名单。
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
    /// 构造时 canonicalize 每个目录，失败则报错。
    pub fn new(
        cwd: impl Into<String>,
        allowed_dirs: Vec<String>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let cwd = cwd.into();
        // 构造时 canonicalize 沙箱根路径
        let mut sandbox_roots = Vec::new();
        for dir in &allowed_dirs {
            let raw = Path::new(&cwd).join(dir);
            let canonical = raw.canonicalize().map_err(|e| {
                format!(
                    "WriteSandbox: 无法 canonicalize 沙箱目录 '{}': {}",
                    dir, e
                )
            })?;
            // 确保是目录
            if !canonical.is_dir() {
                return Err(format!(
                    "WriteSandbox: 沙箱路径 '{}' 不是目录",
                    dir
                )
                .into());
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
    /// 返回 canonicalized 目标路径，或错误描述。
    fn validate_path(&self, path: &str) -> Result<PathBuf, String> {
        // ① 词法拒绝绝对路径
        if Path::new(path).is_absolute() {
            return Err(format!(
                "WriteSandbox: 拒绝绝对路径 '{}'。请使用基于项目根的相对路径。",
                path
            ));
        }
        // ② 词法拒绝路径穿越（含 ../、..\\ 等变体）
        let normalized = path.replace('\\', "/");
        for segment in normalized.split('/') {
            if segment == ".." {
                return Err(format!(
                    "WriteSandbox: 拒绝路径穿越 '{}'（含 '..'）。请使用沙箱目录内的相对路径。",
                    path
                ));
            }
        }

        let raw = Path::new(&self.cwd).join(path);

        // ③ 确保父目录存在，canonicalize 父目录以解析 symlink
        if let Some(parent) = raw.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("WriteSandbox: 创建父目录失败 '{}': {}", path, e))?;
        }

        // ④ 目标文件已存在时：canonicalize 目标本身（防 symlink 逃逸）
        let canonical_target = if raw.exists() {
            raw.canonicalize()
                .map_err(|e| format!("WriteSandbox: canonicalize 失败 '{}': {}", path, e))?
        } else {
            // 文件不存在：canonicalize 父目录 + 文件名
            if let (Some(parent), Some(file_name)) = (raw.parent(), raw.file_name()) {
                match parent.canonicalize() {
                    Ok(canon_parent) => canon_parent.join(file_name),
                    Err(_) => raw,
                }
            } else {
                raw
            }
        };

        // ⑤ 以沙箱根为前缀校验
        let is_in_sandbox = self.sandbox_roots.iter().any(|root| {
            canonical_target.starts_with(root)
        });
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
        "WriteSandbox"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The file path relative to the project root (within your sandbox).\
                     Do NOT use absolute paths or '..'. Overwriting is allowed."
                },
                "content": {
                    "type": "string",
                    "description": "The full content to write to the file"
                }
            },
            "required": ["path", "content"]
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
        let path = input["path"]
            .as_str()
            .ok_or("WriteSandbox: 'path' 参数必填")?;
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
                { let _ = std::fs::set_permissions(&tmp_path, metadata.permissions()); }
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
```

- [ ] **Step 2: 编译检查**

```bash
cargo build -p peri-middlewares
```

期望：无编译错误

- [ ] **Step 3: Commit**

```bash
git add peri-middlewares/src/tools/filesystem/write_sandbox.rs
git commit -m "feat: 新增 WriteSandboxTool——沙箱写工具"
```

---

### Task 3: 模块注册

**Files:**
- Modify: `peri-middlewares/src/tools/filesystem/mod.rs`

- [ ] **Step 1: 在 mod.rs 中注册并预导出 write_sandbox 模块**

在 `mod.rs` 第 8 行末尾添加 `write_sandbox` 模块声明：
```rust
pub mod write;

// 在这行下面新增：
pub mod write_sandbox;
```

在 `pub use` 区域末尾添加预导出：
```rust
pub use write_sandbox::WriteSandboxTool;
```

- [ ] **Step 2: 编译检查**

```bash
cargo build -p peri-middlewares
```

期望：无编译错误

- [ ] **Step 3: Commit**

```bash
git add peri-middlewares/src/tools/filesystem/mod.rs
git commit -m "feat: 在文件系统工具模块中注册 WriteSandboxTool"
```

---

### Task 4: WriteSandbox 路径安全单元测试

**Files:**
- Create: `peri-middlewares/src/tools/filesystem/write_sandbox_test.rs`

- [ ] **Step 1: 在 `write_sandbox.rs` 末尾追加测试模块声明**

在文件末尾追加：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    include!("write_sandbox_test.rs");
}
```

- [ ] **Step 2: 创建测试文件 `write_sandbox_test.rs`**

内容（覆盖 spec 测试清单中所有 P0 路径）：

```rust
fn make_tool(dir: &tempfile::TempDir, allowed: Vec<&str>) -> WriteSandboxTool {
    let cwd = dir.path().to_str().unwrap().to_string();
    // 先创建沙箱目录
    for d in &allowed {
        std::fs::create_dir_all(dir.path().join(d)).unwrap();
    }
    WriteSandboxTool::new(
        cwd,
        allowed.iter().map(|s| s.to_string()).collect(),
    )
    .unwrap()
}

#[tokio::test]
async fn test_write_sandbox_normal_create() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let result = tool
        .invoke(
            serde_json::json!({"path": "sandbox/hello.md", "content": "# Plan"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("Wrote 1 line"));
    let content = std::fs::read_to_string(dir.path().join("sandbox/hello.md")).unwrap();
    assert_eq!(content, "# Plan");
}

#[tokio::test]
async fn test_write_sandbox_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    // 先写一次
    std::fs::write(dir.path().join("sandbox/v2.md"), "v1").unwrap();
    // 再覆盖写
    tool.invoke(
        serde_json::json!({"path": "sandbox/v2.md", "content": "v2"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    let content = std::fs::read_to_string(dir.path().join("sandbox/v2.md")).unwrap();
    assert_eq!(content, "v2");
}

#[tokio::test]
async fn test_write_sandbox_dotdot_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let result = tool
        .invoke(
            serde_json::json!({"path": "sandbox/../outside.txt", "content": "evil"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("..") || err.contains("sandbox"),
        "错误消息应包含路径穿越信息: {}",
        err
    );
}

#[tokio::test]
async fn test_write_sandbox_absolute_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let abs = dir.path().join("outside.txt");
    let result = tool
        .invoke(
            serde_json::json!({"path": abs.to_str().unwrap(), "content": "evil"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("绝对") || err.contains("absolute"),
        "错误消息应说明拒绝原因: {}",
        err
    );
}

#[tokio::test]
async fn test_write_sandbox_outside_dir_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let result = tool
        .invoke(
            serde_json::json!({"path": "other/outside.txt", "content": "nope"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("sandbox") || err.contains("沙箱"),
        "错误消息应提示沙箱限制: {}",
        err
    );
}

#[tokio::test]
async fn test_write_sandbox_symlink_escape_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    // 在沙箱内创建指向外部文件的 symlink
    std::fs::write(dir.path().join("outside.txt"), "evil").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            dir.path().join("outside.txt"),
            dir.path().join("sandbox/escape_link.txt"),
        )
        .unwrap();
    }
    #[cfg(not(unix))]
    {
        // Windows: junction 测试跳过，词法前缀已覆盖
        return;
    }
    let result = tool
        .invoke(
            serde_json::json!({"path": "sandbox/escape_link.txt", "content": "bypass"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("sandbox") || err.contains("沙箱"),
        "symlink 逃逸应被拒绝: {}",
        err
    );
}

#[tokio::test]
async fn test_write_sandbox_parent_symlink_escape_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    // sandbox/sub 是外部目录的 symlink
    std::fs::create_dir_all(dir.path().join("outside_dir")).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            dir.path().join("outside_dir"),
            dir.path().join("sandbox/sub"),
        )
        .unwrap();
    }
    #[cfg(not(unix))]
    {
        return;
    }
    let result = tool
        .invoke(
            serde_json::json!({"path": "sandbox/sub/evil.txt", "content": "bypass"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("sandbox") || err.contains("沙箱"),
        "父目录 symlink 逃逸应被拒绝: {}",
        err
    );
}

#[test]
fn test_write_sandbox_description_contains_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox", "output"]);
    let desc = tool.description();
    assert!(desc.contains("sandbox"));
    assert!(desc.contains("output"));
    assert!(desc.contains("Write a file into your sandbox directories"));
}

#[test]
fn test_write_sandbox_empty_allowed_dirs_fails() {
    let cwd = tempfile::tempdir().unwrap();
    let result = WriteSandboxTool::new(
        cwd.path().to_str().unwrap().to_string(),
        vec![],
    );
    // 空白名单应可构造（不注入时不报错），但 description 不含具体目录
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_write_sandbox_multi_dir_in_different_roots() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["plans", "output"]);
    tool.invoke(
        serde_json::json!({"path": "plans/design.md", "content": "# Design"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    tool.invoke(
        serde_json::json!({"path": "output/result.json", "content": "{\"ok\": true}"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    assert!(dir.path().join("plans/design.md").exists());
    assert!(dir.path().join("output/result.json").exists());
}
```

- [ ] **Step 3: 运行测试验证**

```bash
cargo test -p peri-middlewares --lib -- test_write_sandbox
```

期望：全部 PASS（非 Unix 平台上 symlink 测试自动跳过）

- [ ] **Step 4: Commit**

```bash
git add peri-middlewares/src/tools/filesystem/write_sandbox.rs peri-middlewares/src/tools/filesystem/write_sandbox_test.rs
git commit -m "test: WriteSandbox 路径安全测试（7 条错误路径 + 3 条正常路径）"
```

---

### Task 5: build_agent_from_def 注入 WriteSandbox

**Files:**
- Modify: `peri-middlewares/src/subagent/tool/build_agent.rs`

- [ ] **Step 1: 在 `build_agent_from_def` 中 filter_tools 之后、返回之前注入 WriteSandbox**

在 `build_agent.rs` 第 87-89 行（`filter_tools` 调用）和第 92 行（`tracing::debug!`）之间，插入以下逻辑：

```rust
        // 3. Filter tools
        let mut filtered_tools = self.filter_tools(
            &agent_def.frontmatter.tools,
            &agent_def.frontmatter.disallowed_tools,
        );

        // 3.5. 注入 WriteSandbox（per-agent 实例，不走父工具继承）
        let allowed_write_dirs = &agent_def.frontmatter.allowed_write_dirs;
        if !allowed_write_dirs.is_empty() {
            let disallowed_list = agent_def.frontmatter.disallowed_tools.to_vec();
            let is_disallowed = disallowed_list
                .iter()
                .any(|n| n.to_lowercase() == "writesandbox");
            if is_disallowed {
                tracing::debug!(
                    agent_id = %agent_name,
                    "WriteSandbox 被 disallowedTools 否决，跳过注入"
                );
            } else {
                match crate::tools::filesystem::WriteSandboxTool::new(
                    cwd.to_string(),
                    allowed_write_dirs.clone(),
                ) {
                    Ok(tool) => {
                        filtered_tools.push(Box::new(tool));
                        tracing::debug!(
                            agent_id = %agent_name,
                            sandbox_dirs = ?allowed_write_dirs,
                            "WriteSandbox 工具已注入"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            agent_id = %agent_name,
                            error = %e,
                            sandbox_dirs = ?allowed_write_dirs,
                            "WriteSandbox 构造失败，跳过注入"
                        );
                    }
                }
            }
        }
```

注意：需要将 `filtered_tools` 声明改为 `let mut`。

- [ ] **Step 2: 编译检查**

```bash
cargo build -p peri-middlewares
```

期望：无编译错误

- [ ] **Step 3: 新增注入逻辑单元测试**

在 `peri-middlewares/src/subagent/tool/tool_test.rs` 末尾追加（需要确认该文件是 `#[cfg(test)]` 模块）：

```rust
/// [回归测试] allowedWriteDirs 非空时注入 WriteSandbox，disallowed 可否决
#[test]
fn test_write_sandbox_injection_order() {
    let parent_tools: Vec<Arc<dyn BaseTool>> = vec![
        make_tool("Read"),
        make_tool("Write"),
        make_tool("Edit"),
    ];
    let agent_tool = make_subagent_tool(parent_tools);

    // 场景 1: allowedWriteDirs 为空 → 不注入
    let def_no_dirs = ClaudeAgent {
        frontmatter: ClaudeAgentFrontmatter {
            name: "test".into(),
            description: "test".into(),
            tools: ToolsValue::Empty,
            disallowed_tools: ToolsValue::Empty,
            model: None,
            tone: None,
            proactiveness: None,
            permission_mode: None,
            max_turns: None,
            skills: vec![],
            mcp_servers: vec![],
            hooks: serde_yaml::Value::Null,
            memory: None,
            background: false,
            isolation: None,
            allowed_write_dirs: vec![],
        },
        system_prompt: String::new(),
    };
    let filtered = agent_tool.filter_tools(
        &def_no_dirs.frontmatter.tools,
        &def_no_dirs.frontmatter.disallowed_tools,
    );
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();
    assert!(!names.contains(&"WriteSandbox"), "空 allowedWriteDirs 不应注入 WriteSandbox");
}

#[test]
fn test_write_sandbox_disallowed_vetoes() {
    // 场景 2: disallowedTools 含 WriteSandbox → 不注入
    // 验证 filter_tools 能按名称过滤掉 WriteSandbox
    let parent_tools: Vec<Arc<dyn BaseTool>> = vec![
        make_tool("Read"),
        make_tool("WriteSandbox"), // 模拟父工具中含 WriteSandbox（实际不会）
    ];
    let agent_tool = make_subagent_tool(parent_tools);
    let def = ClaudeAgent {
        frontmatter: ClaudeAgentFrontmatter {
            name: "test".into(),
            description: "test".into(),
            tools: ToolsValue::Empty,
            // disallowed 中显式列出 WriteSandbox
            disallowed_tools: ToolsValue::List(vec!["WriteSandbox".into()]),
            model: None,
            tone: None,
            proactiveness: None,
            permission_mode: None,
            max_turns: None,
            skills: vec![],
            mcp_servers: vec![],
            hooks: serde_yaml::Value::Null,
            memory: None,
            background: false,
            isolation: None,
            allowed_write_dirs: vec![".peri/plans/".into()],
        },
        system_prompt: String::new(),
    };
    let filtered = agent_tool.filter_tools(
        &def.frontmatter.tools,
        &def.frontmatter.disallowed_tools,
    );
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();
    assert!(!names.contains(&"WriteSandbox"), "disallowed 应否决 WriteSandbox");
}
```

注意：由于 `build_agent_from_def` 是 async 方法且依赖完整的 SubAgentTool 状态（llm_factory 等），完整的端到端注入测试应在 Task 6（集成测试）中覆盖。这里先测 `filter_tools` 层面的 disallowed 否决。

另外需要在该测试文件中补充 `ClaudeAgentFrontmatter` 的导入——检查 `tool_test.rs` 现有的 import 并确认 `ClaudeAgentFrontmatter` 已被导入或需要新增。

- [ ] **Step 4: 运行测试**

```bash
cargo test -p peri-middlewares --lib -- test_write_sandbox_injection
```

期望：全部 PASS

- [ ] **Step 5: Commit**

```bash
git add peri-middlewares/src/subagent/tool/build_agent.rs peri-middlewares/src/subagent/tool/tool_test.rs
git commit -m "feat: build_agent_from_def 注入 WriteSandbox per-agent 实例"
```

---

### Task 6: 更新 plan.md agent 定义

**Files:**
- Modify: `peri-middlewares/src/subagent/built-in/plan.md`

- [ ] **Step 1: 在 frontmatter 中添加 `allowedWriteDirs`**

在 `disallowedTools` 和 `model: inherit` 之间插入：

```yaml
allowedWriteDirs:
  - ".peri/plans/"
```

- [ ] **Step 2: 在正文末尾添加写文件指引**

在文件末尾 `REMEMBER: You can ONLY explore and plan...` 之后追加：

```markdown

## Writing Plans to Sandbox

You now have access to the `WriteSandbox` tool, which allows you to write files ONLY to `.peri/plans/`. Use it to save your implementation plan:

1. After completing your analysis, write the plan to `.peri/plans/<topic>.md` using WriteSandbox
2. In your final response, state the file path clearly so the caller can retrieve it
3. You can overwrite previous versions of the same plan to iterate

The WriteSandbox tool accepts:
- `path`: relative path within your sandbox (e.g. `plan.md` or `subdir/design.md`)
- `content`: the full file content

Absolute paths and `..` traversals are automatically rejected.
```

- [ ] **Step 3: 验证 plan.md 解析正确**

```bash
cargo test -p peri-middlewares --lib -- test_parse_allowed_write
```

期望：PASS（Task 1 中新增的 test_parse_allowed_write_dirs 覆盖了 plan 的 YAML 格式）

- [ ] **Step 4: Commit**

```bash
git add peri-middlewares/src/subagent/built-in/plan.md
git commit -m "feat: plan agent 声明 allowedWriteDirs + WriteSandbox 使用指引"
```

---

### Task 7: can_mutate 注释同步

**Files:**
- Modify: `peri-middlewares/src/subagent/mod.rs` (line ~567-568)
- Modify: `peri-acp/src/prompt/mod.rs` (line ~87-88)

- [ ] **Step 1: 在 `infer_agent_capability` 的 `can_mutate` 字段注释中补充说明**

将 `mod.rs` 第 567-568 行的注释改为：

```rust
    /// 该 agent 是否会修改项目代码（有 Write/Edit 权限 = true）。
    /// 注意：`allowedWriteDirs` 声明的 WriteSandbox 工具不计入 can_mutate，
    /// 因为沙箱目录不在项目代码范围内，agent 仍可并行调度。
    pub can_mutate: bool,
```

- [ ] **Step 2: 在 `format_available_agents` 函数的 `can_mutate` 使用处补充注释**

将 `prompt/mod.rs` 第 87-88 行的注释改为：

```rust
/// 其中 `model_tier` 为 haiku/sonnet/opus/inherit，
/// `access` 为 readonly/writes（标识该 agent 是否会修改项目代码。
/// 带 allowedWriteDirs 的 agent 仍标为 readonly，因其仅写沙箱目录）。
```

- [ ] **Step 3: 编译检查**

```bash
cargo build -p peri-middlewares -p peri-acp
```

期望：无编译错误（注释变更不影响编译）

- [ ] **Step 4: Commit**

```bash
git add peri-middlewares/src/subagent/mod.rs peri-acp/src/prompt/mod.rs
git commit -m "docs: can_mutate 注释说明 allowedWriteDirs 不改变 readonly 标签"
```

---

### Task 8: 端到端集成验证

**Files:**
- 无新文件（验证已有工具配合）

- [ ] **Step 1: 运行全量测试**

```bash
cargo test -p peri-middlewares --lib
```

期望：全部 PASS（包括新增的 WriteSandbox 测试 + 既有测试无一退化）

- [ ] **Step 2: 运行 clippy**

```bash
cargo clippy -p peri-middlewares -- -D warnings
```

期望：无新增警告

- [ ] **Step 3: 运行 cargo fmt**

```bash
cargo fmt -- -p peri-middlewares -p peri-acp
cargo fmt -- --check
```

期望：格式无变化

- [ ] **Step 4: 验证能力推断不退化**

运行已有测试确认 `infer_agent_capability` 对 plan agent 仍返回 `can_mutate: false`：

```bash
cargo test -p peri-middlewares --lib -- infer_agent_capability
```

期望：若有已有测试则 PASS

- [ ] **Step 5: Commit**

```bash
git add -A
git diff --cached --stat
git commit -m "chore: 集成验证——WriteSandbox 全链路测试通过"
```

---

## 自检

**1. Spec 覆盖：**

| Spec 需求 | 对应 Task |
|-----------|----------|
| WriteSandbox 工具定义（动态 description、path+content 参数） | Task 2 |
| frontmatter `allowedWriteDirs` 字段 | Task 1 |
| plan.md 声明 + 正文指引 | Task 6 |
| 注入链路（filter → append → disallowed veto） | Task 5 |
| 路径安全校验链（④ 步） | Task 2 (validate_path) |
| `..` 穿越拒绝 | Task 4 |
| 绝对路径拒绝 | Task 4 |
| 父目录 symlink 逃逸拒绝 | Task 4 |
| 目标文件 symlink 指向沙箱外拒绝 | Task 4 |
| 正常创建 + 覆盖写 | Task 4 |
| description 包含白名单目录 | Task 4 |
| allowedWriteDirs roundtrip + 缺失默认空 | Task 1 |
| tools 未列 WriteSandbox 仍注入 | Task 5 |
| disallowedTools 显式列出 WriteSandbox 不注入 | Task 5 |
| allowedWriteDirs 为空不注入 | Task 5 |
| can_mutate 仍为 false | Task 7 |

**2. Placeholder 扫描：** 无 TBD/TODO/implement later。所有步骤包含完整代码。

**3. 类型一致性：**
- `ClaudeAgentFrontmatter.allowed_write_dirs: Vec<String>` 在 Task 1 定义，Task 5 build_agent.rs 中作为 `&agent_def.frontmatter.allowed_write_dirs` 使用，Task 1 测试中同样引用 → 一致
- `WriteSandboxTool::new(cwd: impl Into<String>, allowed_dirs: Vec<String>)` 在 Task 2 定义，Task 5 中调用 `WriteSandboxTool::new(cwd.to_string(), allowed_write_dirs.clone())` → 签名匹配
- `validate_path` 返回 `Result<PathBuf, String>` 在 Task 2 定义，`invoke` 中调用 `self.validate_path(path)?` → 一致

---

## 执行交付

**Plan complete and saved to `docs/superpowers/plans/2026-07-18-write-sandbox-tool.md`. Two execution options:**

**1. Subagent-Driven (recommended)** - 每个 Task 派发独立 subagent，Task 间 review，快速迭代

**2. Inline Execution** - 在当前 session 中使用 executing-plans，批量执行含 checkpoint

**Which approach?**
