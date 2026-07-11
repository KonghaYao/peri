# 多 bg agent 全部回调正常但 loading 仍不退出

**状态**：Open
**优先级**：高
**类型**：Bug
**创建日期**：2026-07-11

## 问题描述

当 AI 在一次回复中连续调用多个 Agent 工具（`run_in_background: true`）——即同一轮 ReAct 迭代中启动 ≥2 个 bg agent，所有 bg agent 顺序完成后，**callback 消息全部正常显示**（双通道 flush-then-push 链路正常），但 **loading spinner 永久不退，输入框仍可用**。

**与已有 issue `2026-07-11-bg-multi-agent-loading-freeze-last-callback-lost` 的关键差异**：

| 维度 | 已有 issue（last-callback-lost） | 本 issue |
|------|-----|-----|
| callback 消息 | ❌ 最后一个丢失 | ✅ 三个全正常 |
| loading 状态 | ❌ 卡死 | ❌ 卡死 |
| 根因猜测 | 多实例终结/清理逻辑导致最后一个 callback 落空 | callback 链路通，但 **loading 清除条件未触发**——多 bg agent 的最后一个 `TurnDone` 可能未发生或 loading 清除被覆盖 |

## 症状详情

| 观察点 | 期望行为 | 实际行为 |
|--------|---------|---------|
| bg-1 callback 消息 | 正常显示 | ✅ 正常 |
| bg-2 callback 消息 | 正常显示 | ✅ 正常 |
| bg-3 callback 消息 | 正常显示 | ✅ 正常 |
| 所有 bg 完成后 loading | loading spinner 停止，输入框恢复 | ❌ **loading 一直转** |
| 卡死期间交互 | — | 输入框**仍可正常输入**（TUI 未完全冻结） |

**关键现象信号**：
- **callback 全到**：双通道 flush-then-push 链路在 3 实例场景下工作正常
- **loading 不退**：说明 loading 清除条件与 callback 到达无关。loading 只能靠主 Agent 的 `TurnDone`/`TurnInterrupted`/`TurnSuspended` 清除（见下文分析）
- **可能性 1**：主 Agent 处理完最后一个 callback 后的 `TurnDone` 没有发生（agent 在 await_wake 中等待 phantom bg agent？）
- **可能性 2**：最后一个 `TurnDone` 发生了，但其 loading 清除**在后续的 bg 相关事件处理中被覆盖回 true**

## 根因分析

### loading 清除机制的硬约束

来自代码分析（`acp_events.rs`）：

```
is_loading = false 的唯一入口（流式路径）：
  TurnDone           (line 240)  ← 主 agent 完成一轮推理
  TurnInterrupted    (line 293,310)
  TurnSuspended      (line 326)

BackgroundTaskCompleted (line 527-550) → ❌ 不打 is_loading = false，只打日志
```

这意味着 bg agent 完成本身**永远不会清除 loading**。loading 必须由主 Agent 产生 `TurnDone` 来清除。正常的 callback 流程是：

```
bg agent 完成 → MQ push → SyntheticUserMessage emit
  → BgCallbackBubble (flush) + LocalUserBubble (push)  ← callback 消息出现 ✅
  → 主 Agent drain → ReAct 循环 → AI 输出 → TurnDone → is_loading = false ✅
```

### 推测的失效场景

**场景 A：最后一个 bg callback 后主 Agent 未产生 TurnDone**

可能原因：`idle_should_wait` probe（`bg_registry.active_count() > 0`）在 bg agent 全部完成后未立即归零，存在竞态窗口导致 agent 继续 `await_wake`。

**场景 B：TurnDone 发生了但 loading 被后续事件覆盖**

`push_acp_state` 有一条防御逻辑（`acp_events.rs:878-880`）：
```rust
if acp.is_loading && state.phase == SessionPhase::Idle {
    state.phase = SessionPhase::PromptRunning;  // 自动提级
}
```

如果 `TurnDone` 之后紧接着有 `ToolCount` 或 `Progress` 等非流事件触发 `push_acp_state`，而此时 atom 侧 `is_loading` 已被某个旁路（如 `submit_consumer`）改回 `true`，则防御逻辑会**自动提升 phase 为 PromptRunning**，loading 永不清除。

### 与 woken_once 移除的关联

`woken_once` 移除（见 `2026-07-11-bg-multi-agent-loading-freeze-last-callback-lost.md`）使 `await_wake` 可以多次进入。如果 bg agent 计数在 `active_count` 和实际完成之间存在时间差（bg agent 已完成但 registry 尚未清理），agent 可能进入一轮不必要的 `await_wake`，而在这次等待后 drain 时遇到空队列——没有新消息 = 没有新的 ReAct 循环 = 没有 TurnDone。

## 复现条件

- **复现频率**：较高（AI 在一轮中调用多个 bg agent 时触发）
- **触发步骤**：
  1. 启动 Peri TUI
  2. 让主 agent 在一次回复中调用 ≥2 个 Agent 工具（background 模式）
  3. 所有 bg agent 完成后，观察消息区：
     - 每个 callback 消息正常出现 ✅
     - loading spinner 不退出 ❌
- **环境**：macOS，任意模型

## 与已有 issue 的关系

| Issue | 状态 | 关联说明 |
|-------|------|---------|
| `2026-07-11-bg-multi-agent-loading-freeze-last-callback-lost.md` | Open | 同一场景的**不同症状**——那个是 callback 丢失 + loading 卡死，本 issue 是 callback 全正常但 loading 不退。可能源自同一根因的不同分支 |
| `2026-07-07-bg-agent-complete-no-resume.md` | Open | 单 bg agent 完成后主 agent 不续跑。本 issue 中主 agent 看起来**续跑了**（callback 消息到了），但最终 TurnDone 没清除 loading |
| `2026-07-11-hung-bg-agent-await-wake-block-forever.md` | Open | await_wake 永久阻塞。若 bg agent 已全部完成但 registry 计数未及时归零，agent 可能进入不必要的 await_wake |

## 涉及文件

- `peri-tui/src/kit/acp_events.rs:527-550` —— `BackgroundTaskCompleted` 处理（不打 loading=false）
- `peri-tui/src/kit/acp_events.rs:240` —— `TurnDone` 清除 loading
- `peri-tui/src/kit/acp_events.rs:878-880` —— `push_acp_state` 防御逻辑（可能覆盖 loading 清除）
- `peri-agent/src/agent/stages/mod.rs:670-711` —— `await_wake` + `idle_should_wait` probe
- `peri-middlewares/src/subagent/background.rs` —— `BackgroundTaskRegistry.active_count()`
- `peri-acp/src/session/executor.rs:691-693` —— `idle_should_wait` probe 定义

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-11 | — | Open | agent | 创建（用户反馈多 bg agent callback 全正常但 loading 不退） |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
