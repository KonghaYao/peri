# 主 agent 完成回复后 loading 不退，因后台 agent 仍在运行

**状态**：Open
**优先级**：中
**类型**：Bug
**创建日期**：2026-07-13

## 问题描述

当主 agent 在一轮对话中启动了后台 agent（`run_in_background: true`），主 agent 已经完成了本轮推理、回复文字完整显示在消息区，但后台 agent 仍在运行中。此时 loading spinner 持续转动不退出。

**用户期望**：主 agent 回复完成后 loading 应立即退出，后台 agent 的运行状态不应影响 loading 指示器。loading 应该只反映主 agent 的推理状态，后台任务应通过状态栏或 Tasks 面板来跟踪。

## 症状详情

| 观察点 | 期望行为 | 实际行为 |
|--------|---------|---------|
| 主 agent 回复 | 文字完整显示 | ✅ 正常显示 |
| 后台 agent | 仍在运行中（Tasks 面板计数 > 0） | 正常运行 |
| loading spinner | 主 agent 完成后退出 | ❌ **持续转动** |
| 输入框 | 应可用 | ⚠️ 状态不明确（可能可用，但 loading 不退造成困惑） |

## 与已有 issue 的关系

已有三个 issue 描述了 bg agent 场景下的 loading 不退问题，但场景和本 issue 不同：

| Issue | 状态 | 差异说明 |
|-------|------|---------|
| `2026-07-11-bg-multi-agent-loading-callback-ok-but-loading-stuck.md` | Open | 多个 bg agent **全部完成后** callback 正常但 loading 不退。本 issue 是 bg agent **仍在运行中**、但主 agent 已完成。 |
| `2026-07-11-bg-multi-agent-loading-freeze-last-callback-lost.md` | Open | 同上场景，最后一个 callback 丢失 + loading 不退。|
| `2026-07-07-bg-agent-complete-no-resume.md` | Open | 单个 bg agent 完成后主 agent 不续跑 + loading 退。本 issue 是主 agent **已完成**，不需续跑。|

**核心差异**：已有 issue 是 bg agent 完成后 `SubagentStopped` 覆盖了 `TurnDone` 的 loading 清除。本 issue 的场景中 bg agent 仍在运行，主 agent 实际产出的是 `TurnSuspended`（非 `TurnDone`），之后 bg 完成时同样被 `SubagentStopped` 覆盖。

## 复现条件

- **复现频率**：待确认（用户发现 bg agent 运行中时触发）
- **触发步骤**：
  1. 启动 Peri TUI
  2. 让主 agent 在一次回复中调用 Agent 工具（`run_in_background: true`）
  3. 主 agent 输出完剩余回复文字，回复完整显示在消息区
  4. 后台 agent 仍在运行中（状态栏显示计数 > 0）
  5. **观察**：loading spinner 持续转动不退出
- **环境**：macOS

## 根因分析

### 机制：主 agent 文字输出完后不产 TurnDone

bg agent 仍在运行时，`idle_should_wait` probe 返回 `true`（`active_count() > 0`）：

- `executor.rs:694-697` 注入 probe：`move || bg_registry.active_count() > 0`
- `stages/mod.rs:639-643` End 阶段检查：有未完成的 bg → 不退出 loop，进入 `await_wake`

主 agent 文字虽然完整显示了，但 agent loop 没有退出，产出的是 **`TurnSuspended`**（非 `TurnDone`）：

```rust
// stages/mod.rs:653-658
context.event_bus.emit_state(StateEvent::TurnSuspended { ... });
```

### TurnSuspended 清 loading 后又被 SubagentStopped 覆盖

时序链路：

```
1. 主 agent 文字输出完 → End → idle_should_wait=true
2. emit TurnSuspended → is_loading = false ✅ (acp_events.rs:326)
3. await_wake 阻塞等待 bg agent
4. bg agent 完成 → SubagentStopped 事件到达 TUI
5. SubagentStopped handler → phase = PromptRunning (acp_events.rs:494)
6. push_acp_state → is_loading = true ❌ (phase=PromptRunning 导致)
7. 无后续流事件 → loading 永远 true 🔴
```

### 核心缺陷：SubagentStopped 无条件设 phase=PromptRunning

`acp_events.rs:492-500`：

```rust
SubagentStopped { agent_id } => {
    state.current_turn.stop_subagent(agent_id);
    state.variant = 1;
    state.phase = SessionPhase::PromptRunning;  // ← 无条件，不区分 bg/non-bg
    push_view_models(state);
    push_acp_state(state);  // → phase=PromptRunning → is_loading=true
}
```

这是所有 bg agent loading 不退系列 issue 的**共同根因**。`SubagentStopped` 应区分 bg agent 场景：只有当前仍有活跃的流式 agent 时才设 `phase=PromptRunning`，否则应保持 `phase=Idle`。

### 与本 issue 的直接关联

本 issue 的场景（bg 仍在运行、主 agent 文字已输出）中：
- TurnSuspended 清了一次 loading
- 随后 bg 完成的 SubagentStopped 重新设回 loading
- **与已有 issue 的差异仅为**：他们是 TurnDone 被覆盖，这里是 TurnSuspended 被覆盖——底层机制完全相同

## 涉及文件

| 文件 | 角色 |
|------|------|
| `peri-tui/src/kit/acp_events.rs:492-500` | **根因**：SubagentStopped 无条件 phase=PromptRunning |
| `peri-tui/src/kit/acp_events.rs:326` | TurnSuspended 清除 loading |
| `peri-tui/src/kit/acp_events.rs:887-913` | push_acp_state 防御逻辑（phase→is_loading 映射）|
| `peri-agent/src/agent/stages/mod.rs:639-708` | End 阶段 idle_should_wait + TurnSuspended + await_wake |
| `peri-acp/src/session/executor.rs:694-697` | idle_should_wait probe 定义 |
| `peri-middlewares/src/subagent/spawner.rs:335-351` | bg 完成回调 + SubagentStopped 发送时序 |
| `peri-middlewares/src/subagent/background.rs:101-342` | BackgroundTaskRegistry 生命周期 |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-13 | — | Open | agent | 创建（issue-create skill 访谈还原现象） |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
