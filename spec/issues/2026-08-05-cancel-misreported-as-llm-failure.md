# 用户取消被误报为 LLM 失败（reason.rs match 两分支完全相同）

**状态**：Open
**优先级**：中
**创建日期**：2026-08-05
**关联计划**：`2026-08-05-core-flow-bugfix-plan.md` S1.3

## 问题描述

`reason.rs` 将 LLM 调用错误映射为 `TurnErrorReason` 的 match **两个分支完全相同**，都返回 `LlmFailure`：

```rust
let reason = match &e {
    AgentError::LlmHttpError { .. } | AgentError::LlmError(..) => TurnErrorReason::LlmFailure,
    _ => TurnErrorReason::LlmFailure,   // ← 两个分支一样，Interrupted 被吞
};
```

明显意图是区分 `AgentError::Interrupted`（`TurnErrorReason::Interrupted` 变体存在，`events_v2.rs:37`）。LLM 调用内部检测到 cancel（`model_bridge.rs:542-544` 的 `is_cancelled` → `Err(Interrupted)`）时，外层 biased select 的 cancel 分支未必抢先（微竞态），走 Err 分支后 emit `TurnError { reason: LlmFailure }`，用户取消被渲染为"LLM 失败"。

## 症状详情

- cancel 与 LLM 流式响应返回同时发生（竞态窗口，`reason.rs:231-235` 是 biased select，cancel 已触发时通常走 cancel 分支，仅微竞态可达）
- 影响面：`TurnError` 消费方主要是 langfuse bridge（`bridge.rs:545`）——遥测分类错误；TUI 红色 SystemNote 实际走 `executor_helpers.rs:612` 的 `AgentExecutionFailed`，UI 直接影响小
- `run_on_error(ctx, &e)`（`reason.rs:277`）在 cancel 时触发 middleware on_error 副作用（与现有 LlmFailure 路径一致，可接受）

## 复现条件

- **复现频率**：偶发（竞态窗口极小）
- **触发步骤**：用户取消与 LLM 流式响应返回同时发生
- **环境**：任意 LLM 提供方

## 涉及文件

- `peri-agent/src/agent/stages/reason.rs:264-269` —— match 死分支
- `peri-agent/src/agent/events_v2.rs:37` —— `TurnErrorReason::Interrupted` 变体（存在但未使用）

## 修复方向（对抗 review 已确认）

- 补 `AgentError::Interrupted => TurnErrorReason::Interrupted` 分支
- 测试：**必须 mock LLM 直接返回 `Err(Interrupted)`**（cancel 竞态窗口无法自然触发；`reason_test.rs` 已有 harness 可复用），并断言 `run_on_error` 行为
- 与挂起项 P4-3（LoopResult 双语义统一）有交互：若取消统一为 Interrupted，本分支只剩 LLM 内部自报 cancel 的场景——应先于 P4-3 修复

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-05 | — | Open | agent | 创建（peri-agent 审查发现，对抗 review 校准影响面） |
| 2026-08-05 | Open | Fixed | agent | 修复：reason.rs match 补 AgentError::Interrupted → TurnErrorReason::Interrupted 分支；mock LLM 直接返回 Err(Interrupted) 的测试锁定 reason 与 run_on_error 行为 |

## 修复记录

### 修复 #1（2026-08-05）

- **操作人**：agent（Slice 1 编码切片，auto-devflow）
- **用户原意**：LLM 调用内部检测到 cancel（`model_bridge.rs` is_cancelled → `Err(Interrupted)`，与 biased select cancel 分支微竞态）时，`TurnError` 应报 `Interrupted` 而非误报 `LlmFailure`（Langfuse 遥测分类错误）
- **修复内容**：
  - **文件**：`peri-agent/src/agent/stages/reason.rs`（:264-269）
    - match 补 `AgentError::Interrupted => TurnErrorReason::Interrupted` 分支（`events_v2.rs:37` 变体此前存在但从未使用）；其余 `_` 兜底仍映射 LlmFailure，行为不变
  - **测试**：`peri-agent/src/agent/stages/reason_test.rs`
    - 更新 `test_run_reason_emits_turn_error_on_llm_failure` → `test_run_reason_emits_turn_error_interrupted_on_null_llm`：NullReactLLM 直接返回 `Err(Interrupted)`（cancel 竞态窗口无法自然触发，mock 注入），断言 TurnError reason == Interrupted
    - 新增 `test_run_reason_interrupted_runs_on_error_with_interrupted`：注入 RecordingErrorMiddleware，断言 `run_on_error` 被调用一次且收到 Interrupted（副作用与现有 LlmFailure 路径一致，issue 声明可接受）
  - **与 P4-3 交互**：本修复先于 P4-3（LoopResult 双语义统一）；P4-3 若将取消统一为 Interrupted，本分支仅剩 LLM 内部自报 cancel 场景，无需改动
- **验证状态**：待验证（build ✅ / peri-agent lib 640 tests ✅，含 2 个相关测试）
