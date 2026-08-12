# bg subagent 运行期流式事件把主 agent loading 拉回 true

**状态**：✅ 已修复（2026-08-12）
**优先级**：高
**类型**：bug
**创建日期**：2026-08-12
**来源**：用户报告（TUI 运行观察：主 agent 派发 bg subagent 后回复完成，bg 仍在运行，loading 不退出）
**最后核查**：2026-08-12

## 问题描述

主 agent 派发 bg subagent（`run_in_background: true`）后完成本轮回复，bg subagent 仍在运行。此时主 agent 已空闲，loading spinner 应退出（bg 运行状态由 BG 区域/Tasks 面板跟踪），但 TUI 持续 loading 直到 bg 完成、主 agent 被唤醒产出新 turn。

## 复现条件

1. 主 agent 在一次回复中调用 Agent 工具（`run_in_background: true`）
2. 主 agent 输出完剩余回复文字，agent loop 因 `idle_should_wait`（bg `active_count() > 0`）进入 `await_wake`，emit `TurnSuspended`（`peri-agent/src/agent/stages/mod.rs:670`）
3. bg subagent 继续运行（思考/调工具/写代码）
4. **观察**：TUI loading 不退出，直到 bg 完成且主 agent 新 turn 结束

## 根因

TUI 侧 `TurnSuspended` 正确把 `phase` 置为 `Idle`（`turn.rs:192`，loading 清除 ✅），但 bg subagent 运行期间的流式事件**无条件把 `phase` 拉回 `PromptRunning`**，每条事件都触发 `push_acp_state` → `is_loading = phase == PromptRunning`（`render.rs:619`）：

| 事件 | handler | bg 分支 |
|------|---------|---------|
| TextChunk | `streaming.rs:38`（路由到 subagent 组） | 设 PromptRunning |
| TextChunk | `streaming.rs:53`（组被 turn 边界清除，`is_bg_agent_without_group`） | 设 PromptRunning |
| ReasoningChunk | `streaming.rs:92`（路由到 subagent 组） | 设 PromptRunning |
| ReasoningChunk | `streaming.rs:105`（组被 turn 边界清除） | 设 PromptRunning |
| ToolStarted | `tool.rs:30`（仅更新 BG_DISPLAY） | 设 PromptRunning |
| ToolEnded | `tool.rs:112`（仅更新 BG_DISPLAY） | 设 PromptRunning |

bg 持续输出 → phase 被反复拉回 `PromptRunning` → loading 一直转。

**为何此前修复未覆盖**：`90e2ea4c`（`SubagentStopped` 不再无条件覆盖 phase，`subagent.rs:49-50`）只封了 bg **完成**事件这一条路径，注释称"phase 由 SubagentStarted + 流式事件维护"——流式事件恰是漏洞所在。与归档 issue `2026-07-13-main-agent-done-loading-persists-bg-still-running` 同场景，该 issue 修复仅覆盖 `SubagentStopped` 覆盖 `TurnSuspended` 的链路，bg 运行期流式事件持续拉 loading 的链路未处理。

## 修复方案

bg agent（`BG_AGENT_IDS` 注册）相关事件分支**不再触碰 phase**（删除/不设置 `PromptRunning`）：

- 安全性论证：主 agent 推理期间 phase 由主 agent 自己的事件（PromptSubmitted / 主 agent chunk / tool / sync subagent）维持 `PromptRunning`，bg 事件设不设无差别；主 agent 挂起后（`Idle`）bg 事件不会把它拉回——loading 正确退出，bg 区域内容更新不受影响（`BG_DISPLAY` 照常写入）。
- 涉及分支：`streaming.rs` 两处 `is_bg_agent_without_group` 兜底分支、`routed_to_subagent` 命中 bg 时的分支；`tool.rs` 两处 `BG_AGENT_IDS` 分支。
- sync subagent 不受影响：其 chunk 路由分支可保留 `PromptRunning`（sync 场景主 agent 必在推理，且 `SubagentStarted` 已设 `PromptRunning`）。

**待验证**：`bg_task_area.rs` 渲染是否依赖 `phase == PromptRunning`（若依赖则需另寻 loading 判定来源）；`acp_events_test.rs` 现有断言是否依赖 bg 分支设 phase。

## 修复

`peri-tui/src/kit/acp_events/streaming.rs` + `tool.rs`：bg agent（`BG_AGENT_IDS` 注册）相关事件分支**不再触碰 phase**：

- `handle_text_chunk` / `handle_reasoning_chunk`：`routed_to_subagent` 分支命中 bg（查 `BG_AGENT_IDS`）时不设 `PromptRunning`；`is_bg_agent_without_group` 兜底分支删除 phase 赋值。sync subagent 不在 `BG_AGENT_IDS`，维持原行为。
- `handle_tool_started` / `handle_tool_ended`：bg 分支（仅更新 `BG_DISPLAY`）删除 phase 赋值；`last_pushed_text_len` 等 block 模式同步变量保留。
- 安全性：主 agent 推理期间 phase 由主 agent 自身事件（PromptSubmitted / 主 agent chunk / tool / sync subagent 的 `SubagentStarted`）维持，bg 事件不参与无副作用；主 agent 挂起后（`Idle`）bg 事件不再拉回。

已验证 `bg_task_area.rs` 渲染不依赖 phase；`BG_DISPLAY` 照常写入。

## 验证

- 新增回归测试 `acp_events_test.rs::test_bg_events_after_turn_suspended_keep_idle_loading`：bg 启动 → TurnSuspended → bg TextChunk / ReasoningChunk / ToolStarted / ToolEnded 全链路到达后 phase 保持 `Idle`、`is_loading` 保持 false；对照组（主 agent chunk 恢复 `PromptRunning`、sync subagent 不误伤）。
- `cargo test -p peri-tui --lib`：1108 passed, 0 failed。
- `cargo clippy -p peri-tui --all-targets -- -D warnings` 通过。

## 涉及文件

- `peri-tui/src/kit/acp_events/streaming.rs` —— 修复（bg 分支不触碰 phase）。
- `peri-tui/src/kit/acp_events/tool.rs` —— 修复（bg 分支不触碰 phase）。
- `peri-tui/src/kit/acp_events_test.rs` —— 回归测试。
- 未改：`turn.rs`（`TurnSuspended` 正确清 loading）、`subagent.rs`（`SubagentStopped` 已修复）。

## 关联

- 归档 `spec/archive-issues/tui-general/2026-07-13-main-agent-done-loading-persists-bg-still-running.md`：同场景，仅修 `SubagentStopped` 覆盖链路。
- 归档 `spec/archive-issues/subagent/2026-07-11-bg-multi-agent-loading-*.md`：`SubagentStopped` 无条件 `PromptRunning` 系列，已由 `90e2ea4c` 修复。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-12 | — | Open | agent | 根因定位：bg 运行期流式事件（chunk/tool）无条件设 `phase=PromptRunning` |
| 2026-08-12 | Open | 已修复 | agent | streaming.rs + tool.rs bg 分支不触碰 phase + 回归测试，随提交落地 |

## 修复记录

| 日期 | commit | 说明 |
|------|--------|------|
| 2026-08-12 | 随本次提交 | bg 分支不触碰 phase + 回归测试 |
