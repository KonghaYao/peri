# Langfuse 中 LlmFailure 错误原因丢失，LLM 调用失败无法排查

**状态**：Open
**优先级**：中
**创建日期**：2026-08-03

## 问题描述

Langfuse 中 LLM 调用失败（LlmFailure）时，错误原因完全不可见：generation 的 `statusMessage` 为 null、level 为 DEFAULT（未标 ERROR），ErrorTurn span 的输出只有 `{"error":"LlmFailure"}` 枚举名。实际错误详情（provider 错误消息、HTTP 状态码、超时/限流等）在链路中丢失，无法用 Langfuse 排查 LLM 调用失败。

## 症状详情

| 现象 | 数据证据 |
|------|---------|
| LlmFailure 时 generation 无错误标记 | 8/2 04:22-04:25 连续 3 条 LlmFailure trace（019fc26c / 019fc26e / 019fc26f），其 step-1 generation level=DEFAULT、statusMessage=null、0 token |
| ErrorTurn span 无错误详情 | 6 个 LlmFailure ErrorTurn 输出均为 `{"error":"LlmFailure"}`，无 provider 错误消息 |
| 同 session 连续失败无法区分原因 | 019fc26b session 04:22、04:24 两次 LlmFailure，无法判断是超时/限流/网络错误还是同一原因 |
| 错误消息实际已产生但被丢弃 | `ObserveEvent::TurnError { message }` 携带完整错误（`e.to_string()`），但 bridge 的 `from_observe_event` 将其映射为 None 丢弃；`ObserveEvent::LlmCallEnd` 的 `output: "ERROR: {e}"` 也未被标记为错误 |

## 复现条件

- **复现频率**：必现（LLM 调用失败时）
- **触发步骤**：
  1. provider 返回错误（网络/超时/4xx/5xx），使 `generate_reasoning` 返回 Err
  2. 在 Langfuse UI 查看该 trace 的 generation 与 ErrorTurn span
- **环境**：deepseek-v4-flash（8/2 04:22 批次），本地 langfuse（localhost:23332）

## 涉及文件

- `peri-acp/src/langfuse/bridge.rs` —— `from_observe_event` 丢弃 `ObserveEvent::TurnError`
- `peri-acp/src/langfuse/tracer/mod.rs` —— `on_llm_end` 不识别 `ERROR:` 前缀；`on_turn_end` 的 ErrorTurn 输出只用枚举名
- `peri-acp/src/session/executor_helpers.rs` —— `last_error` 只保留 `error_kind` 的 Debug 字符串
- `peri-agent/src/agent/stages/reason.rs` —— LlmFailure 时 emit `TurnError { message }` 与 `LlmCallEnd { output: "ERROR: {e}" }`（错误消息源头，无需改）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-03 | — | Open | agent | 创建（langfuse 闲逛发现） |
| 2026-08-03 | Open | Fixed | agent | 修复：TurnError message 透传 + generation 错误标记（见修复记录） |

## 修复记录

### 修复 #1（2026-08-03）

- **操作人**：agent
- **用户原意**：LLM 调用失败时能在 Langfuse 里看到具体错误原因，而不是只有 "LlmFailure" 四个字。
- **修复内容**：
  - `bridge.rs`：`ObserveEvent::TurnError { message }` 不再丢弃，新增 `UnifiedLangfuseEvent::TurnError` 变体并透传完整错误消息（此前映射为 None）。
  - `tracer/mod.rs`：新增 `last_turn_error` 字段 + `on_turn_error(message)` 方法；`on_turn_end` 的 ErrorTurn span / 合成 Trace 输出由 `{"error": "LlmFailure"}` 扩展为 `{"error": ..., "message": <完整错误>}`（取不到时保持原状，不劣化）。
  - `tracer/mod.rs` `on_llm_end`：检测 output 的 `ERROR: ` 前缀（reason.rs 失败路径标记），generation 标记 `level=Error` 并将完整错误写入 `statusMessage`。
  - 新增 2 个测试：`test_llm_error_marks_generation_error`、`test_turn_error_message_in_error_span`。
- **验证状态**：已验证 —— `cargo test -p peri-acp` 411 个测试全过；clippy `-D warnings` 通过；`cargo check --workspace` 通过。Langfuse UI 真实验证待下次 LLM 失败时确认。
