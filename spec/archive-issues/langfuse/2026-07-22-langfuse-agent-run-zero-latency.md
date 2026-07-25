# Langfuse Agent-run Observation 显示 0s Latency

**状态**：Archived
**优先级**：中
**类型**：Bug
**创建日期**：2026-07-22

## 问题描述

Langfuse UI 中 agent-run observation 的 Latency 始终显示为 0s（startTime == endTime）。在 Langfuse 控制台中观察到的现象：`startTime: 2026-07-22T11:00:12.659Z`, `endTime: 2026-07-22T11:00:12.659Z`，无法反映真实的 agent 运行耗时。

## 症状详情

- Agent-run observation（agent-run span）在 Langfuse UI 的 Latency 列始终为 0s
- 通过 curl 直接查询 Langfuse REST API 确认：startTime 与 endTime 完全相同
- 其他 observation（generation、tool span）的时间戳正常

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动 peri，发送任意 prompt
  2. 等待回答完成
  3. 在 Langfuse UI 中查看该 turn 的 trace
  4. 观察 agent-run observation 的 Latency
- **环境**：OTEL 上报路径（`/api/public/otel/v1/traces`）

## 涉及文件

- `peri-acp/src/langfuse/tracer/mod.rs` —— `on_turn_start` 在 turn 开始时发送 `ObservationCreate`（仅设 `start_time`），`on_turn_end` 发送 `ObservationUpdate`（尝试设 `end_time`）
- `langfuse-client/src/types/conversion.rs` —— OTEL 转换：`ObservationUpdate` 创建同名 span，但 OTEL span 属性不可变，`end_time_unix_nano` 更新被忽略

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-22 | — | Open | agent | 创建：e2e 测试后查询 Langfuse API 发现 agent-run latency 为 0s |
| 2026-07-22 | Open | Fixed | agent | 修复：将 agent-run ObservationCreate 从 on_turn_start 推迟到 on_turn_end |
| 2026-07-25 | — | Archived | agent | 归档

## 修复记录

### 修复 #1（2026-07-22）

- **操作人**：agent
- **用户原意**：修复 agent-run observation 在 Langfuse 中显示 0s latency
- **根因**：OTEL span 创建后 `end_time_unix_nano` 不可更新。原实现分两步：`on_turn_start` 发送 `ObservationCreate`（仅设 `start_time`，`end_time=None`），Langfuse 自动用 event timestamp 填充 `endTime`，导致与 `startTime` 相同；随后 `on_turn_end` 发 `ObservationUpdate` 尝试更新 `end_time`，但 OTEL 忽略更新
- **修复内容**：
  - `on_turn_start`：改为存储 `start_time` 和 `input` 到 struct 字段，不再立即发送 `ObservationCreate`
  - `on_turn_end`：取出存储的 `start_time` 和 `input`，在 `ObservationCreate` 中同时设置 `start_time` 和 `end_time`，确保 OTEL span 一次性获得正确的时间范围
- **涉及 commit**：待提交
- **验证状态**：cargo build 通过 / peri-acp 296 passed
