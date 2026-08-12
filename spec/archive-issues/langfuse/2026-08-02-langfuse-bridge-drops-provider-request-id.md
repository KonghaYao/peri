> 归档于 2026-08-11，原路径 spec/issues/2026-08-02-langfuse-bridge-drops-provider-request-id.md

# Langfuse 桥接层丢弃 provider request_id，遥测无法关联

**状态**：Fixed
**优先级**：中
**创建日期**：2026-08-02

## 问题描述

Langfuse generation 的元数据里拿不到 provider 返回的 request_id。`ExecutorEvent::LlmCallEnd` 与 `ObserveEvent::LlmCallEnd` 都携带 `request_id` 字段（已从 TokenUsage 提升为独立字段），但转换到 `UnifiedLangfuseEvent::LlmCallEnd` 时被 `..` 丢弃，`LangfuseTracer::on_llm_end` 也不包含该字段。无法用 request_id 关联 provider 侧日志与 Langfuse 数据。

来源：code review（`target/review.md`，Major）。

## 症状详情

- `from_executor_event`（bridge.rs 约 169-180 行）：`ExecutorEvent::LlmCallEnd { .. }` 模式匹配丢弃 `request_id`。
- `from_observe_event`（bridge.rs 约 385-414 行）：同样丢弃 `request_id`。
- `UnifiedLangfuseEvent::LlmCallEnd` 无 request_id 字段；`on_llm_end` 签名（tracer/mod.rs 约 399 行）也不接收它。
- 对照：`peri-acp/src/event/mapper.rs` 的 `UsageUpdate` 已正确透传 request_id（`requestId` meta），TUI 侧有而 Langfuse 侧无。

## 复现条件

- **复现频率**：必现（provider 返回 request_id 时）
- **触发步骤**：
  1. 运行一次带 Langfuse 追踪的对话（v1 或 v2 事件路径均可）
  2. 在 Langfuse 查看 generation 元数据，无 provider requestId 字段
- **环境**：任何启用 langfuse 的会话

## 期望改进方向

- 在 `from_observe_event` 和 `from_executor_event` 中保留 `request_id`。
- `on_llm_end` 将其写入 generation 元数据；token usage 与 output 处理保持不变。

## 涉及文件

- `peri-acp/src/langfuse/bridge.rs` —— 两个事件转换函数（约 169-180、385-414 行）
- `peri-acp/src/langfuse/tracer/mod.rs` —— `on_llm_end`（约 399 行）
- `peri-agent/src/agent/events.rs` / `events_v2.rs` —— 事件源字段定义

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建（来源：code review） |
| 2026-08-02 | Open | Fixed | agent | 修复: UnifiedLangfuseEvent::LlmCallEnd 增加 request_id 字段并透传，on_llm_end 写入 generation metadata |

## 修复记录

### 修复 #1（2026-08-02）

- **操作人**：agent
- **根因**：`UnifiedLangfuseEvent::LlmCallEnd` 无 `request_id` 字段，`from_executor_event` / `from_observe_event` 用 `..` 丢弃事件源携带的 `request_id`，`LangfuseTracer::on_llm_end` 也不接收该字段。
- **修复内容**：
  - `UnifiedLangfuseEvent::LlmCallEnd` 增加 `request_id: Option<String>` 字段（枚举无 serde derive，无持久化格式兼容问题）。
  - `from_executor_event`（v1 路径）与 `from_observe_event`（v2 路径）模式匹配取出 `request_id` 并透传。
  - `LangfuseTracer::on_llm_end` 签名增加 `request_id: Option<&str>` 参数，无条件写入 Generation metadata 的 `request_id` 键（与 usage 独立，usage 缺失时也不丢失）；token usage 与 output 处理保持不变。
  - 同步更新调用点：`bridge.rs process_event` + 测试（tracer_test.rs 4 处、langfuse_e2e.rs 4 处）。
- **涉及文件**：`peri-acp/src/langfuse/bridge.rs`、`peri-acp/src/langfuse/tracer/mod.rs`、`peri-acp/src/langfuse/tracer/tracer_test.rs`、`peri-acp/tests/langfuse_e2e.rs`
- **验证状态**：`cargo check -p peri-acp --all-targets` 通过；`cargo test -p peri-acp --lib langfuse` 83 passed、`cargo test -p peri-acp --test langfuse_e2e` 5 passed。
