# ACP 协议大重构：废弃冗余自定义事件，复用标准 `session/update`

**状态**：Open
**Triage**：ready-for-agent
**优先级**：高
**创建日期**：2026-07-07

---

## Problem Statement

当前 `peri/unstable-event` 通道中存在大量与标准 ACP `session/update` 重复的自定义事件。全量审计后确定废弃 11 个，保留 13 个。

原则：**标准有的走标准，真没有的才自定义。**

详见 `docs/design/decisions/2026-07-07-acp-reuse-first.md`。

---

## 任务 A：§4.1 流式事件 → 复用标准 `session/update`

废弃四个自定义事件，TUI 改为消费标准 `session/update` 的对应 tag。

| 废弃的自定义事件 | 应使用的 ACP 标准 | `SessionUpdate` tag |
|---|---|---|
| `text-chunk` | 流式文本 | `agent_message_chunk` |
| `reasoning-chunk` | 推理文本 | `agent_thought_chunk` |
| `tool-started` | 工具调用开始 | `tool_call` |
| `tool-ended` | 工具调用结束 | `tool_call_update` |

### 数据流

```
当前（双重，浪费）:
  session/update → agent_message_chunk  → TUI 消费
  uni/unstable-event → text-chunk       → TUI 再消费一次  ← 多余

改为:
  session/update → agent_message_chunk  → TUI 消费（唯一路径）
```

### ACP 层

| 文件 | 改动 |
|------|------|
| `peri-acp/src/event/router.rs` | 删 §4.1 四个事件映射分支 |
| `peri-acp-types/src/event_data.rs` | 删 `TextChunk` / `ReasoningChunk` / `ToolStarted` / `ToolEnded` 结构体及测试 |
| `docs/design/peri-acp-protocol.md` | 更新 §4.1，标注已废弃 |
| `docs/ACP_COMPATIBLE.csv` | 更新 |

### TUI 层

| 文件 | 改动 |
|------|------|
| `peri-tui/src/kit/acp_types.rs` | 删 `AcpEventData::TextChunk` / `ReasoningChunk` / `ToolStarted` / `ToolEnded` 解码分支 |
| `peri-tui/src/kit/acp_bridge.rs` | 确认流式增量构建仅走 `session/update` 路径 |

---

## 任务 B：`view-commit` 废弃 → `session/update` 增量 + `turn-done` 边界

`view-commit` 每次传输全量 `ViewModel[]`，改为标准增量模式。

### 数据流

```
流式阶段（增量，标准 ACP）:
  session/update → agent_message_chunk    → TUI 增量构建 current_turn
  session/update → agent_thought_chunk    → TUI 增量构建 current_turn
  session/update → tool_call              → TUI 增量构建 current_turn
  session/update → tool_call_update       → TUI 更新对应 tool card

边界信号（轻量，自定义事件）:
  uni/unstable-event → turn-done          → TUI 归档 current_turn → committed  ← 仅传 {}
  uni/unstable-event → turn-interrupted   → TUI 丢弃 current_turn
```

### 边界场景

| 场景 | 处理方式 |
|------|----------|
| **session/load** | `session/replay` 通过 `session/update` → `user_message_chunk` / `agent_message_chunk` 逐条重放历史 |
| **compact 后** | 重放 compact 后的消息列表，增量 |
| **rewind 后** | 重放回退后的消息，增量 |

### ACP 层

| 文件 | 改动 |
|------|------|
| `peri-acp/src/event/router.rs` | 删 `TurnCommitted → view-commit` 分支 |
| `peri-acp/src/event/view_mapper.rs` | 删除整个文件（仅为 `view-commit` 服务） |
| `peri-acp/src/event/mod.rs` | 删 `pub mod view_mapper` 和 `pub use ViewMapperImpl` |
| `peri-acp/src/session/event_sink.rs` | `push_event` 不再发 `view-commit`；`TransportEventSink` 删除 `view_mapper` 字段 |
| `peri-acp-types/src/event_data.rs` | 删 `ViewCommit` 结构体及测试 |
| `peri-acp/src/dispatch/session_load.rs` | 不再构建 `view-commit` payload |
| `peri-acp/src/dispatch/session_replay.rs` | 验证 replay 流程正常覆盖首屏 |
| `docs/design/peri-acp-protocol.md` | 更新 §4.2，移除 `view-commit` |
| `docs/ACP_COMPATIBLE.csv` | 移除 `view-commit` 相关行 |

### TUI 层

| 文件 | 改动 |
|------|------|
| `peri-tui/src/kit/acp_types.rs` | 删 `AcpEventData::ViewCommit` 解码分支 |
| `peri-tui/src/kit/acp_bridge.rs` | `turn-done` 时归档 `current_turn` → `committed`；不再等 `view-commit` |
| `peri-tui/src/kit/render_bridge.rs` | 不变——committed + current_turn 来源不变，只是写入时机从 `view-commit` 变为 `turn-done` |
| `peri-tui/src/acp_server/requests.rs` | 删 `view-commit` 发送逻辑 |

---

## 任务 C：`token-usage` 废弃 → 标准 `usage_update` meta

标准 `usage_update` 的 meta 字段已含 `inputTokens` / `outputTokens` / `model` / `cacheTokens`，与自定义 `token-usage` 完全重复。

### ACP 层

| 文件 | 改动 |
|------|------|
| `peri-acp/src/event/router.rs` | 删 `LlmCallEnd { usage: Some } → token-usage` 分支 |
| `peri-acp-types/src/event_data.rs` | 删 `TokenUsage` 结构体及测试 |

### TUI 层

| 文件 | 改动 |
|------|------|
| `peri-tui/src/kit/acp_types.rs` | 删 `AcpEventData::TokenUsage` 解码分支 |
| `peri-tui/src/kit/acp_bridge.rs` | 状态栏 token 从 `usage_update` meta 读取 |

---

## 任务 D：`hitl-pending` / `ask-user` 从事件目录移除

这两个从未作为 `peri/unstable-event` 产出——router 明确跳过，实际走 broker JSON-RPC（`session/request_permission` / `elicitation/create`）。

### 清理

| 文件 | 改动 |
|------|------|
| `peri-acp-types/src/event_data.rs` | 删 `HitlPending` / `ToolApproval` / `AskUser` / `Question` / `QuestionOption` 结构体 |
| `peri-tui/src/kit/acp_types.rs` | 删 `AcpEventData::HitlPending` / `AskUser` 解码分支 |
| `docs/design/peri-acp-protocol.md` | §4.5 移除 `hitl-pending` / `ask-user` |

---

## 验证清单

- [ ] A1：无 `text-chunk` / `reasoning-chunk` / `tool-started` / `tool-ended` 残留引用（grep 全仓）
- [ ] A2：流式文本正常显示（走 `session/update` → `agent_message_chunk`）
- [ ] A3：推理文本正常显示（走 `session/update` → `agent_thought_chunk`）
- [ ] A4：工具卡片正常显示（走 `session/update` → `tool_call` / `tool_call_update`）
- [ ] B1：无 `view-commit` 残留引用（grep 全仓）
- [ ] B2：多轮对话 streaming 正常 → `turn-done` 归档 → committed 正确
- [ ] B3：`session/load` 历史消息通过 replay 正确渲染
- [ ] B4：compact 后消息列表正确重放
- [ ] B5：rewind 后消息正确
- [ ] B6：SubAgent 流式文本（含 agent_id）正确路由到 SubAgentGroup
- [ ] B7：事件数量回归——不再有双重推送
- [ ] C1：无 `token-usage` 残留引用；状态栏 token 从 `usage_update` meta 读取正常
- [ ] D1：无 `hitl-pending` / `ask-user` 残留引用；HITL/AskUser 功能不受影响
- [ ] E1：`cargo build` 全仓通过
- [ ] E2：`cargo test` 全仓通过

## 执行顺序

A → C → D（纯删除/映射切换）→ B（依赖 A 的 `session/update` 增量路径已确认稳定）
