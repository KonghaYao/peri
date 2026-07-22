# Langfuse 工具调用全量丢失 + stage-compact 空阶段始终上报

**状态**：Fixed
**优先级**：高
**类型**：Bug
**创建日期**：2026-07-22

## 问题描述

两个 Langfuse 监控 bug：(1) 所有工具调用（Read/Write/Edit/Bash 等）在 Langfuse 仪表盘中完全不可见，Tools 面板空空如也；(2) `stage-compact` span 每次 turn 都会上报，即使 compact 阶段没有发生真正的 micro/full compact 工作，产生大量无意义的 ~20ms 空 span。

## 症状详情

### Bug 1：工具调用不上报

- 正常 LLM 调用（Generation）在 Langfuse 中有完整记录
- Compact span 也能正常上报
- **但任何工具调用**（Read/Write/Edit/Bash/Agent 等）在 Langfuse UI 中完全找不到
- 无论 main agent 还是 subagent 的工具调用均丢失

### Bug 2：空 stage-compact span

- 每次 ReAct 循环进入 Compact 阶段后，即使没有触发 micro/full compact，也产生一个 `stage-compact` span
- 该 span duration 约 0.02s（20ms），内容为空
- 期望：只有当 compact 阶段**实际执行了 micro compact 或 full compact** 时才上报 stage-compact span

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 配置 Langfuse 环境变量（`LANGFUSE_PUBLIC_KEY` 等）
  2. 执行任意需要工具调用的任务（如"读一下 README.md"）
  3. 观察 Langfuse 仪表盘 → Tools 面板无任何记录
  4. 观察 Langfuse 仪表盘 → 每次 turn 均有 stage-compact span
- **环境**：任意

## 涉及文件

- `peri-acp/src/langfuse/bridge.rs:296` —— `from_render_event()` 仅映射 `TextChunk`/`BudgetWarning`，缺少 `ToolStarted`/`ToolEnded` 映射
- `peri-acp/src/event/forwarder.rs:72` —— render 事件通过 `from_render_event` 转发 Langfuse，但工具事件在此层丢失
- `peri-acp/src/langfuse/tracer/mod.rs:638-706` —— `on_stage_start`/`on_stage_end` 中 compact 阶段 span 未检查是否有实际 compact 工作
- `peri-agent/src/agent/events_v2.rs:83,96` —— `RenderEvent::ToolStarted`/`RenderEvent::ToolEnded` 变体定义

## 根因分析

### Bug 1

v2 架构中工具事件（`ToolStarted`/`ToolEnded`）属于 `RenderEvent`。forwarder.rs 的 render 分支正确调用 `from_render_event()`，但该函数（bridge.rs:296）**只处理了 `TextChunk` 和 `BudgetWarning`**，未添加 `ToolStarted`/`ToolEnded` → `UnifiedLangfuseEvent::ToolStart/ToolEnd` 的映射。v1 的 `from_executor_event()` 有完整的 `ToolStart`/`ToolEnd` 映射，但 v2 路径从未走 v1 分支。

### Bug 2

`on_stage_end` 中对所有 stage 统一检查 `duration_ms == 0` 跳过 0ms span。但 compact 阶段即使不做任何实际压缩（no micro/full compact），从进入 Stage::Compact 到检测"无需压缩"并离开，仍有 ~20ms 代码执行耗时，`duration_ms == 0` 无法拦截。`on_compact_start`/`on_compact_end` 的 compact span 能正确跳过（因为 `CompactStarted` 事件根本没发），但 stage-compact span 仍然被创建。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-22 | — | Open | agent | 创建 |
| 2026-07-22 | Open | Fixed | agent | 修复：bridge.rs 补 ToolStarted/ToolEnded 映射；tracer/mod.rs 加 compact_work_done 条件跳过 |

## 修复记录

### 修复 #1（2026-07-22）

- **操作人**：agent
- **用户原意**：修复 Langfuse 工具调用全量丢失 + 空 stage-compact span 始终上报
- **修复内容**：
  - `peri-acp/src/langfuse/bridge.rs`：`from_render_event()` 新增 `RenderEvent::ToolStarted`/`ToolEnded` → `UnifiedLangfuseEvent::ToolStart/ToolEnd` 映射（+20 行）
  - `peri-acp/src/langfuse/tracer/mod.rs`：新增 `compact_work_done: bool` 字段，在 `on_compact_start()` 中设为 true，在 `on_stage_end()` 中检查 `Stage::Compact && !compact_work_done` 时跳过 span 上报（+10 行）
- **涉及 commit**：待提交
- **验证状态**：cargo build 通过 / cargo test -p peri-acp --lib 296 passed
