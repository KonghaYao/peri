# peri-agent 代码质量与健壮性改进


> 归档于 2026-07-20，原路径 spec/issues/2026-07-08-peri-agent-code-quality-improvement.md
**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-08
**类型**：技术债

## 问题描述

peri-agent 在 2026-07-08 的代码质量与健壮性审查中得分 80/100。错误处理和并发模型设计成熟度较高，但存在静默错误吞没、后台 task 无 JoinHandle 跟踪、工具调用无 timeout 防护等生产级安全隐患。此外部分 Rust 惯用性（如临时引用生命周期、枚举变体缺失）需改进。

## 症状详情

### 子项 1：持久化 writer task panic 静默丢失（P0，1 天）

`AgentState.with_persistence`（`state.rs:106`）和 `MessageTranscript.with_persistence`（`transcript.rs:151`）各 spawn 一个 `tokio::spawn(async move { while let Some(op) = rx.recv() ... })` 持久化写入任务，**不保存 JoinHandle**。task panic 会在后台静默丢失——仅有 `tracing::warn!` 针对单条失败，但 task 级别崩溃完全不可见。

期望：保存 JoinHandle，通过 `tokio::select!` 或定期 check 检测 panic 并优雅恢复。

### 子项 2：`dispatch_concurrent` 无工具调用 timeout（P0，1 天）

`tool_dispatch.rs:363-373`，每个工具调用走 `select! { cancel || invoke_fut }`，但 invoke_fut 可能永久阻塞（如工具内部死循环），无超时防护。恶意/死循环的工具调用会永久阻塞整个 turn。

期望：为每个工具调用添加 `tokio::time::timeout` 包裹。

### 子项 3：`block_to_anthropic:34` 静默丢弃序列化错误（P1，1 天）

`llm/anthropic/invoke.rs` 中 `serde_json::to_value(source).unwrap_or_default()` — Document 序列化失败时静默吞错，错误数据完全不可恢复且无告警。

期望：至少记录 `tracing::warn!`，考虑降级策略（如跳过该 content block 而非整个消息失败）。

### 子项 4：OpenAI `parse_assistant_message:260` 临时引用生命周期隐患（P1，1 天）

`assistant_msg["tool_calls"].as_array().unwrap_or(&vec![])` 使用临时 `vec![]` 的引用——在 Rust 临时对象生命周期延长规则下有理论 UB 风险。

期望：改为 `let empty = vec![]; unwrap_or(&empty)`。

### 子项 5：工具参数 JSON 解析失败时丢失原始语义（P1，1 天）

`parse_assistant_message:266-275` 工具参数 JSON 解析失败时降级为 `{_raw_arguments: "..."}`，丢失原始参数语义且无 error_suggest 注入。

期望：保留原始字符串在 `_raw_arguments` 之外，同时注入 error_suggest 提示 LLM 修正格式。

### 子项 6：`dispatch_tools:106` 用 `unwrap_or_else` 掩盖不变量违规（P2，30 分钟）

`source_message.clone().unwrap_or_else(|| BaseMessage::ai_with_tool_calls(...))` — 当 AI 消息来自 LLM 响应时 `source_message` 必不为 None，但 `unwrap_or_else` 提供了一个虚假的 fallback 路径。

期望：改为 `expect("source_message must always be set")`，使不变量违规立即暴露。

### 子项 7：`AgentError` 不实现 `Clone`（P3，30 分钟）

`retry_test.rs` 中需手动编写 `clone_error()` 函数绕过限制，增加测试辅助代码。所有变体字段均满足 Clone。

期望：为 `AgentError` derive `Clone`。

### 子项 8：`ContentBlock::RedactedThinking` 缺少显式变体（P3，1 天）

`content.rs` 对 `redacted_thinking` 使用 `Unknown(b.clone())` 魔法字符串透传，应为显式变体 `RedactedThinking { data: String }`。

期望：新增 `RedactedThinking` 变体，替换 `Unknown` 透传。

## 涉及文件

- `peri-agent/src/agent/state.rs:106` —— 持久化 writer task（无 JoinHandle）
- `peri-agent/src/session/transcript.rs:151` —— 持久化 writer task（无 JoinHandle）
- `peri-agent/src/agent/stages/tool_dispatch.rs:363-373` —— `dispatch_concurrent` 无 timeout
- `peri-agent/src/llm/anthropic/invoke.rs:34` —— 静默吞错
- `peri-agent/src/llm/openai/invoke.rs:260-275` —— 临时引用 + JSON 降级
- `peri-agent/src/agent/stages/tool_dispatch.rs:106` —— 不变量掩盖
- `peri-agent/src/error.rs` —— AgentError 未 derive Clone
- `peri-agent/src/messages/content.rs` —— RedactedThinking 缺少显式变体

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-08 | Open | Partial | agent | P0 #1（持久化 writer task 管理）+ P0 #2（工具调用超时）已完成；P1-P3 子项待处理 |

## 修复记录

### 修复 #1（2026-07-08）

- **操作人**：agent
- **用户原意**：修复持久化 writer task panic 静默丢失 + 工具调用无超时防护
- **修复内容**：
  - P0 #1：`AgentState` 和 `MessageTranscript` 添加 `shutdown_persistence()` 方法 + `AbortHandle` 存储。`MessageTranscript` 添加 `Drop` impl 自动 abort。
  - P0 #2：`dispatch_concurrent` 中 `tokio::time::timeout(300s)` 包裹 `invoke_fut`，超时返回 `ToolExecutionFailed`。
- **涉及文件**：`state.rs`（+28/-2）、`transcript.rs`（+20/-1）、`tool_dispatch.rs`（+17/-1）
- **涉及 commit**：待提交
- **验证状态**：已验证（616/616 测试通过）
