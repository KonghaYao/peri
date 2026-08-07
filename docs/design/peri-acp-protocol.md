# peri-acp 协议设计

> 设计起点：2026-07-15（v2.1 修订） | 最后核对：2026-08-07

## 1. 协议分层

ACP 协议分为标准 ACP 方法（TUI → 服务）与 ACP 事件通知（服务 → TUI）。标准方法承载请求-响应和交互；事件经 ACP 的 v2 映射与协议化面投递给客户端。

**标准 ACP（JSON-RPC 方法）**：TUI 调用，ACP 执行并返回。覆盖会话生命周期、prompt 提交、命令执行、交互应答、面板数据查询。请求-响应语义——发一个请求，收一个结果。

**ACP 事件通知**：当前 TUI 的主事件面是标准 `session/update` 和 `peri/agent_event`。`peri/unstable-event` 只保留兼容或特定扩展用途，不作为通用 Agent → TUI 事件管道；完整链路见 `docs/standards/architecture-contracts.md` 的 ARC-EVENT-001。

这些消息复用传输通道；transport 只负责编解码和分发，不解释 Agent 业务语义。

---

## 2. 标准 ACP 方法（TUI → Agent）

TUI 的所有主动行为通过标准 ACP JSON-RPC 方法调用。不定义自定义事件来替代标准协议已有的操作。

### 2.1 会话生命周期

| 方法 | 参数 | 返回值 | 语义 |
|------|------|--------|------|
| `session/new` | `{ cwd?, model?, permission_mode? }` | `{ session_id }` | 创建新会话 |
| `session/load` | `{ session_id }` | `{ session_id, messages }` | 恢复历史会话 |
| `session/close` | `{ session_id }` | `{}` | 关闭会话 |
| `session/fork` | `{ source_session_id }` | `{ new_session_id }` | 复制当前会话到新线程 |
| `session/list` | `{ cwd? }` | `{ sessions: SessionInfo[] }` | 列出会话（可按 cwd 过滤） |

### 2.2 交互

| 方法 | 参数 | 返回值 | 语义 |
|------|------|--------|------|
| `session/prompt` | `{ sessionId, message: { role: "user", content }, attachments?, bgResults?, requestId? }` | `{}` | 提交用户输入（notification，sessionId 同时支持 `session_id` 别名；`requestId` 为可选的本轮 turn 标识，服务器随 `peri/agent_event_done` 回带，供 TUI stale 事件配对） |
| `session/cancel` | `{ sessionId }` | — | 中断当前 Agent（notification，非 request-response） |
| `session/execute-command` | `{ session_id, command, args }` | `{}` | 执行 Slash 命令（HITL 审批和 AskUser 回答均走此方法） |

### 2.3 查询与控制

| 方法 | 参数 | 返回值 | 语义 |
|------|------|--------|------|
| `session/switch-model` | `{ sessionId, modelId }` | `{}` | 切换模型（实际方法名 `session/set_model`） |
| `session/switch-provider` | `{ session_id, provider_id }` | `{}` | 切换 Provider |
| `session/set_config_option` | `{ sessionId?, configId, value }` | `{}` | 更新配置项（有 session 时用 request，无 session 时用 `session/config_update` notification） |

> **已移除方法**：`session/approve` 和 `session/answer` 已废弃——HITL 审批走 `HITL_RESPONSE_TX → session/execute-command`，AskUser 回答走 `ASK_USER_TX → session/execute-command`。`session/query` 和 `session/suggest-files` 未实现——文件补全走本地 `FILE_LIST` atom + `SkimMatcher`。

### 2.4 插件管理

| 方法 | 参数 | 返回值 | 语义 |
|------|------|--------|------|
| `plugin/search` | `{ query, sessionId? }` | `{}` | 搜索插件市场 |
| `plugin/install` | `{ name, sessionId? }` | `{}` | 安装插件 |
| `plugin/uninstall` | `{ name, sessionId? }` | `{}` | 卸载插件 |
| `plugin/toggle` | `{ name, enabled, sessionId? }` | `{}` | 启用/禁用插件 |

---

## 3. ACP 事件通知（服务 → TUI）

Agent 侧 v2 事件经 ACP 的协议序列化与 EventSink 映射后发送给客户端。新增或变更事件必须同时覆盖发射、ACP 映射、能力门控（如适用）和 TUI 消费；以 `docs/standards/architecture-contracts.md` 的 ARC-EVENT-001 为单一事实源。

### 3.1 当前事件面

```
v2 EventBus
    ↓
ACP 事件映射 / EventSink
    ├─ `session/update`：标准流式内容、工具与 usage 更新
    ├─ `peri/agent_event`：TUI 专用状态、结构与扩展事件
    ├─ 标准交互请求：HITL / Elicitation
    └─ `peri/agent_event_done`：turn 终止通知
```

`peri/unstable-event` 不参与上述通用链路；其剩余兼容/扩展用途不应作为新 Agent 事件的目标通道。

### 3.2 通知格式

通知的 wire 结构由 ACP method 决定：

- `session/update` 使用标准 ACP `SessionUpdate` payload，承载流式内容、工具调用与 usage 更新。
- `peri/agent_event` 使用 ACP 的 TUI 专用 DTO，承载低频状态、结构和扩展事件。
- `peri/agent_event_done` 承载本轮终止状态，并可带 `requestId` 关联提交请求。

不要为新 Agent 事件定义平行的 `{event, data}` 协议；需要新增或扩展时，按 ARC-EVENT-001 同步更新事件类型、ACP 映射与 TUI 消费。

### 3.3 设计原则

1. **事件链路单一**：新增事件经 v2 EventBus、ACP 映射和协议化面到达 TUI，不恢复 v2_tx 或其他直连通道。
2. **协议面类型化**：标准更新使用 ACP 类型，TUI 专用通知使用 `AcpEvent` DTO；两者的兼容性由映射层维护。
3. **终止可观测**：每个 terminal 事件必须使客户端离开 loading 状态。
4. **传输无关**：MpscTransport 与 StdioTransport 复用同一协议语义；transport 不解释 Agent 业务逻辑。

---

## 4. 事件目录

### 4.1 流式事件（高频，每秒数十次）

> **已迁移至标准 ACP `session/update` 通道**。以下事件不再走 `peri/unstable-event`，改由 `map_event()` Category ① 映射为标准 ACP `SessionUpdate` 通知（`ContentChunk` / `ToolCall` / `ToolCallUpdate`）。TUI 侧通过 `acp_notifier.rs` 的 `handle_session_update` 处理。

| 原事件名 | SessionUpdate tag | 语义 |
|----------|-------------------|------|
| `"text-chunk"` | `AgentMessageChunk(ContentChunk)` | 追加文本到当前气泡 |
| `"reasoning-chunk"` | `AgentThoughtChunk(ContentChunk)` | 追加推理区域文本 |
| `"tool-started"` | `ToolCall(ToolCall)` | 创建执行中的工具卡片 |
| `"tool-ended"` | `ToolCallUpdate(ToolCallUpdate)` | 填充工具卡片结果 |

`sourceAgentId` 通过 `params._peri.sourceAgentId` 扩展字段传递——有值时表示此事件属于子 Agent。

### 4.2 边界与状态事件

| 事件 | 当前传递方式 | 语义 |
|------|--------------|------|
| turn 终止 | `peri/agent_event_done` | 客户端离开 loading 状态；可携带 `requestId` 用于关联 |
| `TurnSuspended` | `peri/agent_event` | Agent 等待后台任务、cron 或 workflow 时更新 TUI 状态 |
| rewind | `peri/agent_event` + `session/rewind*` RPC | 候选、预览与执行结果由 RPC 和事件共同驱动 |
| 上下文使用量 | `session/update` 的 usage 更新与状态快照 | 当前没有单独的用户可见 `budget-warning` 生产路径 |
| compact、subagent、后台任务、workflow | `peri/agent_event` | 由 TUI 专用 DTO 消费 |

HITL 与 AskUser 通过标准交互协议（`UserInteractionBroker`、`RequestPermission`、`Elicitation`）往返；它们不是通用事件目录的一部分。

### 4.3 兼容与扩展事件

`peri/unstable-event` 不再承载 v2 Agent 事件映射。它仅保留给兼容或特定扩展用途；新增 Agent 事件必须经 ARC-EVENT-001 规定的 `session/update` 或 `peri/agent_event` 链路接入。

---

## 5. 事件映射职责

事件映射收敛在 `peri-acp/src/event/` 的 EventSink 与协议化层：负责把 Agent v2 事件转换为标准更新、TUI 专用 DTO 或终止通知。transport 的 router 只负责消息编解码与分发，不应作为 Agent 事件映射事实源。

---

## 6. 传输层

标准 ACP 方法和自定义事件共享同一传输通道。传输层根据 `method` 字段分流——不同 method name 对应不同语义通道。

### 6.1 传输通道

| 通道 | method | 格式 | 语义 |
|------|--------|------|------|
| 标准 ACP JSON-RPC | `session/new`、`session/prompt` 等 | `{method, params, id?}` | TUI → 服务请求或 notification |
| 标准 ACP notification | `session/update` | `{method: "session/update", params: {sessionId, update}}` | 服务 → 客户端流式与使用量更新 |
| TUI 专用 DTO | `peri/agent_event` | `{method: "peri/agent_event", params: {sessionId, event_json}}` | 服务 → TUI 低频状态、结构与扩展事件 |
| 标准交互 | `session/request_permission`、`elicitation/create` | ACP method/response | HITL 与 AskUser 往返 |
| 兼容/扩展 | `peri/unstable-event` | 兼容或特定扩展 payload | 不作为新 Agent 事件的默认通道 |
| Turn 结束信号 | `peri/agent_event_done` | `{method: "peri/agent_event_done", params: {sessionId, stop_reason}}` | 服务 → 客户端 turn 完成 |

### 6.2 传输实现

- **开发环境（TUI 内嵌 ACP）**：`MpscTransport`。同一进程内通过 tokio mpsc 通道传递消息。
- **生产环境（IDE 插件、远程代理）**：`StdioTransport`。stdin/stdout 传递 JSON 消息。

传输层职责限于消息搬动——不做事件过滤、不做事物流、不做重试。事件协议化与通道选择由 `peri-acp/src/event/` 的 EventSink 和映射层决定。

---

## 7. 兼容性

`peri/unstable-event` 的遗留兼容 payload 可能随协议演进变化；新事件应优先使用 `session/update` 或 `peri/agent_event`。修改既有 wire payload 时，必须同步更新 ACP 与 TUI 两侧解析，并按 ARC-EVENT-001 验证完整事件链路。
