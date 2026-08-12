> 归档于 2026-08-11，原路径 spec/issues/2026-07-22-llm-api-error-silently-swallowed-in-tui.md

# LLM API 报错时 TUI 消息区静默不显示错误

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-22

## 问题描述

当 LLM API 返回错误（5xx 服务端错误、429 限流、401 鉴权失败）时，v2 ReAct 循环路径不会向 TUI 消息区推送任何错误提示。用户看到的现象是：loading 动画停止，但消息区没有红色错误 SystemNote，agent 看起来像"正常结束"了一样。错误信息只在 tracing 日志中可见，对终端用户完全不可见。

## 症状详情

| 场景 | 期望行为 | 实际行为 |
|------|----------|----------|
| API 返回 503（服务不可用） | 消息区显示红色错误："Agent 执行失败: API 错误 503: ..."，重试过程可见 | 无任何提示，loading 结束 |
| API 返回 429（限流） | 消息区可见重试通知 + 如果重试耗尽则显示最终错误 | 无任何提示，loading 结束 |
| API 返回 401（鉴权失败） | 消息区立即显示红色错误："Agent 执行失败: API 错误 401: ..." | 无任何提示，loading 结束 |

## 根因分析

v2 ReAct 循环中，LLM 错误的数据流如下：

```
provider_adapter.rs  → AgentError::LlmHttpError / LlmError
retry.rs             → 最多 5 次重试，emit LlmRetrying（仅 Langfuse/tracing）
reason.rs:167        → 捕获错误，emit LlmCallEnd(output="ERROR: ...")（仅 Langfuse）
stages/mod.rs:609    → run_react_loop 返回 LoopResult::Error(e)
```

而 **`build_and_execute_agent_v2`（executor_helpers.rs:582-639）处理 `LoopResult::Error` 时存在两个缺失**：

### 缺失 1：未 emit `AgentExecutionFailed` 事件

代码注释宣称 `LoopResult::Error` 分支先发 `AgentExecutionFailed` 事件再判断 stop_reason，但 Phase 9 实际代码为：

```rust
// executor_helpers.rs:586-596
LoopResult::Error(ref e) => {
    error!(...);
    let reason = if ctx.cancel.is_cancelled() || matches!(e, AgentError::Interrupted) {
        PromptStopReason::Cancelled
    } else if matches!(e, AgentError::MaxIterationsExceeded(_)) {
        PromptStopReason::MaxTurnRequests
    } else {
        PromptStopReason::EndTurn  // ← LLM/HTTP 错误也走这里，视为正常结束
    };
    (false, reason)
};
```

**没有 emit `AgentExecutionFailed { message: e.to_string() }`**。

### 缺失 2：`ObserveEvent::TurnError` 从未被 emit

`events_v2.rs:315-320` 定义了 `ObserveEvent::TurnError { turn_id, agent_id, reason, message }`，且 `v2_bridge.rs:132-133` 将其映射到 `AcpEventData::AgentExecutionFailed` → TUI 显示红色错误。但在**整个生产代码中，没有任何地方调用 `emit_observe(ObserveEvent::TurnError {...})`**。这个事件变体只有定义和测试代码引用。

### TUI 侧已正确准备

`acp_events.rs:899-907` 的 `AgentExecutionFailed` handler 已正确实现：
- 通过 `inject_system_note(text, TuiNoteLevel::Error)` 注入红色错误 SystemNote
- 设置 `phase = Idle` 停止 loading

但上游不发事件，这端永远不会触发。

### TurnEnded 不产生 TUI 输出

虽然 `build_and_execute_agent_v2` 确实 emit 了 `TurnEnded { error_kind: LlmFailure }`（line 615-621），但 `map_executor_event` 将 `TurnEnded` 映射为 `None`（不产生任何 SessionUpdate），spawn_event_pump 仅将其用于 Langfuse tracer 的 `last_error` 追踪。

## 复现条件

- **复现频率**：必现
- **触发条件**：任何导致 `AgentError::LlmHttpError` 或不可重试的 `AgentError::LlmError` 的 LLM 请求
- **受影响路径**：v2 ReAct 循环（当前主路径）

## 涉及文件

- `peri-acp/src/session/executor_helpers.rs:582-639` —— `build_and_execute_agent_v2` Phase 9，缺少 `AgentExecutionFailed` emit
- `peri-agent/src/agent/events_v2.rs:315-320` —— `ObserveEvent::TurnError` 定义（未被 emit）
- `peri-agent/src/agent/stages/mod.rs:609` —— `run_react_loop` 返回 `LoopResult::Error`
- `peri-tui/src/kit/v2_bridge.rs:132-133` —— `TurnError → AgentExecutionFailed` 映射（从未触发）
- `peri-tui/src/kit/acp_events.rs:899-907` —— `AgentExecutionFailed` TUI handler（已正确等待但收不到事件）

## 修复方向（初步）

**方案 A（推荐）**：在 `build_and_execute_agent_v2` 的 Phase 9 中，`LoopResult::Error` 分支追加 emit `ExecutorEvent::AgentExecutionFailed` 到 `event_tx`。最小改动，利用现有 TUI handler 和 ACP 通知通道。

**方案 B**：在 `run_react_loop` 返回 `Error` 后，emit `ObserveEvent::TurnError` 经由 v2_bridge → AgentExecutionFailed。但当前 `build_and_execute_agent_v2` 不在 StageContext 内，没有 EventBus 引用（EventBus 在此函数构造后传给 `run_react_loop` 但不返回）。

**方案 C**：在 `reason.rs` 的 LLM 错误分支内 emit `ObserveEvent::TurnError`，这样 v2_bridge 可以直接将其映射到 `AgentExecutionFailed`。

具体方案需继续评估 StageContext / EventBus 可用性。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-22 | — | Open | agent | 创建 |
| 2026-08-11 | Open | Fixed | agent | 归档：对应实现 executor_helpers.rs AgentExecutionFailed emit → TUI 错误提示可见 |

## 修复记录

（由 auto-issue-fixer 修复阶段追加，创建时留空）
