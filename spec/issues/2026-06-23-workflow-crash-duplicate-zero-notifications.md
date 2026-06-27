# Workflow 脚本运行中途 ReferenceError 后，出现两条统计数据矛盾的完成通知

**状态**：Open
**优先级**：中
**类型**：Bug
**创建日期**：2026-06-23
**关联**：`spec/issues/2026-06-23-workflow-sync-error-on-immediate-failure.md`（同类问题，不同触发路径）、`spec/issues/2026-06-23-workflow-defects-consolidated.md` B1（0 agents 统计问题）

## 问题描述

Workflow 脚本在正常执行若干 agent 后，于模块顶层抛出 ReferenceError 崩溃。此时系统发送了**两条**完成通知，且两条通知的统计数据互相矛盾：第一条来自 workflow runner，数据正确（3 agents）；第二条来自后台任务系统，数据全为 0（1ms, 0 agents, 0 tool calls）。第二条错误通知会造成误导。

## 症状详情

| 通知来源 | 内容 | 数据是否准确 |
|----------|------|-------------|
| Workflow runner（第一条 system-reminder） | `3 phases, 3 agents, 3 tool calls. RESULT: { error: "ReferenceError..." }` | ✅ 正确（实际执行了 3 个 agent） |
| 后台任务通知（第二条 system-reminder） | `1ms, 0 agents, 0 tool calls` | ❌ 时间错误、统计错误 |

**具体场景**：
- 脚本先通过 `await agent(...)` 和 `await parallel([...])` 正常执行了 3 个 agent
- 然后在模块顶层的同步代码中抛出 `ReferenceError`（`undefinedVariableThatDoesNotExist.someProperty`）
- state.json 和 journal.jsonl 未生成（运行目录仅含 `script.js`）

**问题维度**：
1. **统计矛盾**：两条通知对同一 workflow 报告的 agent 数和 tool calls 数不同
2. **无效通知**：引擎正确通知了 "Execution failed" 之后，不应再出一条全 0 的重复通知
3. **时间误导**："1ms" 不代表真实执行时间（agents 实际运行了若干秒）

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 准备一个在若干 `await agent(...)` 调用后抛出 `ReferenceError` 的 workflow 脚本
  2. 通过 Workflow tool 执行该脚本
  3. 观察 system-reminder 区域出现两条完成通知
- **环境**：macOS，peri workflow engine（`feature/workflow-ultracode` squash `b22196ab`）
- **测试脚本**：`.claude/scripts/workflow-broken-test.mjs`

## 涉及文件

- `peri-workflow/src/tool.rs` —— `invoke()` 返回 "started" 并 spawn notification task；第二条通知可能由此路径发出
- `peri-workflow/src/runner.rs` —— 处理 `workflow/done` RPC；第一条通知的 error 字段透传路径
- `peri-middlewares/src/workflow/mod.rs` —— WorkflowMiddleware，持有 notification_buffer_rx 用于完成通知转发

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-06-23 | — | Open | agent | 创建 |

## 修复记录

（待修复后追加）
