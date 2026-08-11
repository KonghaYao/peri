# TUI 无法感知 thinking 结束：补全 ACP messageId 语义 + 文本到达冻结推理

**状态**：Open
**优先级**：中
**创建日期**：2026-08-11

## 问题描述

TUI 不知道模型 thinking 何时结束：推理结束后开始输出正文时，消息区 `◐ Thinking… Ns` 动画持续显示，直到整个 turn 结束（TurnDone）才冻结为 `Thought for Ns`。典型对话（思考 → 回答）每次都会出现。

## 根因分析

事件链（agent → ACP → TUI）上不存在显式"推理结束"事件，TUI 只能靠隐式信号推断，而主路径的隐式信号缺失：

1. **Agent 侧**（`peri-agent/src/agent/model_bridge.rs`）：流式解析只 emit `ThinkingChunk`/`TextChunk`，无 reasoning-done 事件；且 v2 `RenderEvent::TextChunk`/`ThinkingChunk` 无 message 级身份。
2. **ACP 转换**（`peri-acp-types/src/event_v2.rs`）：`ThinkingChunk → AiReasoning` 时丢弃消息身份；`TextChunk` 用 turn_id 填充 message_id——同一 turn 所有迭代共享，恒不变化。
3. **ACP 映射**（`peri-acp/src/event/mapper.rs`）：`ContentChunk::new(...)` 从不设置 `.message_id(...)`——wire 上 messageId 恒缺失。
4. **TUI**（`peri-tui/src/kit/acp_types.rs`）：`append_text`/`append_reasoning` 的 messageId 变化 → flush 逻辑按 ACP 标准语义实现，但因发射侧 messageId 恒 None 而空转；推理段冻结只剩两个触发点（ToolStart flush / phase 离开 PromptRunning）。

**ACP 标准本身有答案**（不重复建设）：
- v1：`ContentChunk.messageId`（可选）——"同一消息的 chunk 共享 ID；变化即新消息"。无 reasoning-done 事件；消息内 thinking→text 切换无显式信号。
- v2 draft：`AgentThought` upsert（thought 定型语义）+ `StateUpdate::Idle`（session 级工作完成）——本项目当前用 v1，未采用。

## 修复方案（方案 1：补协议实现）

1. **发射侧补全 messageId**（每条 assistant 消息 = 一次 LLM 调用）：
   - `model_bridge.generate_from_request`：每次流式调用生成 `MessageId::new()`，emit TextChunk/ThinkingChunk 携带；流式结束构建 `source_message` 时 `with_message_id` 对齐（wire messageId = 规范消息 ID）。
   - 非流式路径（`stages/act.rs`、`stages/tool_dispatch.rs`）：用 `ai_msg.id()`（与 transcript 消息 ID 一致）。
   - `session_replay.rs`：replay chunk 携带消息真实 ID。
2. **ACP 转换/映射透传**：`ExecutorEvent::AiReasoning` 增加 `message_id` 字段；`render_event_to_executor` 透传（不再用 turn_id 填充 TextChunk）；`mapper` 设置 `.message_id(...)`（v1 wire 为字符串 UUID）；`forwarder.extract_message_id` 支持 AiReasoning。
3. **TUI 冻结推理**（`CurrentTurn::append_text`）：文本到达 = 本消息 thinking 块已结束（模型流中 thinking 必先于 text）——冻结 trailing 推理块（新增 `trailing_reasoning_frozen_ms`：推理块 Completed/Collapsed + `Thought for Ns`，正文继续流式）。与 messageId 变化 flush 互补（messageId 缺失时同样生效），幂等（连续文本块不重复冻结），flush 切段时重置（新消息重新计时）。
   - `build_bubble_parts` 增加 `reasoning_running` 参数，支持"推理 Completed + 正文 Running"混合形态。

## 验证

- `cargo test --workspace --lib`：全绿（含 `test_fold_pass_reasoning_running_preview_then_completed_collapsed` 更新为新语义、`test_multi_turn_reasoning_preserved_in_committed` 保持）。
- `cargo clippy --workspace --all-targets`：无警告。
- ARC-EVENT-001 链路：发射（model_bridge/act/tool_dispatch）→ 协议序列化面映射（event_v2.rs）→ ACP 映射/转发（mapper/forwarder）→ TUI 消费（acp_notifier 已有 messageId 解析 + acp_types 冻结逻辑）全覆盖。

## 遗留

- v2 draft 的 `AgentThought` upsert / `StateUpdate::Idle` 语义未采用（v1 路径足够；未来 v2 迁移时可再评估）。
- reasoning→text→reasoning（同一消息内多 thinking 块，模型不支持）时长冻结不精确，可接受。
