# peri-acp 协议设计

> 设计起点：2026-07-15（v2.1 修订） | 最后核对：2026-08-22
>
> 本文是 wire 语义说明；当前实现入口以 `docs/code-index/peri-acp.md` 为准，跨层不变量以 `docs/standards/architecture-contracts.md` 为准。

## 1. 协议分层

ACP 协议分为标准 ACP 方法（TUI → 服务）与 ACP 事件通知（服务 → TUI）。标准方法承载请求-响应和交互；事件经 ACP 的 v2 映射与协议化面投递给客户端。

**标准 ACP（JSON-RPC 方法）**：TUI 调用，ACP 执行并返回。覆盖会话生命周期、prompt 提交、配置/权限控制、插件管理、后台任务与工作流控制。请求-响应语义——发一个请求，收一个结果。

**ACP 事件通知**：当前 TUI 的主事件面是标准 `session/update` 和 `peri/agent_event`。`peri/unstable_event` 只保留兼容或特定扩展用途，不作为通用 Agent → TUI 事件管道；完整链路见 `docs/standards/architecture-contracts.md` 的 ARC-EVENT-001。

这些消息复用传输通道；transport 只负责编解码和分发，不解释 Agent 业务语义。

---

## 2. 标准 ACP 方法（TUI → Agent）

TUI 的所有主动行为通过标准 ACP JSON-RPC 方法调用。不定义自定义事件来替代标准协议已有的操作。

### 2.1 会话生命周期

| 方法 | 参数 | 返回值 | 语义 |
|------|------|--------|------|
| `session/new` | `{ cwd?, model?, permission_mode? }` | `{ session_id }` | 创建新会话 |
| `session/load` | `{ session_id }` | `{ session_id }` | 恢复历史会话（历史经 `session/update` 重放） |
| `session/resume` | `{ sessionId, cwd? }` | `{}` | 复用已有 session_id 继续会话；经统一 host lifecycle handler 处理 |
| `session/close` | `{ session_id }` | `{}` | 关闭会话 |
| `session/fork` | `{ source_session_id }` | `{ new_session_id }` | 复制当前会话到新线程 |
| `session/list` | `{ cwd? }` | `{ sessions: SessionInfo[] }` | 列出会话（可按 cwd 过滤） |
| `session/rename` | `{ sessionId, title }` | `{ sessionId, title }` | 重命名会话并持久化标题 |

### 2.2 交互

| 方法 | 参数 | 返回值 | 语义 |
|------|------|--------|------|
| `session/prompt` | `{ sessionId, message: { role: "user", content }, attachments?, bgResults?, requestId? }` | `{ stopReason }` | 提交用户输入（**request-response**，响应携带 `StopReason`；sessionId 同时支持 `session_id` 别名；`requestId` 为可选的本轮 turn 标识，服务器随 `peri/agent_event_done` 回带，供 TUI stale 事件配对）。长耗时 prompt 在服务端 spawn 后台执行，避免阻塞 `session/cancel` 等后续消息 |
| `session/cancel` | `{ sessionId }` | — | 中断当前 Agent（notification，非 request-response） |
| `session/execute-command` | — | — | **无生产调用者**。Slash 命令由 executor 入口的 `session/command/` 注册表（`CommandRegistry`）拦截处理，不走此 JSON-RPC 方法；HITL 审批和 AskUser 回答也不经过它（见 §2.3 注） |

### 2.3 查询与控制

| 方法 | 参数 | 返回值 | 语义 |
|------|------|--------|------|
| `session/set_mode` | `{ modeId?, sessionId? }` | `{}` | 切换**权限模式**（default / plan / acceptEdits 等） |
| `session/set_config_option` | `{ sessionId?, configId, value }` | `{}` | 更新配置项；`configId` 支持 `mode` / `model`（模型切换）/ `thinking_effort` / `context_1m`（有 session 时用 request，无 session 时用 `session/config_update` notification） |
| `session/update_config` | `{ sessionId?, config }` | `{ configOptions }` | 完整 `PeriConfig` 替换（校验 providers 非空与 profile→provider 引用），变更后推送 `config_option_update` 并 invalidate LLM 实例 |
| `session/switch-model` | — | — | 无服务端注册；模型切换走 `session/set_config_option` 的 `configId="model"` 分支；stdio 与 mpsc 共用统一 request dispatch |
| `session/switch-provider` | — | — | 未实现，无服务端注册 |

> **已移除/不存在方法**：`session/approve` 和 `session/answer` 已废弃——HITL 审批经 broker JSON-RPC `session/request_permission` 往返，TUI 通过 `send_response`（JSON-RPC response）回传审批结果（`hitl_response.rs`），AskUser 同理走 `elicitation/create` + `send_response`（`ask_user_action.rs`），均不走 execute-command。`session/query` 和 `session/suggest-files` 未实现——文件补全走本地 `FILE_LIST` atom + `SkimMatcher`。

### 2.4 插件管理

| 方法 | 参数 | 返回值 | 语义 |
|------|------|--------|------|
| `plugin/search` | `{ query, sessionId? }` | `{ results }` | 搜索插件市场 |
| `plugin/install` | `{ name, marketplace, scope?, sessionId? }` | `{}` | 安装插件 |
| `plugin/uninstall` | `{ name, sessionId? }` | `{}` | 卸载插件 |
| `plugin/toggle` | `{ name, enabled, sessionId? }` | `{}` | 启用/禁用插件 |
| `plugin/update` | `{ pluginId, sessionId? }` | `{ success, plugin }` | 更新插件（结果同时推送 `plugin-action-result` / `plugin-snapshot` 通知） |

### 2.5 后台任务、工作流与 rewind

> 本表方法与 `session/rename`、`plugin/*`、`session/cancel-bg-task`、`workflow/*`、`marketplace/*`、`mcp/*` 均在统一宿主（`host/requests.rs`）注册；批 3 unify 后 stdio 与 TUI 共用同一 `run_acp_server` + `handle_request`（`host/stdio/mod.rs`），stdio 通道同样可用这些方法（wire 验证见 `host/stdio/run_server_integration_test.rs`）。stdio 特有差异仅剩命令面过滤：`stdio_command_filter=true` 时 `core:rewind` / `core:clear` 从命令列表/补全隐藏。

| 方法 | 参数 | 返回值 | 语义 |
|------|------|--------|------|
| `session/cancel-bg-task` | `{ sessionId, taskId }` | `{ success }` | 取消后台任务（会话不存在时如实报错） |
| `workflow/list_runs` | `{ sessionId }` | `{ runs }` | 列出工作流运行快照 |
| `workflow/kill_agent` | `{ sessionId, runId, agentId }` | `{ killed }` | 终止运行中的工作流 agent |
| `workflow/kill_run` | `{ sessionId, runId }` | `{ killed }` | 终止整个工作流运行 |
| `workflow/resume` | `{ sessionId, runId }` | `{ newRunId, resumedFrom }` | 恢复已暂停的工作流运行 |
| `session/rewind-candidates` | `{ sessionId }` | 最多 64 个 `{ message_id, preview }` | 仅在双向协商 `peri.rewind` 后查询清洗后的 user message 候选 |
| `session/rewind-preview` | `{ sessionId, target_message_id, revert_files }` | `{ preview_fingerprint, file_changes }` | 返回有界、project-relative 的 write/edit 文件影响与一次性预览指纹 |
| `session/rewind` | `{ sessionId, target_message_id, preview_fingerprint, revert_files }` | `{ status: "executed" }` | 执行前重算当前历史；指纹缺失或过期时在任何截断/文件恢复前拒绝 |

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
    ├─ `peri/prediction_ready`：输入预测建议（受 caps.prediction 门控）
    ├─ 标准交互请求：HITL / Elicitation
    └─ `peri/agent_event_done`：turn 终止通知
```

`peri/unstable_event` 不参与上述通用链路；其剩余兼容/扩展用途不应作为新 Agent 事件的目标通道。

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
4. **传输无关**：`MpscTransport` 与 ACP stdio 都进入统一 host 和 request dispatch；transport 不解释 Agent 业务逻辑。

---

## 4. 事件目录

### 4.1 流式事件（高频，每秒数十次）

> **已迁移至标准 ACP `session/update` 通道**。以下事件不再走 `peri/unstable_event`，改由 `map_event()` Category ① 映射为标准 ACP `SessionUpdate` 通知（`ContentChunk` / `ToolCall` / `ToolCallUpdate`）。TUI 侧通过 `acp_notifier.rs` 的 `handle_session_update` 处理。

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

`peri/unstable_event` 不再承载 v2 Agent 事件映射。它仅保留给兼容或特定扩展用途；新增 Agent 事件必须经 ARC-EVENT-001 规定的 `session/update` 或 `peri/agent_event` 链路接入。

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
| 兼容/扩展 | `peri/unstable_event` | 兼容或特定扩展 payload | 不作为新 Agent 事件的默认通道 |
| 输入预测 | `peri/prediction_ready` | `{method: "peri/prediction_ready", params: {sessionId, text, actions}}` | 服务 → 客户端输入预测建议 |
| Turn 结束信号 | `peri/agent_event_done` | `{method: "peri/agent_event_done", params: {sessionId, stopReason, requestId?}}` | 服务 → 客户端 turn 完成（wire 字段为 camelCase `stopReason`；`requestId` 可选，仅提交 prompt 时携带时回带） |

### 6.2 传输实现

- **TUI 内嵌 ACP**：`MpscTransport` 在同一进程内通过 tokio mpsc 搬运 wire 消息。
- **IDE / stdio**：`run_acp_stdio(StdioInput)` 完成 stdio 编解码与部署装配，然后进入 `run_acp_server_with_sessions`；它与 TUI 共用 `handle_request` 和 session/prompt 主路径。`session/new` response 必须先于首次 commands notification。legacy `type:cancel` 是兼容性的全 session 强停入口，不等于标准 `session/cancel`。

传输层职责限于消息搬动——不做事件过滤、不做事物流、不做重试。事件协议化与通道选择由 `peri-acp/src/event/` 的 EventSink 和映射层决定。

### 6.3 stdio 提问转发行为

stdio 路径与 TUI/notify（mpsc）共用同一 `AcpTransportBroker`（`broker/transport_broker.rs`）：broker 只依赖最小面 `RequestTransport` 契约（仅 `send_request`），mpsc 经 `AcpTransport` blanket 桥接（`AcpRequestBridge`），stdio 经 `ConnectionTo<Client>` 直连适配（`transport/mod.rs`），协议帧一致（`elicitation/create` + accept/cancel/decline，共享 `build_elicitation_params` / `parse_elicitation_response`）。stdio 装配差异仅由构造参数表达：`with_auto_approve()`（无审批 UI，审批分支无条件批准）+ `with_timeout()`（提问超时兜底）。行为限制：

- **`session/cancel` 不解除挂起提问**：挂起的 elicitation request 由 ACP transport 层管理，`session/cancel` 只中断 agent turn；客户端不响应时提问会一直挂起，直到超时或 transport 关闭。
- **超时兜底**：`PERI_ASK_USER_TIMEOUT_SECS`（秒，缺省 300，`0` 表示不超时）——超时返回 `Rejected`（LLM 侧表现为 `ToolRejected`），不挂死 turn。
- **断连兜底**：transport 关闭（incoming EOF）时挂起请求自动失败，返回空答案，会话可继续。

---

## 7. 兼容性

`peri/unstable_event` 的遗留兼容 payload 可能随协议演进变化；新事件应优先使用 `session/update` 或 `peri/agent_event`。修改既有 wire payload 时，必须同步更新 ACP 与 TUI 两侧解析，并按 ARC-EVENT-001 验证完整事件链路。
