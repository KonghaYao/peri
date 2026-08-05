# auto-compact 后 TurnDone 误触发 session/load 重放（compact 区分判定失效）

**状态**：Open
**优先级**：高
**创建日期**：2026-08-05
**关联计划**：`2026-08-05-core-flow-bugfix-plan.md` S4.1

## 问题描述

`handle_turn_done`（`turn.rs:18-55`）先调用 `flush_current_turn()`（其结尾无条件 `current_turn.reset()`，`mod.rs:227-247`），**之后**才评估 `state.compact_just_completed && state.current_turn.is_empty()`（`turn.rs:45`）。flush 之后 `current_turn` 恒为空（唯一例外是存在运行中 subagent 导致提前 return），该守卫失去区分能力——注释声称的"agent 内部 compact 后 current_turn 有内容 → 不触发重放"在判定时已被 flush 抹平。

**每次含 agent 内部 auto-compact 的 turn 结束时**（CompactCompleted 注入 note → 循环继续产出 → TurnDone）都会误触发一次 `THREAD_LOAD_TX.send(session_id)` → session/load 重放：消息流被服务端历史整体重写、本地 SystemNote（compact 完成提示）消失、滚动跳变、额外一轮 close+load RPC；重放期间与后续流事件交错还会产生 committed 乱序。

## 症状详情

- 现有测试漏检：`acp_events_test.rs:1708-1741` 场景 B 在 dispatch 前 `append_text`，但 flush 先清空判定仍命中；且场景 B **只断言 flag 被清除，从未断言 rx 无消息**——场景 B 实际误触发但测试通过
- 对抗 review 确认：`/compact` 以 `AgentText` 提交（`input_area_test.rs:218`），submit_consumer 不识别命令；`CompactCompleted` 事件（`event/mod.rs:83-94`）**不含 trigger 字段**——`compact_command_pending` 标志无处置位

## 复现条件

- **复现频率**：必现（compact 配置开启时每次 auto-compact turn 结束）
- **触发步骤**：任一轮含 agent 内部 auto-compact → turn 正常结束 → 观察消息流整体重写

## 涉及文件

- `peri-tui/src/kit/acp_events/turn.rs:18-55` —— 判定顺序错误
- `peri-tui/src/kit/acp_events/mod.rs:227-247` —— `flush_current_turn` 无条件 reset
- `peri-tui/src/kit/acp_events_test.rs:1708-1741` —— 场景 B 漏断言
- 服务端（若选方案 A）：`peri-agent/src/agent/events.rs:154-157`（`CompactTrigger` Auto/Manual 枚举，`CompactStarted` 已携带）、`peri-acp/src/session/event_sink.rs`（补映射）

## 修复方向（对抗 review 重设计，二选一）

- **方案 A（最可靠，推荐）服务端透传 trigger**：`CompactStarted` 已携带 `CompactTrigger`，event_sink 补映射或给 `CompactCompleted` 加字段，TUI 据此区分命令/auto compact
- **方案 B（TUI 侧最简）流事件清除标志**：CompactCompleted 置标志后，任何流事件（TextChunk/ReasoningChunk/ToolStarted/ToolEnded/SubagentStarted）到达时清除——命令 compact 后无流事件，auto-compact 后标志被后续流事件清掉；无需知道命令来源
- **必须先写红测试**：场景 B 补 `rx.try_recv() == Err(Empty)` 断言（现有测试已证明会失败）——同时是 bug 锁定与修复验收

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-05 | — | Open | agent | 创建（peri-tui 审查发现；对抗 review 推翻 compact_command_pending 方案） |
| 2026-08-05 | Open | Fixed | agent | 修复：方案 B——dispatch 入口流事件（TextChunk/ReasoningChunk/ToolStarted/ToolEnded/SubagentStarted）到达即清除 compact_just_completed；TurnDone 判定删除失效的 current_turn.is_empty()；场景 B 测试改为真实事件时序并补 rx 空断言 |
| 2026-08-05 | Fixed | Fixed | agent | 追加：方案 A 根治（服务端透传 CompactTrigger），消除修复 #1 的残余风险边缘洞；方案 B 流事件清除保留为防御 |

## 修复记录

### 修复 #2（2026-08-05）

- **方案**：A（服务端透传 CompactTrigger，根治）
- **改动文件**：
  - `peri-agent/src/agent/events.rs` —— `ExecutorEvent::CompactCompleted` 新增 `#[serde(default)] trigger: CompactTrigger` 字段（旧事件无字段按 Auto 处理）；`CompactTrigger` 补 `Default`（Auto）
  - `peri-agent/src/agent/events_v2_mapper.rs` —— MessagesCompacted → CompactCompleted 填 `trigger: CompactTrigger::Auto`
  - `peri-acp/src/session/command/compact/events.rs`、`command/clear.rs` —— 命令 compact / clear 的 CompactCompleted 填 `CompactTrigger::Manual`
  - `peri-acp/src/session/event_sink.rs` —— 解构 trigger，经 `to_serde_str` 映射到 `AcpEvent::CompactCompleted.trigger`
  - `peri-acp/src/event/mod.rs` —— `AcpEvent::CompactCompleted` 新增 `trigger: String`（`#[serde(default = "default_compact_trigger")]` → "auto"）
  - `peri-tui/src/kit/acp_notifier.rs` —— AcpEvent → AcpEventData 透传 trigger
  - `peri-tui/src/kit/acp_events/compact.rs` —— **仅 `"manual"` 置 `compact_just_completed`**；auto 不置位（边缘洞根治）；方案 B 的流事件清除逻辑保留为防御
  - `peri-tui/src/kit/acp_events/mod.rs` —— dispatch 解构透传 trigger 参数
- **测试**：
  - `peri-agent/src/agent/events_test.rs` —— 新增 `test_compact_completed_legacy_json_defaults_trigger_to_auto`（旧 JSON 无 trigger → Auto，向后兼容锁定）
  - `peri-tui/src/kit/acp_events_test.rs` —— 场景 B 改为 `trigger: "auto"`（断言**不置位**）；新增场景 B2（manual 置位 + 流事件清除防御路径仍有效）
- **验证状态**：已验证（`cargo check --workspace`、`cargo clippy --workspace --all-targets -- -D warnings` 全绿；peri-agent 646 passed、peri-acp 415 passed、peri-tui 895 passed 无回归）
- **残余风险**：无（auto 不置位、manual 置位，判定依据与命令来源强一致；旧服务端/旧 TUI 混合版本经 serde default 双向兼容）

### 修复 #1（2026-08-05）

- **方案**：B（TUI 侧最简——流事件清除标志）
- **改动文件**：
  - `peri-tui/src/kit/acp_events/mod.rs` —— `dispatch_and_notify` 入口流事件清除 `compact_just_completed`；`BridgeState.compact_just_completed` 字段注释更新
  - `peri-tui/src/kit/acp_events/turn.rs:42-48` —— TurnDone 判定简化为仅标志（删除 flush 后恒空的 `current_turn.is_empty()`）
  - `peri-tui/src/kit/acp_events_test.rs` —— 场景 B 按真实时序（CompactCompleted → TextChunk → TurnDone）构造，补 `rx.try_recv().is_err()` 断言（红测试锁定 bug：修复前 TextChunk 不清标志，断言 "场景 B: 流事件到达后标志应清除" 失败）
- **验证状态**：待验证（红测试 → 修复 → 绿测试已在本机通过；`cargo build -p peri-tui`、`cargo test -p peri-tui --lib` 894 passed、`cargo clippy -p peri-tui --lib -- -D warnings` 通过）
- **残余风险**：auto-compact 后无任何流事件直接 TurnDone 的边缘场景（CompactCompleted 注入的 SystemNote 非流事件、不清标志）仍会误触发重放——概率低（compact 发生于 agent 产出过程中），如需彻底消除需方案 A（服务端透传 CompactTrigger）；TurnInterrupted/TurnSuspended 不清标志，取消 compact 后标志残留至下一 turn（下一 turn 无流事件时可能误触发，边界极窄）

（由 auto-issue-fixer 修复阶段追加，创建时留空）
