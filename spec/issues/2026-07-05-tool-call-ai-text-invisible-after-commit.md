# 消息流渲染中，AI 消息文本在多分支渲染路径下不可见

**状态**：Open
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

## 复现条件

- **复现频率**：必现（任何涉及工具调用的对话）
- **触发步骤**：
  1. 启动 Peri TUI
  2. 发起一个会触发工具调用的对话
  3. 观察流式输出期间——AI 文字可见
  4. 流式结束后——发起工具调用的 AI 文本消失；工具完成后的最终 AI 回复完全不出现
- **环境**：任意模型，macOS

## 涉及文件

| 文件 | 说明 |
|------|------|
| `peri-acp/src/event/view_mapper.rs` | ACP 层 BaseMessage→ViewModel 转换（`convert_ai` 将所有 ContentBlock::Text 拼接） |
| `peri-tui/src/kit/acp_types.rs` | 流式路径 `CurrentTurn.build_view_models()` 构建增量 ViewModels |
| `peri-tui/src/kit/acp_events.rs` | `ViewCommit` / `TurnDone` 处理，多分支 committed 合并逻辑 |
| `peri-tui/src/kit/view_render.rs` | AssistantBubble 渲染（`render_assistant_bubble`） |
| `peri-acp-types/src/view_model.rs` | `AssistantBubbleData` 只持有一段连续 `text`，不支持交错布局 |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-05 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
