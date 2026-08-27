# bg subagent 流式文本外溢到主回复气泡

**状态**：✅ 已修复（2026-08-12）
**优先级**：高
**类型**：bug
**创建日期**：2026-08-12
**来源**：用户报告（TUI 运行观察：bg agent 并发流式时 subagent 内容混入主回复）
**最后核查**：2026-08-12

## 问题描述

后台 subagent（bg agent）在主 turn 挂起/结束后继续流式输出时，其 TextChunk / ReasoningChunk 被错误地 append 进主 agent 的回复气泡——"subagent 信息外溢"。仅 bg 场景触发；同步 subagent 正常。

## 根因

1. **turn 边界清空 subagent 组**：`handle_turn_suspended`（`peri-tui/src/kit/acp_events/turn.rs`）与 `handle_turn_interrupted` 无条件 `current_turn.reset()`，清空所有 `SubAgentAccumulator`——包括仍在后台运行的 bg subagent。`flush_current_turn` 虽有 `has_running_subagent` 守卫，但这两条路径绕过了它（TurnDone 走 `flush_current_turn`，安全）。
2. **流式 chunk 路由无兜底**：bg 继续输出时 `append_subagent_text` 在已清空的组中找不到 agent_id → 返回 `false`。
3. **无条件回退主分支**：`streaming.rs::handle_text_chunk` / `handle_reasoning_chunk` 把"找不到组"直接当作主 agent 文本 append → bg 内容混入主回复。

对比佐证：`tool.rs` 对同一场景（TurnSuspended 后 bg 工具事件）已有 `BG_AGENT_IDS` 兜底（只更新 `BG_DISPLAY`，不进消息卡片），而 streaming.rs 缺失该兜底——即外溢窗口。

## 修复

`peri-tui/src/kit/acp_events/streaming.rs`：

- 新增 `is_bg_agent_without_group(agent_id)`：路由失败时先查 `BG_AGENT_IDS`（bg 运行期注册、`SubagentStopped` 才移除）。
- `handle_text_chunk` / `handle_reasoning_chunk` 路由失败分支：命中 bg 集合 → 跳过（bg 内容不进主消息区，与 tool.rs 同口径）；不命中 → 回退主分支（主 agent 回复不受影响）。

回归测试 `acp_events_test.rs::test_bg_subagent_chunk_after_turn_suspended_does_not_leak_to_main`：覆盖 bg 启动 → TurnSuspended → 文本/推理不得外溢，加对照组（无组且非 bg 的 chunk 仍正常回退主分支，防过度修复）。

## 验证

- `cargo clippy -p peri-tui --all-targets -- -D warnings` 通过。
- `cargo test -p peri-tui --lib`：1106 passed, 0 failed（含新增回归测试）。

## 残余窗口（未修，防御性说明）

`SubagentStopped` 移除 `BG_AGENT_IDS` 注册后，若协议乱序仍有该 agent 的 chunk 到达，仍会回退主分支。服务端有"Started 同步先于一切、Stopped 为终态"的顺序契约（`peri-agent/src/session/subagent.rs`），正常不触发；彻底封死需维护"已知 bg agent"集合，引入新状态，暂不处理。

## 涉及文件

- `peri-tui/src/kit/acp_events/streaming.rs` —— 修复（BG_AGENT_IDS 兜底 + 两个 handler 的分支）。
- `peri-tui/src/kit/acp_events_test.rs` —— 回归测试。
- 参考（未改）：`peri-tui/src/kit/acp_events/tool.rs` bg 兜底口径、`peri-tui/src/kit/acp_events/turn.rs` turn 边界 reset。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-12 | — | 已修复 | agent | 根因定位 + streaming.rs 兜底修复 + 回归测试，随提交落地 |

## 修复记录

| 日期 | commit | 说明 |
|------|--------|------|
| 2026-08-12 | 随本次提交 | 修复 + 回归测试 |
