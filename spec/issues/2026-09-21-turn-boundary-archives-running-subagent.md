# turn 边界归档仍在运行的 Subagent 分组

**状态**：Open
**优先级**：中
**创建日期**：2026-09-21

## 问题描述

`TurnSuspended` / `TurnInterrupted` 把 `current_turn` 归档进 committed 时不检查是否仍有正在运行的
subagent：仍在运行的组带着「边界那一刻的内容 + `is_running = true`」落进 committed，accumulator 随即被
`reset()` 清空。此后该组不再接收任何子事件，视图永久停在那一刻。

同一守卫在 `TurnDone` 路径上是存在的——`flush_current_turn`（`peri-tui/src/kit/acp_events/mod.rs`）在
存在 `is_running` 的 `SubAgentAccumulator` 时跳过 flush；挂起与中断路径
（`peri-tui/src/kit/acp_events/turn.rs` 的 `handle_turn_suspended` / `handle_turn_interrupted`）没有这个检查。

触发面不小：`TurnSuspended` 正是主 turn 进入 idle 等待异步事件续跑时发出的
（`peri-agent/src/agent/stages/mod.rs` 的 Receive 分支：queue empty 且 `idle_should_wait`），而后台 subagent
活跃正是 `idle_should_wait` 的典型来源——「后台 subagent 在跑」几乎必然经过这条边界。

## 症状详情

以 bg subagent 为例，两份投影在边界处分叉（由 bridge 级回归固定，见
`peri-tui/src/kit/acp_events_test/bg_task_live_test.rs::test_bg_group_frozen_in_view_models_after_turn_suspended`）：

- `VIEW_MODELS` 里的组停在边界那一刻的文本（断言 `["first"]`），且 `is_running` 仍为 true——
  `CurrentTurn::deactivate` 只置顶层 `active = false`，不清理 subagent 的 `is_running`。
- 边界之后的子事件只进 bg 兜底投影 `BG_LIVE_DETAIL`（断言 `["first+second"]`）。
- subagent 详情面板已按「以 live 明细为准 + 运行身份精确对应」规避（commits `2abf56b8`、`db772152`）；
  消息区里那个组本身仍是冻结的，任何直接消费组投影的视图都会看到停在边界那一刻的内容。

## 复现条件

- **复现频率**：确定（turn 在 subagent 运行期间挂起或中断即发生）。
- **触发步骤**：
  1. 起一个后台 subagent（`run_in_background: true`）。
  2. 让主 turn 在它运行期间走到 idle/await_wake（`TurnSuspended`），或在它运行期间取消（`TurnInterrupted`）。
  3. 观察 `VIEW_MODELS` 中该组的文本不再增长，而 `BG_LIVE_DETAIL` 继续增长。
- **环境**：Peri TUI；后台 subagent；与模型和 OS 无关（bridge 级回归即可复现）。

## 涉及文件

- `peri-tui/src/kit/acp_events/mod.rs` —— `flush_current_turn` 的 running-subagent 守卫，目前只覆盖 TurnDone。
- `peri-tui/src/kit/acp_events/turn.rs` —— `handle_turn_suspended` / `handle_turn_interrupted` 的归档路径。
- `peri-tui/src/kit/acp_types/current_turn.rs` —— `deactivate` / `reset` 语义（`deactivate` 不清理 subagent 的 `is_running`）。
- `peri-tui/src/kit/acp_events_test/bg_task_live_test.rs` —— 固定两份投影分叉的回归测试。
- `peri-tui/src/kit/panels/subagent_detail.rs` —— 面板侧的对策，不改本条根因。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-09-21 | — | Open | agent | subagent 详情面板修复的代码评审发现；面板侧已按 live 明细规避，根因未动 |

## 残余风险

- 面板已规避，消息区及其他组投影消费方未规避。
- 照搬 `flush_current_turn` 的守卫会改变 turn 边界语义：挂起期间 `current_turn` 不再归档，
  需确认 committed 顺序、loading 状态与「挂起后新事件续跑」的既有行为不受影响。
- 同步 subagent 走中断路径时的表现（内容落到主流程还是随 `reset` 丢失）尚未核实，修复时需一并确认。
