# Micro Compact 自动触发后无任何痕迹（不触发 + 消息区域无记录）——回归

> **状态**：Fixed | **优先级**：高 | **类型**：Bug | **日期**：2026-08-01

## 问题描述

长对话自动触发 Micro Compact 的场景下，用户完全看不到 micro compact 的任何痕迹：对话没有被压缩（连触发迹象都没有），TUI 消息区域也没有任何微压缩完成的记录。此现象为**回归**——以前长对话时能看到"微压缩完成"之类的提示且压缩生效，最近一两天突然消失。

## 症状详情

- **以前表现**：预算 ≥ 75% 自动触发 Micro Compact 后，对话被截断压缩，TUI 消息区域出现"微压缩完成"通知
- **当前表现**：同样场景下——
  1. **无触发迹象**：对话没有被截断/压缩，micro compact 似乎完全没发生
  2. **消息区域无记录**：TUI 消息区域没有任何 micro compact 相关的记录（SystemNote 等）
- **期望**：micro compact 正常触发执行，且执行后在消息区域有可见记录

## 复现条件

- **复现频率**：自动触发场景下必现（用户观察）
- **触发步骤**：
  1. 长对话进行，budget ≥ 75% 自动触发 Micro Compact
  2. 观察对话是否被压缩、消息区域是否有微压缩记录
- **回归时间线**：以前可见，最近一两天（约 2026-07-31 ~ 08-01）开始消失
- **环境**：默认配置，自动触发路径

## 观察事实（时间线线索，非根因结论）

- `spec/issues/2026-07-29-micro-compact-no-system-note.md`（Open）：当时已确认 Micro Compact 执行但 TUI 不显示 SystemNote，根因是 Debug 格式 vs snake_case 错配；`event_sink.rs` 现已改用 serde 序列化（`to_serde_str`），但用户仍看不到——且现在连压缩行为本身也未见触发
- 2026-08-01 有多个 TUI 消息区域/通知相关改动（如 `f90f9297` message area scroll padding、`cbad92f9` 状态栏通知行位置调整），与消失时间线吻合
- 用户同时报告"无触发迹象"，可能涉及 micro compact 触发链（`peri-agent/src/agent/compact_v2/`）

## 根因（修复时确认）

2026-07-30 合入的 field-level micro compact（`c28826de` Feature/20260729）重写了 `plan_micro()`：

| 行为 | 7-30 前（旧版） | 7-30 后（field-level） |
|------|-----------------|------------------------|
| tool_use 输入 | 无条件整条压缩（minimal 占位） | 仅压缩 >500 字符的顶层字符串字段 |
| tool result | 无条件截断（保留 2000 头 + 200 尾） | 仅压缩 >500 字符的成功结果 |
| 触发条件 | 有 stale 工具调用即触发 | 有超长字段才触发 |

副作用：普通工具调用（短参数、短结果）永远生成空 plan → `mod.rs:314` 判定 "plan 为空，无消息可 compact，跳过"（`CompactOutcome::Skipped`）→ 不 emit CompactStarted/CompactCompleted → TUI 无通知。两个现象（无触发迹象 + 消息区域无记录）同一根因。

## 修复记录

### 修复 #1（2026-08-01）

- **操作人**：agent
- **用户原意**：micro compact 的范围是工具调用，应对工具调用实际生效；不能"完全不起效果"
- **修复内容**：`planner.rs` `plan_micro()` 增加整条压缩兜底——字段级压缩（超长字段）优先，无超长字段时对 object 根的工具调用生成 `fields: vec![]` 的 `CompactToolInput` action（恢复 7-30 前 minimal 占位语义）。`projection.rs` `project_tool_input()` fields 空时渲染为 `{"_compact_note": "tool input compacted"}` 占位；`estimate_projection_chars()` fields 空时按整条 arguments 估算。保留安全边界：黑名单工具跳过、错误 ToolResult 保留、无效 field limits 时不生成兜底 action
- **涉及 commit**：待提交
- **验证状态**：已验证（620 tests pass；新增回归测试 `test_micro_compact_short_param_tool_call_still_compacts`；clippy 0 警告）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-01 | — | Open | agent | 创建 |
| 2026-08-01 | Open | Fixed | agent | 修复：plan_micro 增加整条压缩兜底，短参数工具调用恢复触发 |
