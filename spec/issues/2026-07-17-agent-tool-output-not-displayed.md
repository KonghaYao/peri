# Agent 工具返回值在消息区域不显示

**状态**：Open | **优先级**：P1 | **分类**：Bug / 显示异常 | **日期**：2026-07-17

## 症状

Agent 工具的 ToolCard（如 `● Agent (explore...)`）在消息区域正常显示，但其下方的输出摘要行为空。用户期望看到 SubAgent 的最终返回值（`format_subagent_result` 的产物）出现在 ToolCard 下方。

## 背景

此问题是 ACP-caps 修复（恢复 SubAgent 内部工具调用卡片显示）的后续发现。caps 修复后，SubAgent 内部的工具卡片恢复了，但 Agent 工具本身的返回值仍缺失。

## 调查发现

### 事件通道与数据流

Agent 工具输出通过以下路径到达 TUI：

1. **SubAgent 执行完成** → `define.rs`/`execute_fork.rs` 调用 `format_subagent_result(&output)` 生成 `result_text`
2. **result_text 成为 Agent 工具的返回值** → 由 `BaseTool::invoke()` 的 `Ok(result)` 返回
3. **tool_dispatch.rs** 在 Act 阶段调用 `emit_render(RenderEvent::ToolEnded { output, ... })`（`peri-agent/src/agent/stages/tool_dispatch.rs:482`）
4. **events_v2_mapper.rs** 映射为 `ExecutorEvent::ToolEnd { source_agent_id: None, ... }`（`peri-agent/src/agent/events_v2_mapper.rs:58`）
5. **event/mapper.rs** 映射为 `AcpEvent::ToolEnd`（`peri-acp/src/event/mapper.rs`），`rawOutput` 从 `output` 字段解析
6. **event_sink.rs** 通过 `session/update` 的 `tool_call_update` tag 发送（`TransportEventSink::push_event`）
7. **acp_notifier.rs** `handle_session_update` 从 `tool_call_update` 提取 `rawOutput` → `output_summary`
8. **acp_events.rs** `ToolEnded` handler → `current_turn.end_tool(tool_id, output_summary, is_error)` → 设置 `ToolCardAccumulator.output_summary`
9. **render.rs** `render_tool_card_lines` 检查 `!data.output_summary.is_empty()` 后渲染最多 4 行输出

### 关键发现：AgentOutput.tool_calls 恒为空

在 `define.rs:542-604` 和 `execute_fork.rs:222-234` 中，Agent 工具构造的返回结构为：

```rust
let output = peri_agent::agent::react::AgentOutput {
    text: final_text,
    steps: 0,
    tool_calls: Vec::new(),  // ← 恒为空的 Vec
    stop_reason: None,
    block_continue: None,
};
```

`format_subagent_result` 的逻辑：

```rust
fn format_subagent_result(output: &AgentOutput) -> String {
    if output.tool_calls.is_empty() {
        return output.text.clone();  // ← 直接返回 text
    }
    // ... 带工具统计的格式化
}
```

**问题链条**：`tool_calls: Vec::new()` → `format_subagent_result` 走 `if tool_calls.is_empty()` 分支 → 直接返回 `output.text`。

### 可能的根因场景

1. **`extract_last_ai_text` 返回空字符串**：
   - v2 transcript 的 `visible_messages()` 可能对 v2 SubAgent session 有不同行为
   - V2 SubAgent 的消息在 transcript 中的存储方式可能与预期不同

2. **`final_text` 在非 interrupted 路径下仍为空**：
   - `define.rs:582-586`：
     ```rust
     let final_text = extract_last_ai_text(&v2_ctx.session);
     if interrupted {
         return Ok("Sub-agent execution was interrupted".to_string());
     }
     ```
   - 如果 `interrupted == false` 但 `final_text` 为空，`format_subagent_result` 返回空字符串

3. **`LoopResult::Interrupted` 产生空 final_text**：
   - `define.rs:577` 附近，`interrupted` 标志是 `LoopResult::Interrupted` 且 `final_text.is_empty()`
   - 正常完成的 SubAgent 路径是否也走到此分支？

### 后续调查方向

1. **添加诊断日志**：在 `define.rs` 和 `execute_fork.rs` 的 `format_subagent_result` 调用前，记录 `final_text` 的长度和前缀
2. **追踪 transcript 内容**：在 SubAgent 完成后，dump `v2_ctx.session.transcript()` 的 `visible_messages()` 以确认 AI 消息是否存在
3. **检查 v2 transcript 兼容性**：`extract_last_ai_text` 遍历 `tx.visible_messages()` 反向查找 `BaseMessage::Ai`，确认 V2 SubAgent session 的 transcript 中 AI 消息是否以 `BaseMessage::Ai` 存储

### 已排除的假说

- **ACP caps 导致事件丢失**：caps 修复后，`ToolEnd` 事件已正常发送（`source_agent_id = None` 时走 `session/update` 标准通道，不受 caps 影响）
- **TUI 端 ToolCard 渲染逻辑**：Agent 不在 `COLLAPSED_BY_DEFAULT`/`AUTO_EXPAND`/`FORCE_EXPAND_ON_COMPLETE` 中，默认展开；`render_tool_card_lines` 的 `output_summary.is_empty()` 检查也正常——问题在 output_summary **内容为**空
- **`format_subagent_result` 的 `tool_calls` 参数**：因为 `tool_calls: Vec::new()`，走 `output.text.clone()` 路径，所以最终字符串由 `extract_last_ai_text` 决定

## 失败的修复尝试

尝试通过 `count_tool_calls_from_transcript` 从 session transcript 统计真实 tool calls，并将其注入 `AgentOutput`。此方法被取消，原因：

1. **方向错误**：真正的问题是 `final_text`（即 `extract_last_ai_text` 的返回值）为空，而非缺少工具统计信息。修复工具统计只是掩盖问题，不解决根因
2. **API 不匹配**：`AgentOutput.tool_calls` 字段是 `Vec<(ToolCallInfo, String)>`——包含 `input`/`output` 对，而 `count_tool_calls_from_transcript` 只能填入虚拟 name，缺少真实的 input/output 数据
3. **复杂度高**：从 transcript 反查 tool calls 需要遍历所有消息、匹配 `tool_call_id`、构造 `ToolCallInfo`，引入不必要的复杂度

已回退所有相关更改（`define.rs`、`execute_fork.rs`、`mod.rs`、`acp_events.rs`、`acp_notifier.rs`）。

## 下一步

需要添加诊断日志确认 `extract_last_ai_text` 在 V2 SubAgent session transcript 上的实际行为，重点验证：
- SubAgent 正常完成后，transcript 中是否存在 `BaseMessage::Ai` 消息
- `final_text` 的值是否非空
