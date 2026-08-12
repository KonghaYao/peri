> 归档于 2026-08-11，原路径 spec/issues/2026-08-11-tui-think-end-messageid.md

# TUI 无法感知 thinking 结束：补全 ACP messageId 语义 + 文本到达冻结推理

**状态**：Fixed（2026-08-11 方案 2 精简落地；等待用户实测验证）
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

---

## 方案 2 精简落地（2026-08-11，状态 → Fixed）

**用户决策**：放弃全链路显式 `ReasoningDone` 信号（不动 peri-model/ACP wire），改为最小改动——**agent 层在工具块开始时提前发 ToolStarted**（工具块开始 = thinking 结束），TUI 冻结逻辑不变。

- **`peri-agent` `model_bridge.generate_from_request`**：流式循环内新增 `tool_start_emitted` 标志；首个带 id/name 的 `ToolCallDelta`（Anthropic `content_block_start` 原生携带 id/name）到达时立即 `emit_render(RenderEvent::ToolStarted { input: Value::Null, .. })`——工具参数尚未流式生成，input 置 Null；多工具并行仅首个 delta 发射，其余由 dispatch 正式发。幂等：后续无 id/name 的参数 delta 不再发。TUI 收到即 flush 冻结推理动画（`◐ Thinking…` → `Thought for Ns`），冻结点从"模型流结束 + dispatch"提前到"thinking 真实结束"。
- **`peri-tui` `CurrentTurn::start_tool`**：重复 tool_id 防御升级为 **input upsert**——提前 ToolStarted（raw_input=Null）与 dispatch 正式 ToolStarted（参数完整）同 id 先后到达时，只升级 `raw_input`/`input_summary`/`presentation`，不重建卡片（保留 `started_at`/时长语义）。subagent 路径（`child_turn.start_tool`）自动覆盖。
- **不影响**：v1 wire 事件形态不变（提前发的就是标准 `tool_call` 事件）；`ToolEnded` 按 tool_id 匹配不受影响；ToolRejected 路径（正式 ToolStarted + ToolEnd error）行为不变。
- **风险**：提前发后流中断（cancel/模型错误）时卡片保持 running 至 turn 结束——与既有 dispatch 失败路径同语义，可接受。

### 验证

- `cargo test -p peri-agent --lib`（657 passed，含新增 `bridge_emits_tool_started_on_first_tool_call_delta` 与更新 `bridge_preserves_completed_message_and_only_emits_visible_deltas`）。
- `cargo test -p peri-tui --lib`（1073 passed，含新增 `test_start_tool_duplicate_id_upserts_input`）。
- `cargo test -p peri-acp --lib`（317 passed）；`cargo clippy -p peri-agent -p peri-tui --all-targets` 无警告。
- 待用户实测：thinking → tool call 轮次，动画在 thinking 结束即冻结，工具参数生成期间推理块 Completed。

### 实测失效与根因（2026-08-11）

用户实测：**思考→工具（无正文）** 时动画仍空转到 dispatch 才冻结，所有 provider 一致。排除 provider delta 差异后，根因在 **TUI `sync_cache` 的缓存复用守卫**：

- 流式期间每 token eager sync，缓存尾部是 trailing bubble（推理块 **Running** 形态，index = 段数）。
- 提前 ToolStarted 到达 → `start_tool` → `flush_text_segment` 把 trailing 切成段（推理时长冻结）。
- `sync_cache` 遍历新段时守卫 `cached_view_models.len() <= i`（acp_types.rs:668）——缓存 len=段数+1 > 段索引，**跳过构建、复用陈旧 Running 形态 bubble**。
- 结果：推理段恒 Running，动画空转，直到 turn 结束折叠 pass 才翻转 Completed。dispatch 正式 ToolStarted 只是同 id upsert（不切段），冻结点被推迟到模型流结束。

**测试盲区**：`test_start_tool_duplicate_id_upserts_input` 等现有测试 append 后直接 start_tool、从不先调 `view_models()`（缓存为空 → 守卫 `len(0) <= 0` 成立 → 段正常构建），真实运行时每 token 渲染缓存必非空，必命中。

### 修复（方案 2 补丁）

`CurrentTurn::flush_text_segment`：切段后丢弃缓存尾部失效元素（flush 前 trailing 至多一个）——segment↔cache 索引对齐恢复，新段以 Completed 形态构建，历史冻结段缓存保留（不整缓存重建）。与 `start_subagent` 中部插入的整缓存清空同理、代价更低。

- 新增回归测试 `test_flush_segment_rebuilds_cached_reasoning_status`：先 `view_models()` 构建 Running 缓存 → `start_tool` 切段 → 断言推理块 Completed / !is_running（旧代码在此失败）。
- 同时覆盖方案 1 的 messageId 变化 flush 路径（同一守卫缺陷）与 `start_subagent`/`push_system_note` 的 flush 路径。

## 状态变更记录

| 日期 | 变更 | 说明 |
| --- | --- | --- |
| 2026-08-11 | Open → Fixed | 方案 1（messageId + 文本到达冻结）落地（affd126c） |
| 2026-08-11 | Fixed（方案 2 精简） | 提前发 ToolStarted + TUI input upsert，等待实测 |
| 2026-08-11 | Fixed（方案 2 补丁） | 实测失效：flush 切段后 sync_cache 复用陈旧 Running 缓存；`flush_text_segment` 丢弃失效尾部缓存 + 回归测试 |

---

## 方案 1 落地记录（affd126c，2026-08-11）

- 发射侧补全 messageId：v2 TextChunk/ThinkingChunk 携带消息级 `message_id`（`model_bridge.generate_from_request` 每次流式调用生成，流式结束 `with_message_id` 对齐）；非流式用 `ai_msg.id()`；`session_replay` 携带真实 ID。
- ACP 透传：`ExecutorEvent::AiReasoning` 增加 `message_id`；`render_event_to_executor` 不再以 turn_id 填充；`mapper` 设置 `.message_id(...)`；`forwarder.extract_message_id` 支持 AiReasoning。
- TUI 冻结：`CurrentTurn::append_text` 文本到达 → `trailing_reasoning_frozen_ms` 冻结（推理 Completed/Collapsed + `Thought for Ns`，正文继续 Running）；`build_bubble_parts` 支持混合形态；幂等；flush 切段时重置。

## 备选方案：显式 ReasoningDone 信号（未采用，仅存档）

> 2026-08-11 用户决策改为上文"方案 2 精简落地"（提前发 ToolStarted）；本节全链路设计保留存档，未来 v2 迁移时可再评估。

### 用户实测确认（2026-08-11）

思考完**直接调用工具**（无正文文本）时，`◐ Thinking…` 动画持续到 ToolStart 事件到达（段 flush）才冻结——thinking 真实结束点到工具卡片出现之间（工具参数仍在流式生成，可能数秒）动画空转。方案 1 的"文本到达"推断在此场景永不触发（无正文），冻结点被推迟到 flush。

### 根因补充：上游信号在模型层被丢弃

Anthropic 流式协议原生提供 thinking 块结束信号 `content_block_stop`（`peri-model/src/anthropic/stream.rs:285 finish_block`），但当前实现只把累积文本 push 进 `state.content`，**不产生任何事件**——thinking 结束的信息从模型层就丢了，下游只能靠推断。OpenAI-compatible 流（`peri-model/src/openai_compatible/stream.rs`）同样无 reasoning 结束事件。

### 链路设计

1. **`peri-model`**：`ModelStreamEvent` 增加 `ReasoningDone` 变体。
   - Anthropic：`finish_block` 对 `ActiveKind::Thinking | Redacted` emit `ReasoningDone`（`content_block_stop` 原生信号，精确）。
   - OpenAI-compatible：无块级事件，用推断——reasoning 非空状态下收到 content/tool_calls delta（reasoning_content 停止）即 emit（比"正文到达"早，thinking→tool 场景在 tool_calls 出现时结束）。
2. **`peri-agent`**（`model_bridge.generate_from_request`）：收到 `ReasoningDone` → emit v2 `RenderEvent::ThinkingDone { turn_id, agent_id, message_id }`（复用本次流式调用的 message_id）。
3. **`peri-acp`**：
   - `render_event_to_executor`：`ThinkingDone` → `ExecutorEvent::AiReasoningDone { message_id, source_agent_id }`。
   - 路由（`session/event_sink.rs::push_event`）：`AiReasoningDone` → `AcpEvent::AiReasoningDone { message_id }` 走 `peri/agent_event` 内部通道（TUI-only，categories ②③ 路径）——**不触碰 v1 wire**：`agent-client-protocol-schema 1.4.0` `SessionUpdate` 变体列表无 reasoning-done 事件，外部 v1 客户端保持标准语义，不加非标准扩展。
4. **`peri-tui`**：
   - `acp_notifier.rs convert_agent_event`：`AcpEvent::AiReasoningDone` → `AcpEventData::ReasoningDone`。
   - `CurrentTurn`：新增冻结入口（与 `append_text` 的 `trailing_reasoning_frozen_ms` 共用逻辑）——thinking→tool 场景冻结点从 ToolStart flush 提前到 `content_block_stop`；同时 **tool call 流式生成期间推理块以 Completed 形态展示**（`Thought for Ns` + 工具卡片 Running），不再等 flush。
   - flush 幂等：已冻结的段切走时不受影响；新消息（messageId 变化）重新计时。

### 待确认点

- Anthropic 同一消息内多 thinking 块（thinking→text→thinking interleave）：每个块独立 `content_block_stop`——TUI 冻结后新块到达需解冻继续转。Anthropic 实践中 thinking 仅出现在消息开头，先按"冻结后新块到达解冻"实现幂等，实测再定。
- `redacted_thinking` 块是否 emit（TUI 对 `ContentBlock::RedactedReasoning` 的渲染口径）。
- OpenAI-compatible 的 reasoning_content 结束判定（空串 delta / 切 content / tool_calls / finish_reason）以实际 provider 流为准，实现时用 fixture 验证。
- `ExecutorEvent::AiReasoningDone` 是否进入 `forwarder.extract_message_id` 归组（不产生 v1 wire 输出，仅内部通道）。

### 验证

- `cargo test --workspace --lib`（含 anthropic stream fixture：thinking 块 `content_block_stop` → ReasoningDone；model_bridge 事件发射；TUI 冻结路径）。
- `cargo clippy --workspace --all-targets`。
- 手动：thinking → tool call 轮次，动画在 thinking 结束即冻结，工具参数生成期间推理块 Completed。
