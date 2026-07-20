# Sandboxed Write Tool：用 Write + allowedWriteDirs 替代 WriteSandbox

**日期**：2026-07-20  
**状态**：设计完成，待实现  
**作者**：deepseek-v4-pro

---

## 1. 问题陈述

当前 SubAgent 对 `Write` 和 `allowedWriteDirs` 的处理方式存在两个独立工具，造成概念分裂：

- **Write**（核心工具）— 无目录限制，LLM 直接调用
- **WriteSandbox**（专用工具）— 声明 `allowedWriteDirs` 时注入，受目录限制

这导致：

1. **LLM 需知道两个工具名** — explorer 要写报告用 `WriteSandbox`，coder 正常写用 `Write`。增加认知负担
2. **代码重复** — WriteSandbox 的原子写入逻辑与 WriteFileTool 几乎一样（原子 temp + rename + 权限保留），独立维护
3. **概念分裂** — "写文件"这个操作被拆成两个工具，新 agent 作者需理解何时用哪个

---

## 2. 方案选择

**选择方案 B — SandboxedWrite Wrapper**：

- 不改动 `WriteFileTool`（开闭原则）
- 创建 `SandboxedWriteTool` wrapper，name="Write"，描述动态追加目录限制
- `invoke()` 先校验路径 → 委托 `WriteFileTool.invoke()`
- 删除 `WriteSandboxTool`，路径校验逻辑提取为公共 `validate_sandbox_path()` guard
- 风格与现有 `ArcToolWrapper`/`BoxToolWrapper` 一致

---

## 3. 架构对比

### 改造前

```
explorer agent
├── Glob, Grep, Read, WebFetch...
├── WriteSandbox tool (name="WriteSandbox")
│   └── validate + write (重复实现)
└── Write tool → disallowed，不可见

coder agent
├── Write tool (name="Write") — 无限制
└── 无 WriteSandbox
```

### 改造后

```
explorer agent
├── Glob, Grep, Read, WebFetch...
└── Write tool (name="Write")  ← SandboxedWriteTool wrapper
    ├── desc: "WRITE_DESC\n**Restriction**: .peri/plans/"
    ├── invoke: validate_sandbox_path → WriteFileTool.invoke
    └── WriteFileTool (inner, 不改动)

coder agent
├── Write tool (name="Write") — 无限制（无 allowedWriteDirs，不包装）
└── 不变
```

---

## 4. 组件设计

### 4.1 `validate_sandbox_path` — 公共路径校验 guard

**位置**：`peri-middlewares/src/tools/filesystem/sandbox_guard.rs`（新建）

**来源**：从 `WriteSandboxTool::validate_path()` 提取，逻辑不变

**签名**：

```rust
/// 校验写入路径是否在 sandbox 白名单内
///
/// 返回 canonicalized 后的绝对路径，若路径逃逸则返回错误
pub fn validate_sandbox_path(
    cwd: &str,
    file_path: &str,
    allowed_dirs: &[PathBuf],
) -> Result<PathBuf, SandboxPathError>
```

**5 层校验链**（继承自 WriteSandbox）：
1. 拒绝绝对路径
2. 拒绝 `..` 路径穿越
3. 最长已存在祖先 `canonicalize` + 沙箱前缀检查
4. 创建剩余父目录 + 再 canonicalize
5. 最终路径前缀匹配白名单

**错误类型** `SandboxPathError`：

```rust
#[derive(Debug, thiserror::Error)]
pub enum SandboxPathError {
    #[error("Absolute paths are not allowed: {path}")]
    AbsolutePath { path: String },
    #[error("Path traversal detected: {path}")]
    PathTraversal { path: String },
    #[error("Path '{path}' is outside allowed directories: {dirs:?}")]
    OutsideSandbox { path: String, dirs: Vec<PathBuf> },
    #[error("Failed to create parent directory: {source}")]
    CreateDirError { source: std::io::Error },
    #[error("Failed to canonicalize path: {source}")]
    CanonicalizeError { source: std::io::Error },
}
```

### 4.2 `SandboxedWriteTool` — Wrapper

**位置**：`peri-middlewares/src/tools/filesystem/sandboxed_write.rs`（新建）

```rust
pub struct SandboxedWriteTool {
    inner: WriteFileTool,          // 不改动的核心工具
    sandbox_dirs: Vec<PathBuf>,    // 允许写入的目录（canonicalized）
    desc: String,                  // 动态描述（构造时预生成）
}
```

**构造器**：

```rust
impl SandboxedWriteTool {
    pub fn new(cwd: impl Into<String>, allowed_dirs: Vec<String>) -> Self {
        let cwd = cwd.into();
        let sandbox_dirs: Vec<PathBuf> = allowed_dirs.iter()
            .map(|d| Self::canonicalize_sandbox_dir(&cwd, d))
            .collect::<Result<_, _>>()
            .expect("Sandbox dir must be valid"); // 目录不存在时自动创建

        let desc = format!(
            "{}\n\n**Restriction**: You can only write to files within the following directories:\n{}",
            WRITE_FILE_DESCRIPTION,
            sandbox_dirs.iter()
                .map(|d| format!("- {}", d.display()))
                .collect::<Vec<_>>()
                .join("\n")
        );

        Self { inner: WriteFileTool::new(cwd), sandbox_dirs, desc }
    }
}
```

**BaseTool trait 实现**：

| 方法 | 实现 |
|------|------|
| `name()` | `"Write"` |
| `description()` | `&self.desc`（动态生成） |
| `parameters()` | 委托 `self.inner.parameters()` |
| `timeout()` | 委托 `self.inner.timeout()` |
| `aliases()` | 委托 `self.inner.aliases()` |
| `invoke(input, ctx)` | 见下方 invoke 流程 |

**invoke 流程**：

```
1. 从 input 提取 file_path, content, append
2. resolve_path(cwd, file_path) → 相对路径解析为绝对路径
3. validate_sandbox_path(cwd, file_path, &sandbox_dirs) → 5 层校验
   ├─ 失败 → 返回 SandboxPathError（含目录白名单信息）
   └─ 通过 → canonicalized_path
4. inner.invoke(input, ctx) → 委托 WriteFileTool 执行原子写入
```

### 4.3 现有 WriteSandboxTool 处理

- `validate_path()` 内部逻辑 → 替换为调用公共 `validate_sandbox_path()`
- `invoke()` 内写入逻辑保留（过渡期），添加 `#[deprecated]` 标记
- 模块注册从 `pub use` → 改为 `#[allow(deprecated)]` 内部使用
- **清理时间**：确认无外部依赖后可物理删除文件

---

## 5. build_agent_from_def 集成

**位置**：`peri-middlewares/src/subagent/tool/build_agent.rs`

**当前逻辑**（lines 92-127）：

```rust
// (3.5) 注入 WriteSandbox
if let Some(allowed_write_dirs) = &agent_def.frontmatter.allowed_write_dirs {
    if !allowed_write_dirs.is_empty() {
        let tool = WriteSandboxTool::new(&cwd, allowed_write_dirs.clone());
        if !self.is_disallowed("WriteSandbox") {
            filtered_tools.push(Box::new(tool));
        }
    }
}
```

**改造后逻辑**：

```rust
// (3.5) 注入 sandboxed Write（替代 WriteSandbox）
if let Some(allowed_write_dirs) = &agent_def.frontmatter.allowed_write_dirs {
    if !allowed_write_dirs.is_empty() && !self.is_disallowed("Write") {
        // 移除父工具集中未经限制的 Write
        filtered_tools.retain(|t| t.name() != "Write");
        // 注入受目录限制的沙箱版 Write
        filtered_tools.push(Box::new(SandboxedWriteTool::new(
            &cwd,
            allowed_write_dirs.clone(),
        )));
    }
}
```

**关键规则**：
- `allowedWriteDirs` 非空 **且** `Write` 未被 `disallowedTools` 排除 → 注入沙箱 Write
- `Write` 被 disallow 时 → `allowedWriteDirs` 不生效（disallowedTools 优先）
- coder 无 `allowedWriteDirs` → 不触发，Write 保持原样

---

## 6. 内置 Agent 定义变更

### 6.1 explorer.md

```yaml
# 改前
disallowedTools: [Agent, Write, Edit, Bash]

# 改后
disallowedTools: [Agent, Edit, Bash]
```

system prompt 变更：
- **删除**：`=== CRITICAL: READ-ONLY MODE` 整段（6-22 行）中关于 Write/Edit 的提及
- **删除**：`## Writing Reports to Sandbox` 整节（39-47 行）— WriteSandbox 使用说明
- **调整**：`NOTE` 段落中删除与 WriteSandbox 相关的指引

### 6.2 plan.md

```yaml
# 改前
disallowedTools: [Agent, Bash]

# 改后（不变，plan 本来就不 disallow Write）
disallowedTools: [Agent, Bash]
```

system prompt 变更：
- **删除**：WriteSandbox 使用说明段落
- Write 的描述已动态包含限制，无需额外说明

### 6.3 verification.md

```yaml
# 改前
disallowedTools: [Agent, Bash]

# 改后（不变）
disallowedTools: [Agent, Bash]
```

system prompt 变更：同 plan.md，删除 WriteSandbox 使用说明段落。

---

## 7. 文件变更清单

| 操作 | 文件 | 说明 |
|------|------|------|
| **新建** | `peri-middlewares/src/tools/filesystem/sandbox_guard.rs` | 公共路径校验函数 |
| **新建** | `peri-middlewares/src/tools/filesystem/sandboxed_write.rs` | SandboxedWriteTool wrapper |
| **修改** | `peri-middlewares/src/tools/filesystem/mod.rs` | 注册新模块 |
| **修改** | `peri-middlewares/src/tools/filesystem/write_sandbox.rs` | 委托给公共 guard + `#[deprecated]` |
| **修改** | `peri-middlewares/src/subagent/tool/build_agent.rs` | 替换注入逻辑 |
| **修改** | `peri-middlewares/src/subagent/built-in/explorer.md` | 移除 Write from disallowedTools + 删除 WriteSandbox 说明 |
| **修改** | `peri-middlewares/src/subagent/built-in/plan.md` | 删除 WriteSandbox 说明 |
| **修改** | `peri-middlewares/src/subagent/built-in/verification.md` | 删除 WriteSandbox 说明 |
| **修改** | `peri-middlewares/src/claude_agent_parser/mod.rs` | 无结构变更，仅确保 allowed_write_dirs 字段注解正确 |
| **删除** | `peri-middlewares/src/tools/filesystem/write_sandbox.rs` | 后续清理（过渡期标记 deprecated） |

---

## 8. LLM 交互设计

### 8.1 工具列表

explorer agent 的 LLM 请求中，工具列表包含：

```json
{
  "name": "Write",
  "description": "Writes a file to the local filesystem.\n...\n\n**Restriction**: You can only write to files within the following directories:\n- /path/to/project/.peri/plans",
  "parameters": { /* 同标准 Write */ }
}
```

### 8.2 典型交互

```
LLM: 我想保存探索报告
  → Write(file_path="report.md", content="# Findings\n...")

SandboxedWriteTool:
  → resolve_path → "/abs/path/to/project/.peri/plans/report.md"
  → validate_sandbox_path → ✓ 在 .peri/plans/ 下
  → WriteFileTool.invoke → 原子写入成功
  → 返回: "Successfully wrote to .peri/plans/report.md"

LLM: 我想修改系统文件（恶意尝试）
  → Write(file_path="/etc/passwd", content="...")

SandboxedWriteTool:
  → validate_sandbox_path → ✗ 绝对路径拒绝
  → 返回错误: "Absolute paths are not allowed: /etc/passwd"
```

### 8.3 错误反馈

目录外写入时返回的错误信息包含白名单提示：

```
Path '/etc/hosts' is outside allowed directories: ["/project/.peri/plans"]
```

LLM 从错误中获知限制范围，无需提前在 prompt 中长篇说明。

---

## 9. 错误处理

| 场景 | 行为 |
|------|------|
| `allowedWriteDirs` 配置的目录不存在 | `SandboxedWriteTool::new()` 自动创建 |
| file_path 是绝对路径 | 拒绝，返回 `SandboxPathError::AbsolutePath` |
| file_path 包含 `..` | 拒绝，返回 `SandboxPathError::PathTraversal` |
| file_path 解析后在沙箱外 | 拒绝，返回 `SandboxPathError::OutsideSandbox`（含白名单列表） |
| symlink 逃逸 | `canonicalize` 后前缀匹配失败，拒绝 |
| `Write` 在 disallowedTools 中 | allowedWriteDirs 不生效，不注入沙箱 Write |
| `allowedWriteDirs` 为空数组 | 不触发注入，行为不变 |

---

## 10. 测试策略

### 10.1 单元测试

**P0**：

- `test_validate_sandbox_path_relative_ok` — 正常相对路径
- `test_validate_sandbox_path_absolute_rejected` — 绝对路径被拒
- `test_validate_sandbox_path_traversal_rejected` — `..` 穿越被拒
- `test_validate_sandbox_path_outside_dir` — 沙箱外目录被拒
- `test_sandboxed_write_tool_name` — name 返回 "Write"
- `test_sandboxed_write_tool_description_contains_restriction` — 描述含目录限制
- `test_sandboxed_write_tool_delegates_parameters` — parameters 委托给 inner
- `test_sandboxed_write_tool_invoke_success` — 合法路径写入成功
- `test_sandboxed_write_tool_invoke_outside_sandbox` — 非法路径写入失败

**P1**：

- `test_write_not_injected_when_disallowed` — Write disallowed 时不注入沙箱版
- `test_write_not_replaced_when_no_allowed_dirs` — 无 allowedWriteDirs 时不替换
- `test_sandboxed_write_append_mode` — append=true 追加写入

### 10.2 集成测试

- `test_explorer_agent_can_write_to_plans` — explorer 能写 .peri/plans/
- `test_explorer_agent_cannot_write_outside_sandbox` — explorer 不能写沙箱外
- `test_coder_agent_write_unchanged` — coder 的 Write 行为不变

### 10.3 回归测试

- 现有 `write_sandbox_test.rs` 中的路径校验测试 → 迁移到 sandbox_guard 测试
- `build_agent` 测试确保工具注入逻辑不变

---

## 11. 迁移兼容性

### 对现有 agent 定义的影响

| Agent | 影响 |
|-------|------|
| explorer（内置） | `disallowedTools` 移除 Write，由本设计覆盖 |
| plan（内置） | system prompt 中 WriteSandbox 说明删除，无配置变更 |
| verification（内置） | 同 plan |
| coder（内置） | 无影响 |
| 外部/插件 agent 使用 `WriteSandbox` | WriteSandbox 标记 deprecated，外部 agent 需迁移到 Write + allowedWriteDirs |

### 回滚方案

若出现问题，可回滚 `build_agent.rs` 中的注入逻辑恢复 WriteSandbox 注入，同时对 explorer.md 恢复 Write 到 disallowedTools。

---

## 12. Spec 自审查

- [x] 无 TODO / TBD 占位符
- [x] 各节之间无矛盾（架构图与集成代码一致）
- [x] 范围聚焦：单一 SandboxedWrite 替换 WriteSandbox，不涉及其他重构
- [x] 无歧义：边界规则表格 + 错误场景表格覆盖所有分支
- [x] 测试策略覆盖 P0/P1/回归
- [x] 迁移路径明确（deprecated → 清理）
