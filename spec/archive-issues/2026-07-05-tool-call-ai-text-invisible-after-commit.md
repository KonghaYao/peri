> 归档于 2026-07-10，原路径 spec/issues/2026-07-05-tool-call-ai-text-invisible-after-commit.md

# 消息流渲染中，AI 消息文本在多分支渲染路径下不可见

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-05

## 问题描述

在 TUI 消息流渲染中，当 LLM 回复包含文本和工具调用时，存在两个关联的渲染缺失问题：(1) 发起工具调用的 AI 消息文本在提交后消失，仅剩 ToolCard；(2) 工具调用全部完成后的最终 AI 回复（总结文字）也没有被渲染出来。怀疑渲染管线中存在多分支路径导致某些 Ai 消息被遗漏。

## 症状详情

### 症状 1：发起工具调用的 AI 消息文本丢失

| 阶段 | 期望行为 | 实际行为 |
|------|----------|----------|
| 流式渲染中 | AI 文本和 ToolCard 交错可见 | 正常，所有文本可见 |
| 流式结束后 | AI 文本保持可见 | AI 消息文本消失，只看到 ToolCard |

- **受影响的内容**：发起工具调用的 Ai 消息自身的文本（如"我来读取文件……"这类说明文字）
- **复现频率**：必现

### 症状 2：工具全部完成后的最终 AI 回复不可见

当所有工具执行完毕后，LLM 会生成一段总结/回复文本（纯文本 Ai 消息，无 tool_calls）。这段消息在 TUI 中完全没有被渲染出来。

- **受影响的内容**：工具链完成后、无 tool_calls 的纯文本 Ai 消息
- **复现频率**：必现

### 典型消息序列

```
Human: "帮我搜索 foo 并修改"
  ↓ 
Ai: "让我先搜索一下……" [带工具调用 Grep, Read]    ← 症状1：文本消失
  ↓
Tool: Grep result
  ↓  
Tool: Read result
  ↓
Ai: "找到了，在 bar.rs 中，我来修改它" [带工具 Edit]  ← 症状1：文本消失
  ↓
Tool: Edit result
  ↓
Ai: "修改完成，foo 函数已更新到 v2"               ← 症状2：整条消息不渲染
```

- **影响范围**：所有 LLM 模型（OpenAI / Anthropic）
- **环境**：macOS

### 现象 3（2026-07-07）追加确认

同日二次确认：当一条 Ai message 同时包含 content 文本和 tool call 时，渲染结果中 AI 消息文本不可见，界面上只渲染了 ToolCard。用户描述："如果同时一个 ai message 包含 content 和 tool call，渲染好像没有了 ai message"。

这与症状 1 描述一致——流式期间文字可见（由 `current_turn` 路径渲染），流式结束（`ViewCommit` 写入 committed）后文字消失。

## 复现条件

- **复现频率**：必现（任何涉及工具调用的对话）
- **触发步骤**：
  1. 启动 Peri TUI
  2. 发起一个会触发工具调用的对话
  3. 观察流式输出期间——AI 文字可见
  4. 流式结束后——发起工具调用的 AI 文本消失；工具完成后的最终 AI 回复完全不出现
- **环境**：任意模型，macOS

## 根因分析

**定位**：`peri-agent/src/llm/openai/stream.rs:253-275`，`build_stream_response` 函数

在流式 `ToolUse` 分支中，流式期间累积的文本 `content_text` 从未被推入 `blocks` 数组。对比非流式路径 `invoke.rs:252-254` 正确地在添加 ToolUse blocks 之前先将文本推入 blocks。

```rust
// stream.rs:253（Bug：content_text 丢失）
if stop_reason == StopReason::ToolUse {
    for tc in &tool_call_requests {
        blocks.push(ContentBlock::tool_use(...));  // 只推了 ToolUse blocks
    }
    // ← content_text 从未被加入 blocks
}

// invoke.rs:252-254（正确：文本先推入）
if !content_str.is_empty() {
    blocks.push(ContentBlock::text(&content_str));
}
```

**影响范围**：仅影响 OpenAI 兼容 API (ChatOpenAI) 的流式路径。Anthropic 的流式路径 (`anthropic/stream.rs`) 通过 `parse_content_blocks` 正确处理文本块，不受影响。

**症状解释**：
- 流式期间：`TextChunk` 事件 → `current_turn.text` 累积 → TUI 渲染可见 ✓
- ViewCommit 后：`build_stream_response` 产生的 BaseMessage 不含 Text block → `view_mapper.convert_ai` 产生 `AssistantBubble(text="")` → TUI 只显示 ToolCard ✗

## 涉及文件

| 文件 | 说明 |
|------|------|
| `peri-agent/src/llm/openai/stream.rs` | **🔴 根因**：`build_stream_response` ToolUse 分支遗漏 `content_text` |
| `peri-acp/src/event/view_mapper.rs` | ACP 层 BaseMessage→ViewModel 转换（非 bug，正确消费已存在的内容） |
| `peri-tui/src/kit/acp_types.rs` | 流式路径 `CurrentTurn.build_view_models()` 构建增量 ViewModels |
| `peri-tui/src/kit/acp_events.rs` | `ViewCommit` / `TurnDone` 处理 |
| `peri-acp-types/src/view_model.rs` | `AssistantBubbleData` DTO 定义 |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-05 | — | Open | agent | 创建 |
| 2026-07-07 | Open | Open | agent | 追加现象 3——用户二次确认症状 1（ai message 含 content+tool call 时文本不可见） |
| 2026-07-07 | Open | Fixed | agent | 定位根因：stream.rs build_stream_response ToolUse 分支遗漏 content_text，修复并添加单元测试 |

## 修复记录

### 修复 #1（2026-07-07）

- **操作人**：agent (deepseek-v4-pro)
- **用户原意**：修复 AI 消息同时包含文本和工具调用时文本在 ViewCommit 后消失的问题
- **修复内容**：
  1. `peri-agent/src/llm/openai/stream.rs`：在 `build_stream_response` 的 ToolUse 分支中添加 `content_text` 到 `blocks` 数组（与非流式路径 `invoke.rs` 行为对齐）
  2. `peri-agent/src/llm/openai/stream.rs`（内联 `#[cfg(test)] mod tests`）：添加 3 个单元测试
- **验证状态**：待验证
