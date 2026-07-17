> 归档于 2026-07-17，原路径 spec/issues/2026-07-16-system-note-cache-warning-position-wrong.md
# Cache 命中率警告 SystemNote 在消息流中位置错位——被积压到上一个 user/AI message 后面

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-16

## 问题描述

Cache 命中率低于 80% 时，系统会在消息流中注入一条 `SystemNote` 警告（如 "Prompt cache 命中率 18% < 80%"）。这条 system note 应该维持在其出现的消息流位置——即在触发它的那轮 turn 的 user message 和 AI response 之间。但实际上它被积压到了上一个 user message 或 AI message 的后面，丢失了原本的时序位置。

## 症状详情

| 维度 | 期望行为 | 实际行为 |
|------|----------|----------|
| SystemNote 位置 | 维持在消息流中产生它的时刻的位置（该轮 user message 和 AI response 之间） | 被积压到上一个 user 或 AI message 后面，不在原位 |
| 出现频率 | — | 每轮都出现 |
| 影响范围 | — | Cache 命中率警告类型的 SystemNote（`app-note-cache-hit-low`） |

## 复现条件

- **复现频率**：必现（每轮 turn 都出现）
- **触发条件**：Prompt cache 命中率 < 80% 时，当前 turn 产生 cache 警告 system note
- **环境**：任何 provider（只要触发 cache 命中率计算）

## 涉及文件

- `peri-tui/src/kit/acp_notifier.rs:499-532` —— `usage_update` 处理器中检测 cache hit rate < 80%，构造 `SystemNotification` 并通过 `bridge_tx` 发送
- `peri-tui/src/kit/acp_events.rs:390-406` —— `SystemNotification` 事件处理器，将 `TuiSystemNote` push 到 `state.committed`（`push_back`）
- `peri-tui/src/kit/acp_events.rs:234-282` —— `TurnDone` 处理器，将 `current_turn` 的 AI 内容归档到 `committed`

## 根因分析

`SystemNotification` / `BudgetWarning` 事件直接 push 到 `state.committed`（持久化队列），绕过了 `current_turn` 的 `TurnSegment` 分段系统。`push_view_models` 始终按 `committed + current_turn.view_models()` 的顺序拼接，导致 SystemNote 永远出现在所有 `current_turn` 内容之前，丢失时序位置。

**事件时序**（单步 ReAct turn）：

```
Reason 阶段 → LlmCallEnd(usage) 晚于流式 reasoning chunk，早于 Act 阶段的工具调用
  → TUI bridge_tx 收到事件的顺序：
    ① ReasoningChunk（→ current_turn.append_reasoning）
    ② SystemNotification（→ committed.push_back）      ← 问题所在
    ③ ToolStarted / ToolEnded（→ current_turn）
    ④ TurnDone（→ current_turn → committed）
```

**`push_view_models` 合并**：
```rust
items = committed.clone() + current_turn.view_models()
//        ↑ SystemNote 在这里       ↑ AI 内容在这里
//    导致 SystemNote 总是排在所有 AI 内容前面
```

**修复方向**：在 push SystemNote 到 committed 之前，先将 `current_turn` flush 到 `committed`（与 `BgCallbackBubble` 的 flush-then-push 模式一致），使 SystemNote 出现在已产出 AI 内容之后、后续 AI 内容之前。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-16 | — | Open | agent | 创建 |
| 2026-07-16 | Open | Fixed | agent | 根因定位 + 修复完成 |

## 修复记录

**日期**：2026-07-16
**修复文件**：`peri-tui/src/kit/acp_events.rs`

**修改点**：
1. `BudgetWarning` 处理器（L354）—— push SystemNote 到 committed 前，先 flush current_turn
2. `SystemNotification` 处理器（L390）—— 同上

**修复方式**：在两处处理器的 `committed.push_back(TuiSystemNote)` 之前，增加 current_turn flush 逻辑（与 `BgCallbackBubble` 的 flush-then-push 模式一致）：
```rust
if !state.current_turn.committed && !state.current_turn.is_empty() {
    for vm in state.current_turn.view_models() {
        state.committed.push_back(vm.clone());
    }
}
state.current_turn.reset();
```

**效果**：
- SystemNote 出现在已产出 AI 内容之后、后续 AI 内容之前，而非永远堆积在最前面
- 多步 ReAct turn 中，每个 step 的 cache 警告出现在其对应 LLM 调用的位置附近
- 不影响 TurnDone 归档逻辑（current_turn 已经 reset，TurnDone 不会重复归档）
