> 归档于 2026-07-18，原路径 spec/issues/2026-07-18-anthropic-adapter-tool-result-order-reversed.md
# Anthropic adapter 合并连续 Tool 消息时 tool_result 顺序反转

**状态**：Done
**优先级**：中
**创建日期**：2026-07-18

## 问题描述

`messages_to_anthropic` 将连续多条 `BaseMessage::Tool` 合并进同一条 user message 的 content 数组时，使用 `arr.insert(0, tool_result_block)` 把每个新 tool_result 插到数组**最前面**。当 assistant 一次发出多个并行 tool_use（t1, t2, t3）且结果按序返回时，合并后的 user message 中 tool_result 顺序为 `[r3, r2, r1]`——与 assistant message 中 tool_use 的顺序相反。

## 症状详情

示例：assistant 发出 `tool_use(t1), tool_use(t2), tool_use(t3)`，运行后依次产生三条 Tool 消息 r1, r2, r3。

- **期望**：合并后 user content = `[r1, r2, r3]`（与 tool_use 顺序一致）
- **实际**：合并后 user content = `[r3, r2, r1]`（每条新结果 `insert(0)` 插到最前）

单条 Tool 消息（最常见路径）不受影响——此时走 `result.push` 新建 user message。仅当 ≥ 2 条连续 Tool 消息合并时反转。Anthropic API 按 `tool_use_id` 配对，协议层面不会报错，但顺序不一致可能干扰模型对并行调用结果的对应理解（尤其结果内容相似时）。

该转换是确定性的，不影响 prompt cache 稳定性；这是纯正确性问题。

## 复现条件

- **复现频率**：必现（当触发条件满足时）
- **触发步骤**：
  1. 任意会话中让 assistant 在同一轮发出 ≥ 2 个并行 tool_use
  2. 各工具执行完毕产生连续 Tool 消息
  3. 下一轮请求序列化时，这些 Tool 消息被合并进同一 user message，顺序反转
- **环境**：所有 Anthropic provider 会话

## 涉及文件

- `peri-agent/src/llm/anthropic/adapter.rs:179` —— `arr.insert(0, tool_result_block.clone())`，连续 Tool 消息合并点，应改为 `arr.push(...)`

## 期望改进方向

将 `insert(0, ...)` 改为 `push(...)`，保持 tool_result 与 tool_use 顺序一致；并补充连续 Tool 消息合并的序列化测试（断言多个 tool_result 在 user content 中的相对顺序）。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-18 | — | Open | agent | 创建（prompt cache 稳定性审计顺带发现） |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
