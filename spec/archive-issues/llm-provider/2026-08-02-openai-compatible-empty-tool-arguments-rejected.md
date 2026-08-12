> 归档于 2026-08-11，原路径 spec/issues/2026-08-02-openai-compatible-empty-tool-arguments-rejected.md

# OpenAI-compatible 响应中空字符串工具参数导致整个响应被拒

**状态**：Fixed
**优先级**：中
**创建日期**：2026-08-02

## 问题描述

部分 OpenAI-compatible provider 对无参数工具调用返回 `"arguments": ""`。`serde_json::from_str("")` 失败，整个响应被当作 provider 协议错误拒绝，一次本可成功的调用直接失败；`arguments` 字段缺失时同样失败。

来源：code review（`target/review.md`，Minor）。

## 症状详情

- `peri-model/src/openai_compatible/response.rs` 约 89-96 行：
  - `function.get("arguments").and_then(Value::as_str).ok_or_else(provider_protocol_error)?` —— 缺失即报错。
  - `serde_json::from_str(arguments)` 对空串解析失败 → `ok_or_else(provider_protocol_error)?`。
- 空串或缺失的 arguments 本应等价于 `{}`。

## 复现条件

- **复现频率**：取决于 provider（部分 OpenAI-compatible 端点对无参工具返回空串）
- **触发步骤**：
  1. 使用此类 provider 发起一次包含无参工具调用的请求
  2. 观察响应被判定为 protocol error，整个轮次失败
- **环境**：OpenAI-compatible 适配器 + 返回空 arguments 的端点

## 期望改进方向

- 缺失或空白（trim 后为空）的 arguments 按空对象 `{}` 处理；非空值保持现有 JSON 解析与错误语义。

## 涉及文件

- `peri-model/src/openai_compatible/response.rs` —— 工具参数解析（约 89-96 行）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-02 | — | Open | agent | 创建（来源：code review） |
| 2026-08-02 | Open | Fixed | agent | 修复: arguments 缺失或空白（trim 后为空）按空对象 JsonObject::default() 处理，非空值保持原解析与协议错误语义 |
| 2026-08-11 | Fixed | Fixed | agent | 终态确认归档：空字符串工具参数容错（arguments 缺失/为空不再拒响应），修复记录见正文 |

## 修复记录

- **改动摘要**：`peri-model/src/openai_compatible/response.rs` `decode_assistant_message` 工具调用解析：`arguments` 先取字符串、trim、过滤空串，空/缺失时返回 `JsonObject::default()`（空对象），非空时保持原有 `serde_json::from_str` + `JsonObject::from_value` 解析及 provider_protocol_error 语义。
- **验证结果**：`cargo check -p peri-model --all-targets` 通过；`cargo test -p peri-model --lib openai_compatible` 13 passed。
