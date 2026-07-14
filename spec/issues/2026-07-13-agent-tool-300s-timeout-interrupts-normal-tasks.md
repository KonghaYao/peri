# 工具调用统一 300s 超时导致 Agent/SubAgent 正常任务被强制中断

**状态**：Fixed
**优先级**：中
**类型**：Bug
**创建日期**：2026-07-13

## 问题描述

`peri-agent/src/agent/stages/tool_dispatch.rs:35` 定义了 `TOOL_CALL_TIMEOUT = Duration::from_secs(300)`，在 `dispatch_concurrent`（L375）中对**所有**工具调用统一使用 `tokio::time::timeout` 包裹。Agent/SubAgent 工具也会被这个 300s 外层超时覆盖——当 Agent/SubAgent 正在正常执行复杂任务（如读代码、修改文件、多轮 LLM 推理）时，到 300s 就被硬性 kill，返回 "tool call timed out after 300s"，任务中断。

## 症状详情

| 场景 | 期望行为 | 实际行为 |
|------|---------|---------|
| fork Agent 执行复杂任务（多轮 ReAct 循环） | 任务正常完成，不因超时中断 | 300s 后被强制 kill，返回 timeout 错误 |
| bg Agent 执行长时间任务 | 后台任务正常完成 | 300s 后被强制 kill |

**触发条件**：Agent/SubAgent 工具的总执行时间超过 300s（单次 fork/bg 调用包含最多 200 轮 ReAct 迭代，每轮 LLM 调用 + 工具分发，复杂任务极易超过 300s）。

## 复现条件

- **复现频率**：Agent/SubAgent 处理复杂任务（多文件修改、长链路推理）时必现
- **触发步骤**：
  1. 主 agent 调用 Agent(fork: true) 或 Agent(run_in_background: true) 执行复杂任务
  2. 子 agent 的 run_react_loop 正常运行，但总耗时超过 300s
  3. 外层 `tool_dispatch.rs` 的 `tokio::time::timeout(300s, ...)` 触发
  4. 子 agent 被强制中断，返回 "tool call timed out after 300s"

## 涉及文件

- `peri-agent/src/agent/stages/tool_dispatch.rs:35` —— `TOOL_CALL_TIMEOUT = 300s`，对所有工具统一生效（L375）
- `peri-middlewares/src/subagent/tool/execute_fork.rs:161` —— fork Agent 的 `run_react_loop`，完全被外层 300s 包裹，无独立超时
- `peri-middlewares/src/subagent/tool/execute_bg.rs:214` —— bg Agent 的 `run_react_loop`，同样无独立超时

## 背景

300s 超时最初作为 P0 防御措施引入（见 `spec/issues/2026-07-08-peri-agent-code-quality-improvement.md` 子项 2），目的是防止恶意/死循环工具调用永久阻塞 turn。但当时设计未区分工具类型——Read/Write/Grep 等毫秒级操作和 Agent/SubAgent（可跑 200 轮 ReAct）共用同一个 300s 上限。

各层已有超时一览：

| 层级 | 超时 | 位置 |
|------|------|------|
| 工具分发外层（统一） | 300s | `tool_dispatch.rs:35` |
| MCP 工具 | 120s | `tool_bridge.rs:40` |
| Bash | 默认 120s，上限 600s | `terminal.rs:299` |
| Agent/SubAgent | 无独立超时 | — |

## 期望改进方向

需要讨论：Agent/SubAgent 是唯一需要长时间运行（分钟级）的工具类型，是否应该：

- 按工具类型差异化超时（Agent 更长，快工具更短）
- 统一提高上限
- Agent 取消硬超时，依赖 max_iterations / 用户手动 cancel

具体方案待讨论确定。

## 关联 Issue

- `spec/issues/2026-07-08-peri-agent-code-quality-improvement.md`（Partial）—— 引入了 300s 统一超时
- `spec/issues/2026-07-11-hung-bg-agent-await-wake-block-forever.md`（Open）—— bg agent await_wake 阻塞，讨论过 bg 级 600s 超时方案

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-13 | — | Open | agent | 创建 |
| 2026-07-13 | Open | Fixed | agent | 按工具类型差异化超时方案实施完毕 |

## 修复记录

### 修复 #1（2026-07-13）

- **操作人**：agent
- **用户原意**：Agent/SubAgent 不需要硬超时，其他工具按各自特点设计超时
- **修复内容**：
  - `BaseTool` trait 新增 `timeout() -> Option<Duration>` 方法，默认 `Some(120s)`
  - 12 个工具覆写为 `None`（自管超时或无需外层）：Agent、Bash、MCP bridge/resource、WebFetch/Search、Write、Grep、LSP、AskUserQuestion、Workflow、AgentResult
  - 12 个工具继承默认 120s：Read、Edit、Glob、folder_operations、TodoWrite、cron_* ×3、goal、SearchExtraTools、ExecuteExtraTool、artifact
  - `dispatch_concurrent` 删除硬编码 `TOOL_CALL_TIMEOUT = 300s`，改用 `tool.timeout()` 按工具查询
  - `BoxToolWrapper` / `ArcToolWrapper` 代理 `timeout()` 到内层工具
- **涉及文件**：`peri-agent/src/tools/mod.rs`、`peri-agent/src/agent/stages/tool_dispatch.rs`、`peri-middlewares/src/tools/mod.rs` + 12 个工具文件 + `peri-workflow/src/tool.rs`
- **验证状态**：待验证
