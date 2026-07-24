# Tool Batch 中快速工具的 Langfuse Latency 被慢工具拖高

- **状态**：Fixed
- **类型**：Bug（Langfuse 观测数据不准）
- **优先级**：中
- **创建时间**：2026-07-23
- **涉及文件**：
  - `peri-agent/src/agent/stages/tool_dispatch.rs`（dispatch_concurrent + settle_results）
  - `peri-acp/src/langfuse/tracer/tool_batch.rs`（ToolBatch 时间记录）
  - `peri-acp/src/langfuse/tracer/mod.rs`（emit_tools_flush）

## 问题描述

同一 tool batch 中的快速工具在 Langfuse 上显示的 latency 等于 batch 中最慢工具的 latency，无法反映真实执行时间。

## 症状详情

### 实际案例

在 session `019f8c5b-970b-7912-87a8-d2a5ab674a28` (trace `019f8c5bdc257331a142dc1ba989836d`) 中：

| 工具 | Langfuse 显示 latency | startTime | endTime | 实际执行时间 |
|------|:---:|---|---|---|
| **Grep**（搜索 `ParsedBlock::`） | **104.3s** | 00:30:29.664Z | 00:32:13.961Z | < 1s |
| **Bash**（`find /Users/konghayao`） | **104.3s** | 00:30:29.664Z | 00:32:13.970Z | 104s |

两个工具 startTime 完全相同、endTime 仅差 9ms。Grep 的输入为搜索 `peri-tui/src/kit/markdown/` 下 `ParsedBlock::`，实际执行 < 1s。

Grep 输入：`{"pattern":"ParsedBlock::","path":"/Users/konghayao/code/ai/perihelion/peri-tui/src/kit/markdown","output_mode":"content"}`

Bash 输入：`find /Users/konghayao -path "*/ratatui-kit-markdown*/src/lib.rs"`（扫描整个 home 目录）

### 复现条件

1. LLM 在一次 turn 中同时发出多个工具调用（tool batch）
2. 其中一个工具执行时间远大于其他工具
3. 查看 Langfuse 中快速工具的 latency → 等于慢工具的 latency

### 根因

`tool_dispatch.rs` 的执行流程：

```
run_before_tool_approvals()   → emit ToolStarted（所有工具，串行）
dispatch_concurrent()         → join_all（并发执行，等所有工具完成才返回）
settle_results()              → emit ToolEnded（所有工具，串行，在 join_all 之后）
```

- **`ToolStarted`** RenderEvent 在阶段一就全部 emit 了 → 所有工具 startTime 基本相同
- **`dispatch_concurrent`** 用 `futures::future::join_all` 等待全部工具完成 → 即使 Grep < 1s 完成，函数也不返回
- **`settle_results`** 在 `join_all` 返回后才串行 emit `ToolEnded` → 快速工具的 ToolEnded 被延迟到慢工具完成后才发出

Langfuse tracer 的 `ToolBatch::on_tool_end()` 用 `chrono::Utc::now()` 记录 endTime，所以 Grep 的 endTime = settle_results 处理到 Grep 的时间 ≈ Bash 完成时间。

## 状态变更记录

| 日期 | 旧状态 | 新状态 | 操作人 | 备注 |
|------|--------|--------|--------|------|
| 2026-07-23 | - | Open | agent | 初始创建 |
| 2026-07-23 | Open | Fixed | agent | 修复：dispatch_concurrent 内 emit ToolEnded，#636 tests pass |

## 修复记录

### 修复 #1（2026-07-23）

- **操作人**：agent
- **用户原意**：工具 batch 中快速工具的 Langfuse latency 不应被同批慢工具拖高
- **修复内容**：将 `ToolEnded` RenderEvent 发射时机从 `settle_results`（join_all 之后）移至 `dispatch_concurrent` 每个工具 async block 内（工具完成即刻发射）。`settle_results` 删除重复的 ToolEnded emit 块。仅修改 `peri-agent/src/agent/stages/tool_dispatch.rs`（+29 -17）
- **涉及 commit**：dd3afc1f
- **验证状态**：已验证（workspace build ✅, peri-agent 636 tests ✅, peri-acp 315 tests ✅）

### 修复 #2（2026-07-23）

- **操作人**：agent
- **用户原意**：Bash 默认 timeout 120s 过长，无法阻止低效命令（如 `find /Users/xxx` 扫描整个 home 目录）
- **修复内容**：Bash 默认 timeout 从 120s 降至 15s；超时提示引导 agent 三选一（优化命令 / 增大 timeout / run_in_background）。修改 `peri-middlewares/src/middleware/terminal.rs`（+9 -3）、`terminal_test.rs`（2 行注释）、`descriptions/bash.md`（1 行）
- **涉及 commit**：27713142
- **验证状态**：已验证（18/18 tests ✅, build ✅）
