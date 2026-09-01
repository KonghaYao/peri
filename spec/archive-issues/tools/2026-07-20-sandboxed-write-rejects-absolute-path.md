# SandboxedWrite 工具的 file_path 描述与校验逻辑矛盾——LLM 传入绝对路径被拒绝

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-20

## 问题描述

`SandboxedWriteTool`（`36b0aef3` 引入，替代 WriteSandbox）的 `parameters()` 委托给了内部 `WriteFileTool`，后者明确要求 `file_path` 为绝对路径（"must be absolute, not relative"）。但 `validate_sandbox_path` 在校验链第一层就拒绝所有绝对路径，导致 LLM 严格遵循参数描述传入的绝对路径被拦截报错。100% 必现，所有声明了 `allowedWriteDirs` 的 subagent（plan、explorer、verification）调用 Write 写 `.peri/plans/` 等沙箱目录时均失败。

## 症状详情

| 维度 | 期望行为 | 实际行为 |
|------|----------|----------|
| LLM 传入绝对路径 | 沙箱校验接受在沙箱范围内的绝对路径 | `Absolute paths are not allowed: /Users/.../.peri/plans/...` |
| 参数描述一致性 | 工具描述与校验逻辑一致 | `parameters()` 说 "must be absolute"，`validate_sandbox_path` 第一行拒绝绝对路径 |

**用户观察到的错误**：

```
● Write (/Users/konghayao/code/ai/perihelion/.peri/plans/2026-07-20-read-tool-directory-handling-folder-deep-scan-explore.md)
  ⎿ Tool execution failed: Write - Absolute paths are not allowed: /Users/konghayao/code/ai/perihelion/.peri/plans/2026-07-20-read-tool-directory-handling-folder-deep-scan-explore.md

● Write (/Users/konghayao/code/ai/perihelion/.peri/plans/explore-read-folder.md)
  ⎿ Tool execution failed: Write - Absolute paths are not allowed: /Users/konghayao/code/ai/perihelion/.peri/plans/explore-read-folder.md
```

## 复现条件

- **复现频率**：100% 必现
- **触发步骤**：
  1. 派发声明了 `allowedWriteDirs` 的 subagent（plan / explorer / verification）
  2. subagent 调用 Write 工具尝试写入沙箱目录下的文件
  3. LLM 看到 `parameters()` 要求绝对路径，传入绝对路径
  4. `validate_sandbox_path` 第一层校验直接拒绝
- **环境**：任何环境，与 commit `36b0aef3` 之后

## 矛盾链

```
SandboxedWriteTool.parameters() → 委托 inner (WriteFileTool)
  → parameters() 描述："The absolute path to the file to write (must be absolute, not relative)"
  → LLM 看到 → 传入 "/Users/.../.peri/plans/foo.md"

SandboxedWriteTool.invoke()
  → validate_sandbox_path(cwd, file_path, sandbox_roots)
  → 第一层校验：Path::new(file_path).is_absolute() → true → Err(AbsolutePath)
  → LLM 收到 "Absolute paths are not allowed"
```

两个组件各说各话：`parameters()` 告诉 LLM "传绝对路径"，`validate_sandbox_path` 告诉 LLM "不许传绝对路径"。

## 关联历史

- 当时未采纳的 Sandboxed Write 方案未考虑此矛盾——`parameters()` 委托行为在方案中未被提及；过程方案已从 `docs/` 删除，可由 Git 历史追溯
- `36b0aef3`（feat: replace WriteSandbox with SandboxedWriteTool）引入问题
- `580ceef3`（test: add 27 adversarial path validation tests）中的 `test_sandboxed_write_invoke_absolute_rejected` **断言了绝对路径应被拒绝**，与 `parameters()` 描述的 "must be absolute" 直接冲突
- `spec/issues/2026-07-20-plan-agent-writesandbox-not-found.md` 是前一个问题（沙箱目录不存在导致工具构造失败），已通过 `ab8786e8` 修复，但被 `36b0aef3` 重构取代后引入了新的矛盾

## 涉及文件

- `peri-middlewares/src/tools/filesystem/sandboxed_write.rs:99-101` —— `parameters()` 委托给 inner WriteFileTool，继承了 "file_path must be absolute" 描述
- `peri-middlewares/src/tools/filesystem/sandbox_guard.rs:44-48` —— `validate_sandbox_path` 第一层校验无条件拒绝绝对路径
- `peri-middlewares/src/tools/filesystem/sandboxed_write_test.rs:82-99` —— `test_sandboxed_write_invoke_absolute_rejected` 断言绝对路径被拒绝，与参数描述冲突
- `peri-middlewares/src/tools/filesystem/write.rs:47-49` —— WriteFileTool 的 `file_path` 参数描述要求绝对路径

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-20 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
