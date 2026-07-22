# Auto Compact 完成后 loading 短暂变冷却态再恢复

**状态**：Fixed
**优先级**：中
**类型**：Bug
**创建日期**：2026-07-21

## 问题描述

在长对话中自动触发 Full Compact 后，loading spinner 会出现短暂异常：loading 变成冷却态（"Brewed for Xm Xs"），然后又变回 active 态（spinner 旋转）。用户感知为 loading 的闪烁/跳动。

**"full compact 没事, 后面的流式输出的 loading 有问题"**——compact 本身的 loading 显示正常，问题出在 compact 完成后到下一轮 Reason 阶段开始之间的过渡期。

## 症状详情

| 观察点 | 期望行为 | 实际行为 |
|--------|---------|---------|
| Compact 期间 | loading 显示正常 | ✅ 正常 |
| Compact 完成后 | loading 继续保持 active（ReAct 循环未结束） | ❌ loading 变冷却态 |
| 下轮流式输出开始 | loading 恢复 active | ✅ 恢复（但中间有不必要的闪烁） |

## 复现条件

- **复现频率**：每次 auto full compact 触发时
- **触发步骤**：
  1. 进行一个长对话任务（如多文件重构）
  2. 等待上下文使用率超过 85%，系统自动触发 Full Compact
  3. Compact 完成后观察 loading spinner 状态
- **环境**：macOS

## 根因分析

### 事件时序

在 ReAct v2 循环中，auto compact 的事件流如下：

```
Reason (loading=true) → Act (loading=true)
  → Compact 阶段:
      CompactStarted → loading=true（已是 true）
      compact LLM 摘要执行中
      CompactCompleted → phase=Idle → loading=false ← 这里把 loading 关了！
  → Receive 阶段: StageStarted{Receive} → v2_bridge 丢弃
  → Reason 阶段: StageStarted{Reason} → v2_bridge 丢弃 → loading 仍为 false
  → 第一条 TextChunk/ToolStarted → phase=PromptRunning → loading=true ← 才恢复
```

**核心矛盾**：`CompactCompleted` 是 auto compact 和手动 `/compact` 共享的事件，两者对 loading 状态的语义不同：

| 场景 | CompactCompleted 后 | loading 应如何 |
|------|-------------------|---------------|
| auto compact | ReAct 循环继续运行，stream 事件马上就来 | 保持 true，不应中断 |
| 手动 /compact | 命令执行完毕，agent 已结束 | 设为 false / Idle |

当前代码在 `peri-tui/src/kit/acp_events.rs:814` 无条件设 `state.phase = SessionPhase::Idle`，破坏了 auto compact 场景的 loading 连续性。

### 为什么手动 /compact 不受影响

手动 `/compact` 走 Immediate 命令路径：`CompactCompleted` 后紧接着 `push_done` → `TurnDone` 也会设 `phase=Idle`。移除 `CompactCompleted` 中的 phase 重置后，手动路径的 loading 由 `TurnDone` 兜底清除。

### 相关代码位置

- `peri-tui/src/kit/acp_events.rs:813-814` — `CompactCompleted` handler 中 `state.phase = SessionPhase::Idle`
- `peri-tui/src/kit/v2_bridge.rs:142` — `StageStarted{..}` 统一返回 `None`，不触发 phase 变更
- `peri-agent/src/agent/stages/compact.rs:96-103` — auto compact 路径 emit `CompactStarted`
- `peri-agent/src/agent/stages/compact.rs:174-187` — auto compact 路径 emit `MessagesCompacted`
- `peri-acp/src/session/command/compact/pipeline.rs:102-103,146-155` — 手动 compact 路径

## 期望修复

移除 `CompactCompleted` 中对 loading 状态的重置：

```diff
// peri-tui/src/kit/acp_events.rs:813-814
     state.compact_just_completed = true;
-    state.phase = SessionPhase::Idle;
+    // 不重置 phase——auto compact 后 ReAct 循环继续运行，
+    // loading 由流式事件（TextChunk/ToolStarted）和 TurnDone 管理。
```

**影响分析**：

- **auto compact**：loading 从 CompactStarted 到 TurnDone 持续显示，无中断 → ✅ 修复
- **手动 /compact**：loading 从 CompactStarted 到 TurnDone（通过 push_done）持续显示 → ✅ 无回归（TurnDone 兜底清 loading）
- **`/clear` 命令**：`clear.rs:40` 直接 emit `CompactCompleted` 带空 messages，不依赖此处 phase 重置 → ✅ 无影响

## 关联

- `spec/archive-issues/2026-07-17-loading-state-split-brain.md` — loading 状态三个写入源的 split-brain 问题（本 issue 同属此体系）
- `spec/archive-issues/2026-07-13-main-agent-done-loading-persists-bg-still-running.md` — bg agent 场景 loading 不退

## 状态变更记录

| 日期 | 变更前 | 变更后 | 操作人 | 说明 |
|------|--------|--------|--------|------|
| 2026-07-21 | - | Open | agent | 初始创建 |
| 2026-07-21 | Open | Fixed | agent | 修复完成，待用户确认 |

## 修复记录

| 日期 | 提交 | 说明 |
|------|------|------|
| 2026-07-21 | (未提交) | 移除 `peri-tui/src/kit/acp_events.rs:814` — `state.phase = SessionPhase::Idle`。auto compact 路径 loading 由流式事件和 TurnDone 管理，不再中途重置。 |
