> 归档于 2026-08-11，原路径 spec/issues/2026-08-05-stage-ended-missing-on-error-path.md

# run_stage Err 路径不 emit StageEnded（Langfuse 悬挂 span 不对称）

**状态**：Fixed
**优先级**：低
**创建日期**：2026-08-05
**关联计划**：`2026-08-05-core-flow-bugfix-plan.md` S1.4

## 问题描述

`run_stage` 中 `StageStarted` 无条件 emit，`StageEnded` 只在 `Ok` 分支 emit。LLM 失败、cancel、工具错误等路径（loop 内几乎所有退出路径）都会留下只有 Start 没有 End 的 Langfuse 阶段 span。`compact.rs` 特意避免了同类不对称（Skip 不 emit `CompactStarted`，`compact.rs:85-93`），但 `run_stage` 层没有同等处理。

## 症状详情

- `stages/mod.rs:557-572`：Err 分支不 emit `StageEnded` → 遥测数据残缺（悬挂 span），Langfuse 面板误导
- `compact.rs:125-133`：`CompactStarted` 在 select 前 emit；cancel 且未提交变更（`:203`）只有 Start 没有结束事件——与 `compact.rs:125` 注释自称的"Start→End 成对原则"矛盾
- 注意：已提交变更的 cancel 路径会 emit `MessagesCompacted`（`:172-201`），仅"未提交"路径孤立

## 复现条件

- **复现频率**：必现（任何 Err 退出路径）
- **触发步骤**：任一轮 LLM 失败、取消、工具错误后查看 Langfuse 阶段 span

## 涉及文件

- `peri-agent/src/agent/stages/mod.rs:557-572` —— `run_stage` Err 分支
- `peri-agent/src/agent/stages/compact.rs:125-133,203` —— cancel 未提交时 CompactStarted 孤立

## 修复方向（对抗 review 已确认）

- `run_stage` Err 分支补 emit `StageEnded { status: StageStatus::Error }`（先确认 `StageStatus` 有 Error 变体）
- compact cancel 未提交时补独立结束观测事件；**不应** emit `MessagesCompacted`（会误导遥测以为压缩发生了）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-05 | — | Open | agent | 创建（peri-agent 审查发现，对抗 review 验证） |
| 2026-08-05 | Open | Fixed | agent | 修复：run_stage Err 分支补 emit StageEnded{status: Error}；compact cancel 未提交时补发 CompactEnded{outcome: Interrupted} 配对事件（新增 ObserveEvent 变体，不 emit MessagesCompacted） |
| 2026-08-11 | Fixed | Fixed | agent | 终态确认归档：Err 路径 StageEnded emit 补齐（run_stage/compact 对称化），修复记录见正文 |

## 修复记录

### 修复 #1（2026-08-05）

- **操作人**：agent（Slice 1 编码切片，auto-devflow）
- **用户原意**：消除两类遥测不对称——① `run_stage` Err 路径只有 StageStarted 没有 StageEnded（Langfuse 悬挂 span）；② compact cancel 且未提交变更时 CompactStarted 无配对结束事件（与 compact.rs:125 "Start→End 成对原则"注释矛盾）
- **修复内容**：
  - **文件**：`peri-agent/src/agent/stages/mod.rs`（run_stage，:557-572 区域）
    - Err 分支补 emit `StageEnded { status: StageStatus::Error, duration_ms }`（`StageStatus::Error` 变体已存在，events.rs:120，无需新增）；Ok 分支不变
  - **文件**：`peri-agent/src/agent/stages/compact.rs`
    - 两处 cancel 未提交路径（select cancel arm :203 区域、r arm 胜出但 cancel 触发且未应用 :265 区域）补发 `CompactEnded { outcome: CompactOutcome::Interrupted }` 配对事件；**不** emit `MessagesCompacted`（那会误导遥测以为压缩发生了——issue 明确禁止）
    - 已提交变更的 cancel 路径（emit MessagesCompacted + InterruptedAfterCommit）不变
  - **文件**：`peri-agent/src/agent/events_v2.rs` — 新增 `ObserveEvent::CompactEnded` 变体（turn_id/agent_id/step/strategy/outcome），并补 `turn_id()`/`agent_id()` 匹配臂
  - **文件**：`peri-agent/src/agent/compact_v2/mod.rs` — `CompactOutcome` 新增 `Interrupted`（取消且未提交，与 `InterruptedAfterCommit` 互补；不加入 `has_applied_change` 列表）
  - **文件**：`peri-acp/src/langfuse/bridge.rs` — `from_observe_event` 补 `CompactEnded` 映射到 `UnifiedLangfuseEvent::CompactEnded`（闭合 compact span；outcome 字段区分取消路径，token 估算置 0）
  - **文件**：`peri-tui/src/kit/v2_bridge.rs` — 穷尽 match 补 `CompactEnded` 到 None 组（TUI 不渲染该观测事件；新增变体的必要编译影响）
  - **测试**：`peri-agent/src/agent/stages/compact_test.rs` 新增 `test_compact_stage_cancel_without_commit_emits_compact_ended`（预取消 turn token → biased cancel arm 立即胜出：断言 Err(Interrupted)、CompactStarted=1、MessagesCompacted=0、CompactEnded(Interrupted)=1）
- **验证状态**：待验证（build ✅ / peri-agent lib 640 tests ✅ / peri-acp lib 415 tests ✅ / peri-tui check ✅ / clippy -D warnings ✅）
