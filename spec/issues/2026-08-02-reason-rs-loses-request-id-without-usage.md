# reason.rs 在 usage 缺失时丢失 request_id，Langfuse 关联失效

**状态**：Fixed
**优先级**：低
**创建日期**：2026-08-02

## 问题描述

`reason.rs` 发射 `LlmCallEnd` 时，`request_id` 在 `reasoning.usage` 的 `map` 闭包内读取。若 provider 返回的响应没有 usage（`usage: None`），回退分支 `unwrap_or((0, 0, 0, 0, None))` 会连本可用的 `request_id` 一起丢弃，Langfuse 侧丢失 provider 请求 ID。

来源：code review（`target/review.md`，Minor）。

## 症状详情

- reason.rs 约 291-303 行：五元组 `(in_tok, out_tok, cache_create, cache_read, req_id)` 整体从 `reasoning.usage.as_ref().map(...)` 派生。
- `usage` 为 `None` 时走 `unwrap_or((0, 0, 0, 0, None))`，即使 `reasoning.request_id` 为 `Some` 也被替换为 `None`。
- `request_id` 与 `usage` 来源独立（model_bridge.rs：`response.usage().cloned()` 与 `response.request_id().map(...)` 分别赋值），两者可以不同时存在。

## 复现条件

- **复现频率**：偶发（provider 不返回 usage 时）
- **触发步骤**：
  1. 使用某 provider 发起一次不返回 usage 的调用（部分 OpenAI-compatible 端点）
  2. 观察 `LlmCallEnd` 事件的 `request_id` 为 None
- **环境**：任何 usage 缺失的响应路径

## 期望改进方向

- 将 `request_id` 从 usage 闭包中移出，独立读取（如 `let req_id = reasoning.request_id.clone()`）。
- usage 缺失时 token 字段仍归零，但 `request_id` 保持原值。

## 涉及文件

- `peri-agent/src/agent/stages/reason.rs` —— LlmCallEnd 发射逻辑（约 289-303 行）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建（来源：code review） |
| 2026-08-02 | Open | Fixed | agent | 修复: request_id 移出 usage 解包表达式，usage 缺失时不再丢失 |

## 修复记录

### 修复 #1（2026-08-02）

- **操作人**：agent
- **根因**：`request_id` 在 `reasoning.usage.as_ref().map(...)` 闭包末尾读取，usage 为 `None` 时整个五元组被 `unwrap_or((0, 0, 0, 0, None))` 替换，present 的 `request_id` 被丢弃。
- **修复内容**：五元组改为四元组 `(in_tok, out_tok, cache_create, cache_read)`（token 字段在 usage 缺失时仍归零）；`request_id` 独立读取 `let req_id = reasoning.request_id.clone()`，与 usage 解包结果分开，保证 usage 缺失时 request_id 保持原值。下游 `ObserveEvent::LlmCallEnd` 构造不变。
- **涉及文件**：`peri-agent/src/agent/stages/reason.rs`
- **验证状态**：`cargo check -p peri-agent --all-targets` 通过；`cargo test -p peri-agent --lib reason` 20 passed。
