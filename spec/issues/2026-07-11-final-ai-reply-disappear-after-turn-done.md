# 工具调用吞掉前面 AI 消息文本的显示

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-11

## 问题描述

当 AI 消息同时包含文本和工具调用时（例如 "让我搜索一下" + tool_calls: Grep），该 AI 消息的文本部分在 TUI 中不显示——界面上只渲染了 ToolCard，前面的文本消失了。Reasoning block 正常渲染。

## 症状详情

### 典型消息序列

```
Human: "帮我搜索 foo 并修改"
  ↓
Ai: "让我搜索一下" [带 tool_calls: Grep]    ← 文本消失，只看到 ToolCard
  ↓
Tool: Grep result
  ↓
Ai: "找到了，读取内容" [带 tool_calls: Read] ← 同样：文本消失，只看到 ToolCard
  ↓
...
```

### 观察对比

| 元素 | 渲染状态 | 备注 |
|------|----------|------|
| 引发工具调用的 AI 消息文本 | ❌ 不显示 | 如 "让我搜索一下" / "找到了，读取内容" 等说明文字 |
| ToolCard | ✅ 正常 |
| Reasoning block | ✅ 正常 |

- **影响范围**：所有同时包含文本和工具调用的 AI 消息
- **定位**：纯前端渲染层问题（TUI bridge / build_view_models / 事件处理）

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动 Peri TUI
  2. 发起会触发工具调用的对话
  3. 观察到 AI 文本不显示，只有 ToolCard
- **环境**：macOS

## 涉及文件

| 文件 | 说明 |
|------|------|
| `peri-tui/src/kit/acp_types.rs` | `CurrentTurn.build_view_models()` 构建增量 ViewModels，负责按 segment 交错产出文本气泡和工具卡片 |
| `peri-tui/src/kit/acp_events.rs` | `TextChunk` / `ToolStarted` 事件处理，`TurnDone` 归档逻辑 |
| `peri-tui/src/kit/acp_bridge.rs` | BridgeState 管理：`dispatch_and_notify` 流程入口 |
| `peri-tui/src/kit/acp_notifier.rs` | ACP `session/update` → `AcpEventData` 解码（文本走 `TextChunk`，工具走 `ToolStarted`） |

## 关联 Issue

- `spec/archive-issues/2026-07-05-tool-call-ai-text-invisible-after-commit.md` —— 同症状（症状 1 + 现象 3），根因为 LLM 适配层 `openai/stream.rs` 中 `build_stream_response` 的 `content_text` 遗漏。该 issue 已 Fixed。本 issue 为**纯前端渲染层**问题——LLM 适配层修复后，TUI 渲染层仍有同症状的 bug。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-11 | — | Open | agent | 创建；2026-07-11 纠正症状描述——不是"最终回复消失"，而是"工具调用吞掉前面文本" |
| 2026-07-11 | Open | Fixed | agent | 修复提交 commit 93e44c92；根因在 render_bridge.rs HASH_DIFF_APPEND 截断逻辑 |

## 修复记录

### 修复 #1（2026-07-11）

- **操作人**：agent
- **用户原意**：工具调用前 AI 消息的文本部分（如"让我搜索一下"）应在 TUI 中正常显示，与 ToolCard 同时可见
- **修复内容**：`peri-tui/src/kit/render_bridge.rs` 中 `HASH_DIFF_APPEND` 策略的截断逻辑从按 entry 数量截断改为按 `VmKey::Item(idx)` 边界截断。一个 assistant bubble（含 reasoning+text）可产出 2+ 个 RenderedEntry，按 entry 数量截断会丢弃同一 item 内的后续 entry（如 text），导致界面只渲染 reasoning 和 ToolCard，文本消失。
- **涉及 commit**：`93e44c92`（分支 refactor/md）
- **涉及文件**：`peri-tui/src/kit/render_bridge.rs`
- **验证状态**：待验证
