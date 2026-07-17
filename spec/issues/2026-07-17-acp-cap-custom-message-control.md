# ACP Cap 管控自定义信息传递

**状态**：Open
**优先级**：中
**创建日期**：2026-07-17

## 问题描述

目前在 ACP 协议交互中，agent 向 TUI 传递了大量自定义界面信息（token 统计、skill 列表、replay 标记、子 agent 路由等），全部通过 `_meta` 或 `_peri` 扩展字段无差别发送。TUI 侧 `InitializeRequest` 中的 `client_capabilities` 被完全忽略（`_req` 前缀），agent 侧 `build_initialize_response` 声明的 caps 也不完整。缺少基于 cap 的开关机制来控制这些自定义数据的传递。

## 症状详情

### 现象 1：自定义数据无差别传递

以下 7 类自定义数据当前无条件发送，无论 TUI 是否需要：

| # | 数据键 | 事件 | 用途 |
|---|--------|------|------|
| 1 | `UsageUpdate._meta.{inputTokens, outputTokens, cacheReadTokens, requestId, model, stopReason}` | `usage_update` | Token 统计、缓存命中率警告 |
| 2 | `AvailableCommandsUpdate._meta.skillNames` | `available_commands_update` | 斜杠命令 skill 列表 |
| 3 | `ContentChunk._meta.periReplay` | `agent_message_chunk` / `user_message_chunk` / `agent_thought_chunk` | Session replay 检测 |
| 4 | `ToolCall._meta.periReplay` | `tool_call` | Replay 工具卡片 |
| 5 | `ToolCallUpdate._meta.periReplay` | `tool_call_update` | Replay 工具完成 |
| 6 | `params._peri.sourceAgentId` | 全部流式事件 | 子 Agent 输出路由 |
| 7 | `AcpEvent::StateSnapshotMeta.{budget_pct, context_total_tokens}` | `peri/agent_event` | 上下文使用率状态栏 |

### 现象 2：ClientCapabilities 被忽略

- **Stdio 路径** `peri-tui/src/acp_stdio/transport.rs:14`：参数名 `_req: InitializeRequest`（`_` 前缀=未使用）
- **MpscTransport 路径** `peri-tui/src/acp_server/requests.rs:44-53`：只读 `protocolVersion`，不读 `client_capabilities`

### 现象 3：AgentCapabilities 声明不完整

当前 `build_initialize_response()`（`peri-acp/src/dispatch/init.rs:17-29`）只声明了：
- `load_session(true)`
- `prompt_capabilities`（空，未声明 image/audio/embedded_context）
- `session_capabilities`（list, close, resume, fork）

**缺失**：
- `mcp_capabilities`
- `auth`
- `providers`（unstable_llm_providers）
- `nes`（unstable_nes）
- `session_capabilities.delete`
- `session_capabilities.additional_directories`

## 期望改进方向

为每类自定义信息传递定义对应的 ACP cap，agent 侧在发送前检查 TUI 的 `ClientCapabilities`（或通过 `_meta` 扩展字段声明的自定义 cap），未声明则不发送对应数据。TUI 侧可以根据自身需求选择声明哪些 cap。

## 涉及文件

- `peri-acp/src/dispatch/init.rs`（17 行）—— `build_initialize_response()`，当前 cap 声明入口
- `peri-tui/src/acp_server/requests.rs`（~600 行）—— MpscTransport `initialize` 处理，需读取 client_capabilities
- `peri-tui/src/acp_stdio/transport.rs`（35 行）—— Stdio `initialize` 处理，需读取 client_capabilities
- `peri-acp/src/event/mapper.rs`（~400 行）—— `UsageUpdate` `_meta` 构造，需加 cap 检查
- `peri-tui/src/acp_server/notify.rs`（~200 行）—— `AvailableCommandsUpdate` `skillNames` 构造
- `peri-tui/src/acp_stdio/commands.rs`（~50 行）—— 同上，Stdio 路径
- `peri-acp/src/dispatch/session_replay.rs`（~200 行）—— `periReplay` `_meta` 构造
- `peri-acp/src/session/event_sink.rs`（~300 行）—— `_peri.sourceAgentId` 注入
- `peri-tui/src/kit/acp_notifier.rs`（~600 行）—— 所有 `_meta` 消费端

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-17 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
