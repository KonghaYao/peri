# peri-acp 协议设计

> 日期：2026-07-15（v2.1 修订）

## 1. 协议分层

ACP 协议分为两层——标准 ACP 方法处理 TUI → Agent 的请求-响应模式，自定义事件处理 Agent → TUI 的推送模式。有标准走标准，标准不覆盖的走自定义事件。

**标准 ACP（JSON-RPC 方法）**：TUI 调用，ACP 执行并返回。覆盖会话生命周期、prompt 提交、命令执行、交互应答、面板数据查询。请求-响应语义——发一个请求，收一个结果。

**自定义事件（`peri/unstable-event`）**：Agent 产出后 ACP 推送到 TUI。覆盖流式输出、状态更新、输入辅助、交互请求。推送语义——Agent 侧发起，TUI 被动接收。

两层在实现上共享同一传输通道——标准方法走 JSON-RPC 的 method/params/result 格式，自定义事件走 `{event, data}` 格式。传输层在收到消息时根据格式自动分流：有 `method` 字段走标准 RPC，有 `event` 字段走自定义事件。

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

## 3. 自定义事件（Agent → TUI）

Agent 产出的事件经 ACP 事件映射器（`mapper.rs`）四路分路后推入 TUI。事件传输架构如下：

### 3.1 事件传输架构（四路分路）

```
ExecutorEvent
    ↓
map_event() 四路分路
    ├─ ① session/update（标准 ACP）     TextChunk/AiReasoning/ToolStart/ToolEnd
    │                                    TodoUpdate/LlmCallEnd(usage)/MessageAdded
    ├─ ② peri/hitl_pending（HITL 审批）   预留，当前 HITL 走 UserInteractionBroker
    ├─ ③ peri/agent_event（TUI 专用）    StateSnapshot/TurnCommitted/Subagent*/
    │                                    Compact*/RewindCompleted/BackgroundTask*/
    │                                    LspDiagnostics/AgentExecutionFailed/ContextWarning
    │                                    LlmRetrying/WorkflowProgress
    └─ ④ peri/observable（观测层）       预留，无外部订阅者
```

EventSink 在 `push_event()` 中依次投递各通道：

1. **`session/update`**：标准 ACP notification，携带 `{sessionId, update}`，IDE/stdio 客户端消费。`_peri` 扩展字段携带 `sourceAgentId`。
2. **`peri/agent_event`**：TUI 专用 notification，携带 `{sessionId, event_json}`，event_json 为 `AcpEvent` DTO 序列化后的 JSON 字符串。
3. **`peri/hitl_pending`**：HITL 审批 notification（预留）。
4. **`peri/unstable-event`**：新协议事件路由，由 `router::route()` 从 ExecutorEvent 映射为 `{event, data}` 格式。仅 3 个事件产出此通道。
5. **`peri/agent_event_done`**：turn 结束信号 notification（`push_done()` 发送），TUI 侧映射为 `AcpNotification::AgentDone → AcpEventData::TurnDone`。payload 为 `{sessionId, stopReason}`，可选扩展字段 `requestId`：TUI 提交 prompt 时生成（`session/prompt` params 携带 `requestId`），服务器随 done 事件原样回带，供 TUI 侧 stale `TurnInterrupted` 的 turn 归属配对判定（Issue 2026-08-05）。缺失路径（continuation / Immediate 命令 / stdio 等）不携带该字段。

> **`_meta` key 序列化**：ACP SDK 的 `ContentChunk`/`ToolCall`/`ToolCallUpdate` 均标注 `#[serde(rename = "_meta")]`，运行时 key 为 `"_meta"`（带下划线）。session replay 检测采用四级 fallback：`_meta → meta → content._meta → content.meta`，取 `periReplay` 布尔值。

### 3.2 消息格式

```json
{
  "event": "<事件名>",
  "data": <事件数据>
}
```

- `event` — kebab-case 字符串，全局唯一。
- `data` — 每个事件名对应一个特定的 JSON 结构。流式事件的 data 小（几十字节），边界事件的 data 大（可能数十 KB）。

### 3.3 设计原则

1. **字符串事件名，非类型化枚举**：事件名是字符串，不在 Rust 类型系统中定义枚举。新增事件只需约定事件名和 data 结构。
2. **消费端各自保证类型安全**：ACP 事件路由器负责 AgentEvent → `{event, data}` 的映射正确性。TUI 状态机负责按事件名解析 data。通道本身不做类型校验。
3. **高频事件轻量，边界事件完整**：流式事件 data 仅携带原始文本片段。边界事件 data 携带完整 ViewModel 列表。
4. **传输无关**：开发环境用 MpscTransport，生产环境可换 StdioTransport。事件格式不变。

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

### 4.2 边界事件（低频）

data 携带完整结构或标志状态切换。跳过节流立即渲染。

| 事件名 | data 结构 | 语义 | 通道 |
|--------|----------|------|------|
| `"turn-done"` | `{}` | Agent 本轮结束，Streaming → Idle | `peri/agent_event_done` |
| `"turn-interrupted"` | `{ reason: string }` | Agent 被中断（用户取消或超时） | `peri/unstable-event`（未实现，router 返回 None） |
| `"turn-suspended"` | `{}` | Agent turn 挂起，等待 bg agent/cron/workflow | `peri/unstable-event` |

> **已移除事件**：`view-commit` 是 TUI 内部概念（由 `TurnCommitted` → `AcpEvent::TurnCommitted` 在 `peri/agent_event` 通道传输），不作为 `peri/unstable-event` 事件产出。

### 4.3 状态事件（更新状态栏，不触发消息区变化）

| 事件名 | data 结构 | 语义 | 通道 |
|--------|----------|------|------|
| `"tool-count"` | `{ count: number }` | 本轮工具调用次数 | `peri/unstable-event`（未实现） |
| `"progress"` | `{ percent: number, label: string }` | 进度百分比 | `peri/unstable-event`（未实现） |
| `"budget-warning"` | `{ used: number, limit: number, threshold: string }` | 上下文预算警告 | `peri/unstable-event` |
| `"system-notification"` | `{ text: string, level: string }` | 系统通知文本 | `peri/unstable-event`（未实现） |

> **已移除事件**：`token-usage` 已废弃——token 用量现通过标准 ACP `session/update` 的 `UsageUpdate` tag 传递（`map_event()` Category ①）。

### 4.4 输入辅助事件

| 事件名 | data 结构 | 语义 |
|--------|----------|------|
| `"prediction"` | `{ text: string }` | 输入预测建议，灰色占位符 |
| `"file-suggestions"` | `{ files: string[] }` | @ 提及文件补全候选 |

### 4.5 交互请求事件（需要用户决策）

| 事件名 | data 结构 | 语义 | 通道 |
|--------|----------|------|------|
| `"hitl-pending"` | `{ tool_name: string, tool_input: Value, batch: ToolApproval[] \| null }` | HITL 工具审批 | `UserInteractionBroker`（不走事件路由） |
| `"ask-user"` | `{ questions: Question[] }` | Agent 发起的多问题表单 | `UserInteractionBroker`（不走事件路由） |
| `"rewind-preview"` | `{ files: FileChange[], messages: RewindMessage[] }` | 回滚变更预览 | `peri/unstable-event` |
| `"oauth-needed"` | `{ server_name: string, auth_url: string }` | MCP 服务授权 | `peri/unstable-event`（未实现） |

> HITL 审批和 AskUser 不经过 `peri/unstable-event` 或 `peri/agent_event` 通道——通过 `UserInteractionBroker` 直接交互。TUI 侧通过 `HITL_RESPONSE_TX` / `ASK_USER_TX` 发送回答，走 `session/execute-command`。

### 4.6 结构事件（控制消息区布局）

| 事件名 | data 结构 | 语义 | 通道 |
|--------|----------|------|------|
| `"subagent-started"` | `{ agent_id: string, agent_name: string, is_background: bool }` | 子 Agent 创建 | `peri/agent_event`（Category ③+④） |
| `"subagent-stopped"` | `{ agent_id: string }` | 子 Agent 退出 | `peri/agent_event`（Category ③+④） |

子 Agent 的流式事件（`"text-chunk"`、`"tool-started"` 等）通过 `_peri.sourceAgentId` 扩展字段标识归属。TUI 将其路由到对应的 SubAgentGroup 内渲染——不合并到父 Agent 的输出流中。

### 4.7 后台任务事件（bg-task-*）

| 事件名 | data 结构 | 语义 | 通道 |
|--------|----------|------|------|
| `"bg-task-started"` | `BgTaskEntry` | 后台任务注册 | `peri/agent_event` |
| `"bg-task-completed"` | `{ task_id, success, duration_ms }` | 后台任务完成 | `peri/agent_event` |
| `"bg-task-cancelled"` | `{ task_id, reason }` | 后台任务取消 | `peri/agent_event` |
| `"bg-task-snapshot"` | `BgTaskEntry[]` | 全量后台任务快照 | `peri/agent_event` |

### 4.8 Agent Event 扩展事件（P1-5，通过 peri/agent_event 传输）

| 事件名 | data 结构 | 语义 |
|--------|----------|------|
| `"turn-committed"` | `{ messages_json: string, steps: number }` | ReAct 迭代提交信号，TUI 归档 current_turn |
| `"compact-started"` | — | 上下文压缩开始 |
| `"compact-completed"` | `{ summary, files, skills, micro_cleared, messages_json }` | 上下文压缩完成 |
| `"compact-error"` | `{ message }` | 上下文压缩失败 |
| `"background-task-completed"` | `{ task_id, agent_name, success, output, tool_calls_count, duration_ms, child_thread_id? }` | 后台 agent 任务完成 |
| `"agent-execution-failed"` | `{ message }` | agent 执行失败 |
| `"workflow-progress"` | `{ run_id, workflow_name, event_type, agent_id?, phase?, label?, agent_status?, token_count?, tool_count?, run_status?, message? }` | 工作流进度更新 |

### 4.9 插件事件（plugin-*）

| 事件名 | data 结构 | 语义 |
|--------|----------|------|
| `"plugin-snapshot"` | `PluginSnapshot` | 插件列表全量快照 |
| `"plugin-action-result"` | `PluginActionResult` | 插件操作结果通知 |
| `"plugin-search-result"` | `PluginSearchResult` | Discover 搜索返回 |

---

## 5. 事件路由器（peri/unstable-event 通道）

ACP 层持有事件路由器（`router.rs`）——将 `ExecutorEvent` 映射为 `peri/unstable-event` 通知。此通道仅承载**少量新协议事件**，大部分事件已迁移至标准 ACP 或 `peri/agent_event` 通道（见 §3.1）。

### 5.1 活跃映射表

当前仅 3 个 `ExecutorEvent` 变体产出 `peri/unstable-event`：

| ExecutorEvent | 事件名 | data 结构 | 备注 |
|---------------|--------|----------|------|
| `ContextWarning` | `"budget-warning"` | `{ used, limit, threshold }` | 上下文预算警告 |
| `RewindCompleted` | `"rewind-preview"` | `{ files: [], messages: RewindMessage[] }` | 回滚预览（files 字段当前为空） |
| `TurnSuspended` | `"turn-suspended"` | `{}` | Agent turn 挂起，TUI 应停止 loading |

### 5.2 丢弃的 ExecutorEvent

除上述 3 个变体外，所有 `ExecutorEvent` 均不产出 `peri/unstable-event`。丢弃列表包括但不限于：

| 类别 | 变体 | 原因 |
|------|------|------|
| 流式（已迁移至 session/update） | `TextChunk`、`AiReasoning`、`AiReasoningChunk`、`ToolStart`、`ToolEnd` | §3.1 Category ① |
| turn 生命周期 | `TurnStarted`、`TurnEnded`、`TurnCommitted`、`SessionStarted` | Category ③ `peri/agent_event` 或无输出 |
| LLM 生命周期 | `LlmCallStart`、`LlmCallEnd`、`LlmRequestPayload`、`LlmRetrying` | 观测层或 Category ③ |
| Compact 生命周期 | `CompactStarted`、`CompactCompleted`、`CompactError` | Category ③ `peri/agent_event` |
| SubAgent 生命周期 | `SubagentStarted`、`SubagentStopped` | Category ③+④ `peri/agent_event` |
| 状态快照 | `StateSnapshot`、`StateSnapshotMeta`、`MessageAdded` | Category ③ 或 ① |
| 后台/工作流 | `BackgroundTaskCompleted`、`BgToolStep`、`WorkflowProgress`、`WorkflowStarted`、`WorkflowEnded` | Category ③ 或无输出 |
| 预算 | `BudgetThresholdHit` | 静默丢弃 |
| 其他 | `TodoUpdate`、`LspDiagnostics`、`MessageQueueDrained`、`AgentExecutionFailed`、`StageStarted`、`StageEnded`、`MiddlewareStarted`、`MiddlewareEnded` | Category ③ 或无输出 |

> **HITL/AskUser/OAuth**：`ExecutorEvent` 中无对应的 `HitlPending`、`AskUserQuestion`、`OAuthAuthorizationNeeded` 变体。HITL 审批走 `UserInteractionBroker`，AskUser 走 `UserInteractionBroker`，OAuth 走 MCP 服务交互。

---

## 6. 传输层

标准 ACP 方法和自定义事件共享同一传输通道。传输层根据 `method` 字段分流——不同 method name 对应不同语义通道。

### 6.1 传输通道

| 通道 | method | 格式 | 语义 |
|------|--------|------|------|
| 标准 ACP JSON-RPC | `session/new`、`session/prompt` 等 | `{method, params, id?}` | TUI → Agent 请求-响应 |
| 标准 ACP notification | `session/update` | `{method: "session/update", params: {sessionId, update}}` | Agent → TUI 流式推送 |
| TUI 专用 DTO | `peri/agent_event` | `{method: "peri/agent_event", params: {sessionId, event_json}}` | Agent → TUI 低频事件 |
| 新协议事件路由 | `peri/unstable-event` | `{method: "peri/unstable-event", params: {sessionId, event, data}}` | Agent → TUI 少量事件 |
| HITL 审批（预留） | `peri/hitl_pending` | `{method: "peri/hitl_pending", params: {sessionId}}` | HITL 审批信号 |
| Turn 结束信号 | `peri/agent_event_done` | `{method: "peri/agent_event_done", params: {sessionId, stop_reason}}` | Agent → TUI turn 完成 |

### 6.2 传输实现

- **开发环境（TUI 内嵌 ACP）**：`MpscTransport`。同一进程内通过 tokio mpsc 通道传递消息。
- **生产环境（IDE 插件、远程代理）**：`StdioTransport`。stdin/stdout 传递 JSON 消息。

传输层职责限于消息搬动——不做事件过滤、不做事物流、不做重试。通道分流由 EventSink（`event_sink.rs`）在推送时按 `map_event()` 的分路结果决定。

---

## 7. 稳定性

自定义事件通道名为 `peri/unstable-event`，永久保持此名称——不改为 `stable` 或版本化命名。事件名和 data 结构在 v2 开发期间可能变化。标准 ACP 方法按 ACP 协议版本管理。

不稳定期的约束：

- 新增事件名——随时允许
- 修改已有事件的 data 结构——破坏性变更，需同步更新 ACP 和 TUI 两侧的解析代码
- 删除事件名——需确认两侧不再使用
