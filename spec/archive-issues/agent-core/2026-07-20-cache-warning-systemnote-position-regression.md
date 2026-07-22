# Cache 命中率警告 SystemNote 位置错位——多警告时全部出现在 user 消息正下方

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-20

## 问题描述

触发多轮 LLM 调用（如 `/start-devflow` 等同时发起多个 LLM 请求的操作）时，多个 `Prompt cache 命中率 XX% < 80%` 的 SystemNote 警告会全部出现在当前轮 user 消息的正下方，而不是与各自对应的 AI 响应内容交织在正确的时序位置。

**根因**：`flush_current_turn()` 在 cc81263（SubAgent 同 turn 多次调用修复）中新增了 `has_running_subagent` 守卫，当 current_turn 中存在正在运行的 SubAgentAccumulator 时跳过 flush。这与 SystemNote 的 "flush-then-push" 时序设计冲突——多并行 subagent 场景下，所有 LlmCallEnd 到达时 subagent 仍在运行，守卫阻止 flush，SystemNote 直接 push 到 committed，而 subagent 内容留在 current_turn。`push_view_models` 按 `committed + current_turn` 拼接，导致所有 SystemNote 排在所有 AI 内容前面。

这是 `2026-07-16-system-note-cache-warning-position-wrong`（Fixed）修复与后续 SubAgent 竞态修复（`#cf0d4ac2`）之间的设计冲突。

## 症状详情

| 维度 | 期望行为 | 实际行为 |
|------|----------|----------|
| SystemNote 位置 | 各警告与各自触发 LLM 调用的 AI 响应内容按时序交织 | 4 条警告全部出现在 user 消息正下方，之后才开始 AI 回复 |
| 出现频率 | 必现（一轮 turn 内触发 4 次 LLM 调用时） | 每次 |
| 影响范围 | Cache 命中率警告类型的 SystemNote（`app-note-cache-hit-low`） |
| 历史关联 | `2026-07-16-system-note-cache-warning-position-wrong`（Fixed）+ 后续 SubAgent 竞态修复 `#cf0d4ac2` | 两个 fix 存在设计冲突：flush_current_turn 守卫阻止了 flush |

**用户观察到的 TUI 消息流**：

```
❯ /start-devflow 先单独做 #3
Prompt cache 命中率 76% < 80% (req: c0599410-...)
Prompt cache 命中率 62% < 80% (req: da029faf-...)
Prompt cache 命中率 70% < 80% (req: 0d3e0492-...)
Prompt cache 命中率 57% < 80% (req: af8377b5-...)
[此处才开始 AI 的回复内容...]
```

## 复现条件

- **复现频率**：必现（有 subagent 并行运行 + cache hit rate < 80%）
- **触发条件**：单轮 turn 内并行触发多个 subagent（如 `/start-devflow` dispatch 多个 worker），每个 subagent 的 `LlmCallEnd` 到达时主 agent 的 SubAgentAccumulator 仍在运行
- **根因交互**：
  1. `flush_current_turn()` 中的 `has_running_subagent` 守卫阻止 flush → SystemNote 留在 committed
  2. 并行 forwarder 共享 `event_tx`，各 subagent 的 LlmCallEnd 可批量到达
  3. `push_view_models` 按 `committed + current_turn` 拼接，SystemNote 永远在前

## 设计原则

> **SystemNote 什么时候出现，它就应该停留在那个位置上。**

SystemNote 是消息流中的一等公民，不应通过"旁路绕过 current_turn → 直接写入 committed"来实现。当前 committed 旁路方案的本质问题是：SystemNote 的时序位置依赖 `flush_current_turn()` 将 current_turn 内容先移到 committed 来"制造空位"，但这个空位制造操作不总是可行的（有 subagent 运行中时被守卫阻止）。

## 推荐方案

**改为在 current_turn 内部注入 SystemNote segment**，而非 push 到 committed。

```
现状：SystemNote → committed（旁路，时序依赖 flush_current_turn()）
推荐：SystemNote → current_turn 内部 segment（随 turn 自然流动）
```

这样 SystemNote 天然位于它出现的时序位置——在已产出的 AI 内容之后、后续 AI 内容之前。TurnDone 归档时随 current_turn 一起进入 committed。

**优势**：
- 不再依赖 `flush_current_turn()`，与 `has_running_subagent` 守卫彻底解耦
- SystemNote 的生命周期和 current_turn 一致，commit / rewind 路径无需额外处理
- 改动面小：仅 `SystemNotification` handler（注入 segment）+ `view_models()`（渲染时产出 `TuiSystemNote` VM）

## 涉及文件

| 文件 | 行号 | 作用 |
|------|------|------|
| `peri-tui/src/kit/acp_events.rs` | 186-206 | `flush_current_turn()` — `has_running_subagent` 守卫跳过 flush |
| `peri-tui/src/kit/acp_events.rs` | 608-628 | `SystemNotification` handler — 调 `flush_current_turn()` 后 push `TuiSystemNote` 到 `committed` |
| `peri-tui/src/kit/acp_events.rs` | 1289-1292 | `push_view_models()` — committed + current_turn 拼接导致 SystemNote 前置 |
| `peri-tui/src/kit/acp_notifier.rs` | 506-538 | `usage_update` handler — cache < 80% 时构造 `SystemNotification` 桥接发送 |
| `peri-tui/src/kit/acp_types.rs` | 311-317 | `CurrentTurn::is_empty()` — subagents 非空时返回 false |
| `peri-agent/src/agent/stages/executor_helpers.rs` | 207-273 | `spawn_event_pump()` — 并行 forwarder 共享 event_tx，事件顺序不确定 |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-20 | — | Open | agent | 创建 |
| 2026-07-21 | Open | Fixed | agent | 修复：SystemNote/BudgetWarning 改为注入 current_turn 内部 segment，与 flush_current_turn 守卫解耦 |

## 修复记录

### 修复 #1（2026-07-21）

- **操作人**：agent
- **用户原意**：SystemNote/BudgetWarning 在 subagent 并行场景下全部堆积在 user 消息下面，而非与 AI 内容按时序交织
- **修复内容**：
  1. `peri-tui/src/kit/acp_types.rs`：
     - 新增 `TurnSegment::SystemNote { text, level, content_hash }` 变体
     - 新增 `CurrentTurn::push_system_note()` 方法（先 flush_text_segment 再追加 SystemNote 到 segments）
     - `build_view_models()` 中处理 `TurnSegment::SystemNote` → 产出 `TuiSystemNote` VM
  2. `peri-tui/src/kit/acp_events.rs`：
     - `SystemNotification` handler：改为 `current_turn.push_system_note()` 替代原 `flush_current_turn()` + `committed.push_back()`
     - `BudgetWarning` handler：同上
  3. `peri-tui/src/kit/tui_render_unit.rs`：`TuiNoteLevel` 增加 `Eq` derive（TurnSegment 需要）
- **修复思路**：SystemNote 不再通过 `flush_current_turn()` → `committed.push_back()` 走旁路，而是作为 `TurnSegment` 直接注入 `current_turn`，与其出现的时序位置天然对齐。不再依赖 `flush_current_turn()` 及其 `has_running_subagent` 守卫，彻底解耦。
- **验证状态**：待验证
