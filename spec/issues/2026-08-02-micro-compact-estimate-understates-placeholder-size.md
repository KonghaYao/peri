# micro compact 空字段投影按 after=0 估算，短参数实际变大却报节省

**状态**：Open
**优先级**：低
**创建日期**：2026-08-02

## 问题描述

micro compact 的整条压缩兜底（`fields: vec![]`）实际渲染为约 40 字符的 `{"_compact_note": "tool input compacted"}` 占位，但 `estimate_projection_chars` 对该路径记录 `after = 0`。短参数工具调用（如 `{"cmd":"ls"}`，12 字符）被替换为 40 字符占位、实际变大，估算却报告"节省"，`estimated_tokens_saved` 虚高。

来源：code review（`target/review.md`，Minor）。

## 症状详情

- `projection.rs` 约 730-740 行：空 fields 渲染为 `{"_compact_note": "tool input compacted"}`（约 40 字符）。
- `projection.rs` 约 312-318 行：`estimate_projection_chars` 对空 fields 计 `before += 整条序列化长度`、`after += 0`。
- 对比：12 字符输入 → 40 字符占位，`estimated_tokens_saved = (12 - 0)/4 = 3`，实际为负收益。
- `MicroCompactPlan.estimated_tokens_saved` 进入 CompactCompleted 等事件上报。

## 复现条件

- **复现频率**：必现（短参数 object 工具调用触发兜底压缩时）
- **触发步骤**：
  1. 触发 micro compact，存在短参数工具调用（无超长字段）
  2. 观察 CompactCompleted 事件报 estimated_tokens_saved > 0，但实际序列化变大
- **环境**：任意 micro compact 触发场景

## 期望改进方向

- 空 fields 路径按占位实际长度（约 40 字符）计入 `after`，使估算接近真实收益。
- （可争议）若占位不小于原始输入，考虑跳过该 action——注意这与「短参数也要压缩、防 micro compact 静默失效」的既有设计意图冲突，需权衡。

## 涉及文件

- `peri-agent/src/agent/compact_v2/projection.rs` —— `estimate_projection_chars`（约 312-318 行）与占位渲染（约 730-740 行）
- `peri-agent/src/agent/compact_v2/planner.rs` —— 兜底 action 生成（约 322-337 行）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建（来源：code review） |
| 2026-08-02 | Open | Fixed | agent | 修复: estimate 空字段分支把 `{"_compact_note":"tool input compacted"}` 占位实际长度计入 after，占位构造提取为渲染/估算共享 helper；planner 的 debug_assert 对兜底路径放宽（短输入下 after 可大于 before） |

## 修复记录

- **改动摘要**：
  - `peri-agent/src/agent/compact_v2/projection.rs`：新增 `compact_note_placeholder()`（返回 `{"_compact_note":"tool input compacted"}`），`project_tool_input` 与 `estimate_projection_chars` 共用，保证占位内容/长度单一事实源；`estimate_projection_chars` 空 fields 分支由 `after += 0` 改为按占位实际序列化字符数计入。
  - `peri-agent/src/agent/compact_v2/planner.rs`：`debug_assert!(after_chars <= before_chars)` 在存在空 fields 兜底 action 时豁免——短参数被替换为更大占位时估算如实反映放大（沿用「宁可放大也不静默 no-op」的既有权衡，未采纳跳过投影方案）。
  - 未改渲染行为、未改 planner 兜底 action 生成。
- **验证结果**：`cargo check -p peri-agent --all-targets` 通过；`cargo test -p peri-agent --lib compact_v2` 150 passed（修复前 4 个失败均为 planner.rs:417 断言在 debug 下 panic，放宽断言后全部恢复）。
