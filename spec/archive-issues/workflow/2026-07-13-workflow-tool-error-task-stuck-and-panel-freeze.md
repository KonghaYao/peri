> 归档于 2026-07-18，原路径 spec/issues/2026-07-13-workflow-tool-error-task-stuck-and-panel-freeze.md
# Workflow Tool 快速失败后，BgTaskArea 任务条目永久卡在黄色 ◎

**状态**：Fixed
**Triage**：ready-for-agent
**优先级**：中
**分类**：Bug
**创建日期**：2026-07-13

---

## 问题描述

当 LLM 调用 Workflow Tool 并且 workflow 在启动后 1 秒内快速失败（脚本语法错误、路径不存在等）时，Workflow Tool 正确地向 LLM 返回了错误信息，但 TUI 底部 `BgTaskArea` 中对应的 workflow 任务条目始终显示黄色 ◎（空闲/运行中状态），永远不会变为 ✔ 或 ✗。用户只能等条目超时后自然消失。

**期望行为**：task 条目应在 workflow 失败后立即更新为 ✗（失败），并在 3 秒后自然消失。

---

## 症状详情

`peri-workflow/src/tool.rs` 的 `invoke()` 方法中存在时序问题：

1. **第 224-232 行**：workflow 已被注册到 `BgTaskRegistry`（`bg.register_workflow(...)`），此时 BG_DISPLAY 中显示黄色 ◎ 条目
2. **第 235-273 行**：快速失败检测逻辑——在 1 秒内检测到 workflow 失败后，直接 `return Err(...)` 返回错误给 LLM
3. **第 278 行**：通知任务 `tokio::spawn(async move { ... })` 的代码位置在快速失败检测 **之后**——负责等待 workflow 完成并调用 `complete_workflow()` 来更新 BG_DISPLAY 状态

当快速失败检测命中时，代码在第 267 行提前返回，跳过了第 278 行的通知任务 spawn。因此 `complete_workflow()`（`peri-middlewares/src/workflow/mod.rs:54`）永远不会被调用，BG_DISPLAY 条目状态永远停留在 ◎。

---

## 复现条件

- **复现频率**：必现
- **触发步骤**：使用 Workflow Tool 启动一个包含语法错误或无效 `scriptPath` 的 workflow 脚本
- **环境**：任何模型、macOS / Linux

---

## 涉及文件

- `peri-workflow/src/tool.rs` —— `invoke()` 方法，快速失败 `return Err()` 跳过了通知任务 spawn
- `peri-middlewares/src/workflow/mod.rs` —— `BgTaskRegistry` trait 实现，`complete_workflow()` 在快速失败路径中不会被调到

---

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-13 | — | Open | agent | 创建（原含 Bug B，已修复并分离） |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
