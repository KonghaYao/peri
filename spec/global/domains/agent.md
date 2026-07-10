# Agent / ReAct 循环领域

## 领域综述

Peri Agent 的 ReAct 循环、LLM 适配器、工具系统、Context 管理、SubAgent 构建。核心数据流为消息构建 → ReAct 循环（Compact → Receive → Reason → Act → End）→ 工具分发 → 响应生成。Provider 适配层统一 OpenAI/Anthropic 流式与非流式路径。

## 核心流程

- **ReAct 循环**：每轮 `before_agent → loop(500) { before_model → LLM → after_model → [工具分发] | [回答] } → End`
- **消息构建**：`BaseMessage`（Human/Ai/System/Tool），`ContentBlock`（Text/Image/Document/ToolUse/ToolResult/Reasoning）
- **Provider 适配**：`invoke.rs`（非流式）和 `stream.rs`（流式）双路径，需产出相同的 `BaseMessage` 序列
- **SubAgent 构建**：fork/non-fork 双模式，共享 `build_v2_subagent_context` 入口，`parent_messages` 注入 transcript

## 技术方案总结

| 维度 | 选型 |
|------|------|
| 循环引擎 | ReAct Loop（`run_react_loop`），max 500 iterations |
| 消息类型 | `BaseMessage` + `ContentBlock` 枚举（ACP 协议对齐） |
| LLM Provider | OpenAI chat completions + Anthropic Messages API，统一适配层 |
| SubAgent | fork 模式（隔离构建）+ inline 模式（共享上下文），v2_bridge 统一入口 |
| 工具分发 | 3 阶段分发（Core/Meta/Deferred），`tool_dispatch.rs` 延迟消息写入 |

---

## Issue 经验附录

### issue_2026-07-07-fork-subagent-no-parent-conversation-history
**摘要:** Fork 模式 SubAgent 收不到父对话历史
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** fork SubAgent, parent_messages, 对话历史, v2_bridge
**问题本质:** fork SubAgent 的 `parent_messages` 在 `v2_bridge.rs` 的 transcript 注入点未被正确传递，导致 fork SubAgent 只能看到 fork directive + prompt，看不到父对话历史。
**通用模式:** SubAgent 的 transcript 构建是三层叠加（frozen prompt + parent_messages + task prompt），每个注入点必须逐层验证：fork 路径和 non-fork 路径在 `parent_messages` 注入处是否走同一代码路径。
**涉及文件:** peri-middlewares/src/subagent/tool/execute_fork.rs, peri-middlewares/src/subagent/v2_bridge.rs, peri-middlewares/src/subagent/fork.rs
**CLAUDE.md 链接:** false

### issue_2026-07-05-tool-call-ai-text-invisible-after-commit
**摘要:** 消息流渲染中，AI 消息文本在多分支渲染路径下不可见
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** content_text 丢失, stream.rs ToolUse 分支, ViewCommit, OpenAI 流式
**问题本质:** `build_stream_response` 在 ToolUse 分支中，流式期间累积的 `content_text` 从未被推入 `blocks` 数组。流式路径正确（`TextChunk` → `current_turn` 可见），但 ViewCommit 后 `BaseMessage` 不含 Text block → `AssistantBubble(text="")` → TUI 只显示 ToolCard。非流式路径（`invoke.rs`）正确，仅 OpenAI 流式路径受影响。
**通用模式:** 流式路径和非流式路径在构建最终 `BaseMessage` 时必须产出相同的 `ContentBlock` 序列——这是协议层的一致性约束。新增 ContentBlock 变体时需检查所有 provider 的流式/非流式路径是否都正确填充。
**技术决策:** 修复不仅添加了 `content_text` 到 blocks，还新增了 3 个单元测试防止回归——这类"看起来很简单但影响很大的 bug"最适合用测试固化。
**涉及文件:** peri-agent/src/llm/openai/stream.rs
**CLAUDE.md 链接:** false

### issue_2026-07-09-bg-agent-loading-never-stops-after-first-turn
**摘要:** Agent 工具（background 模式）启动 bg agent 后 loading 不停止
**状态:** Fixed
**归档日期:** 2026-07-10
**关键词:** background agent, loading 生命周期, TurnDone, ReAct 循环
**问题本质:** AI 调用 Agent 工具（background 模式）后本轮 turn 正常结束（TurnDone），但 bg callback 触发的 `SyntheticUserMessage` 在 agent End 阶段 emit，导致连续 loading 周期——首轮 TurnDone 和 bg callback 的 TurnStart 之间没有 loading=false 间隙。与双通道 flush-then-push 修复关联。
**通用模式:** ReAct 循环中 background agent 的 loading 生命周期需要显式的 TurnDone→TurnStart 过渡：background agent 启动后当前 turn 应正常 TurnDone（loading 停止），callback 唤醒时创建新的 TurnStart（loading 重新开始）。二者必须独立发送，不能合并为连续 loading。
**涉及文件:** peri-agent/src/agent/stages/mod.rs, peri-tui/src/kit/acp_bridge.rs, peri-tui/src/kit/acp_events.rs
**CLAUDE.md 链接:** false
