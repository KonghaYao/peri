# ACP 协议功能清单

> 生成日期：2026-07-15 | 数据来源：`peri-acp/`、`peri-acp-types/`、`peri-tui/src/kit/tui_render_unit.rs`

## 一、标准 ACP 方法（TUI → Agent，JSON-RPC request/response）

### 1.1 会话生命周期

| 方法 | 作用 | 状态 |
|------|------|------|
| `session/new` | 创建新会话 | ✅ 已实现 |
| `session/load` | 加载历史会话 | ✅ 已实现 |
| `session/close` | 关闭会话并 cancel 所有 agent | ✅ 已实现 |
| `session/resume` | 复用已有 session_id | ✅ 已实现 |
| `session/list` | 列出所有会话，支持 cwd 过滤 | ✅ 已实现 |
| `session/fork` | 复制消息到新 thread | ✅ 已实现 |

#### JSON 结构

**`session/new`**
```json
// req
{ "method": "session/new", "params": { "cwd": "/path/to/project", "model": "claude-sonnet-4-20250514", "permission_mode": "default" } }
// res
{ "id": 1, "result": { "session_id": "abc123" } }
```

**`session/load`**
```json
// req
{ "method": "session/load", "params": { "session_id": "abc123" } }
// res
{ "id": 1, "result": { "session_id": "abc123", "messages": [...] } }
```

**`session/close`**
```json
// req
{ "method": "session/close", "params": { "session_id": "abc123" } }
// res
{ "id": 1, "result": {} }
```

**`session/list`**
```json
// req
{ "method": "session/list", "params": { "cwd": "/path/to/project" } }
// res
{ "id": 1, "result": { "sessions": [{ "session_id": "...", "created_at": "...", "cwd": "..." }] } }
```

**`session/fork`**
```json
// req
{ "method": "session/fork", "params": { "session_id": "abc123" } }
// res
{ "id": 1, "result": { "new_session_id": "def456" } }
```

### 1.2 交互

| 方法 | 作用 | 状态 |
|------|------|------|
| `session/prompt` | 提交用户输入 | ✅ 已实现 |
| `session/cancel` | 中断当前 Agent | ✅ 已实现 |
| `session/execute-command` | 执行 / 命令 | ✅ 已实现 |

#### JSON 结构

**`session/prompt`**
```json
// req
{ "method": "session/prompt", "params": {
    "sessionId": "abc123",
    "message": { "content": "hello" },
    "attachments": []
} }
// res
{ "id": 1, "result": {} }
```

**`session/cancel`**
```json
// req
{ "method": "session/cancel", "params": { "session_id": "abc123" } }
// res
{ "id": 1, "result": {} }
```

**`session/execute-command`**
```json
// req
{ "method": "session/execute-command", "params": {
    "session_id": "abc123",
    "command": "/clear",
    "args": ""
} }
// res
{ "id": 1, "result": {} }
```

### 1.3 交互应答

| 方法 | 作用 | 状态 |
|------|------|------|
| `session/request_permission` | HITL 审批（AllowOnce/RejectOnce） | ✅ 已实现 |
| `elicitation/create` | AskUser 问答表单 | ✅ 已实现 |

#### JSON 结构

**`session/request_permission`**
```json
// req (Agent → TUI)
{ "method": "session/request_permission", "params": {
    "session_id": "abc123",
    "tool_calls": [{ "tool_id": "tc-1", "tool_name": "Bash", "input_summary": "rm -rf /" }],
    "options": ["allow_once", "reject_once", "allow_always"]
} }
// res (TUI → Agent)
{ "id": 1, "result": { "decision": "allow_once", "tool_ids": ["tc-1"] } }
```

**`elicitation/create`**
```json
// req (Agent → TUI)
{ "method": "elicitation/create", "params": {
    "session_id": "abc123",
    "questions": [{
        "id": "q1",
        "header": "确认删除",
        "question": "确定要删除吗？",
        "options": [{ "label": "是", "description": "确认删除" }],
        "multi_select": false
    }]
} }
// res (TUI → Agent)
{ "id": 1, "result": { "answers": { "q1": "是" } } }
```

### 1.4 查询与控制

| 方法 | 作用 | 状态 |
|------|------|------|
| `session/query` | 面板数据查询（13 种资源类型） | ✅ 已实现 |
| `session/suggest-files` | @ 提及文件补全 | ✅ 已实现 |
| `config/update` | 更新单个配置项 | ✅ 已实现 |
| `session/update_config` | 完整 PeriConfig CRUD | ✅ 已实现 |
| `session/switch-model` | 切换模型 | ✅ 已实现 |
| `session/switch-provider` | 切换 Provider | ✅ 已实现 |

#### JSON 结构

**`session/query`**
```json
// req
{ "method": "session/query", "params": {
    "session_id": "abc123",
    "resource": "skills"  // skills | cron | mcp | hooks | plugins | agents
} }
// res
{ "id": 1, "result": { "data": [...] } }
```

**`config/update`**
```json
// req
{ "method": "config/update", "params": { "key": "model", "value": "claude-sonnet-4-20250514" } }
// res
{ "id": 1, "result": {} }
```

### 1.5 初始化

| 方法 | 作用 | 状态 |
|------|------|------|
| `initialize` | 握手，声明 AgentCapabilities | ✅ 已实现 |

#### JSON 结构

**`initialize`**
```json
// req
{ "method": "initialize", "params": {
    "protocol_version": "v1",
    "client_info": { "name": "peri-tui", "version": "0.1.0" }
} }
// res
{ "id": 1, "result": {
    "protocol_version": "v1",
    "agent_capabilities": {
        "load_session": true,
        "prompt_capabilities": {},
        "session_capabilities": {
            "list": {},
            "close": {},
            "resume": {},
            "fork": {}
        }
    }
} }
```

### 1.6 命令系统

| 功能 | 作用 | 状态 |
|------|------|------|
| AvailableCommands | 返回内置命令 + 动态 skill 列表 | ✅ 已实现 |
| `session/replay` | 加载会话时重放完整历史（`dispatch/session_replay.rs:replay_session_history()`，通过标准 `session/update` notification 发射 UserMessageChunk、AgentMessageChunk、AgentThoughtChunk、ToolCall、ToolCallUpdate 等事件，非独立 JSON-RPC 方法） | ✅ 已实现 |

---

## 二、自定义事件 `peri/unstable-event`（Agent → TUI 推送）

> 消息格式：`{ "event": "<事件名>", "data": <事件数据> }`
> 事件名均为 kebab-case 字符串。

### §4.1 流式事件 → 已废弃，走标准 `session/update`

> **决策（2026-07-07）**：流式四事件与 ACP 标准 `session/update` 重复，已废弃。
> 详见 `docs/design/decisions/2026-07-07-acp-reuse-first.md`。

| 废弃的自定义事件 | 应使用的 ACP 标准 |
|---|---|
| ~~`text-chunk`~~ | `session/update` → `agent_message_chunk` |
| ~~`reasoning-chunk`~~ | `session/update` → `agent_thought_chunk` |
| ~~`tool-started`~~ | `session/update` → `tool_call` |
| ~~`tool-ended`~~ | `session/update` → `tool_call_update` |

### §4.2 边界事件（低频，跳过节流直接渲染）

| 事件名 | 作用 | 状态 |
|--------|------|------|
| `view-commit` | 完整 ViewModel 列表全量替换 UI | 🗑 废弃（改用 `session/update` 增量） |
| `turn-suspended` | Agent turn 挂起（等待 bg agent/cron/workflow），通知 TUI 停止 loading spinner | ✅ 已实现 |

> **决策（2026-07-08）**：`turn-done` / `turn-interrupted` 改用 ACP 标准 `session/prompt` 响应 `StopReason`（EndTurn / Cancelled），不再作为 `peri/unstable-event` 发送。push_done 签名扩展，AgentDone 通知携带 `stopReason` 字段。
> 注意：TUI 侧 `acp_types.rs:808-812` 仍 decode `turn-done` / `turn-interrupted` 作为 `AcpEventData` 变体，它们可能通过 `peri/agent_event` 等其他通道到达。

### §4.3 状态事件（更新状态栏）

| 事件名 | 作用 | 状态 |
|--------|------|------|
| `budget-warning` | 上下文预算警告（阈值 0.70/0.85） | ✅ 已实现 |
| `progress` | 进度百分比 + 文本 | 🔲 预留 |
| `system-notification` | 系统通知文本 + 级别 | 🔲 预留 |

> **决策（2026-07-07）**：`token-usage` / `tool-count` 已废弃，改走标准 `session/update` → `usage_update` meta。

#### JSON 结构

```json
// budget-warning
{ "event": "budget-warning", "data": { "used": 170000, "limit": 200000, "threshold": "0.85" } }

// system-notification (预留)
{ "event": "system-notification", "data": { "text": "模型已切换", "level": "info" } }
```

### §4.4 输入辅助事件

| 事件名 | 作用 | 状态 |
|--------|------|------|
| `prediction` | 输入预测建议，灰色占位符 | 🔲 预留 |
| `file-suggestions` | @ 提及文件补全候选 | 🔲 预留 |

#### JSON 结构

```json
// prediction (预留)
{ "event": "prediction", "data": { "text": "cargo build --release" } }

// file-suggestions (预留)
{ "event": "file-suggestions", "data": { "files": ["src/main.rs", "src/lib.rs", "Cargo.toml"] } }
```

### §4.5 交互请求事件（需用户决策）

| 事件名 | 作用 | 状态 |
|--------|------|------|
| `rewind-preview` | 回退预览（FileChange + RewindMessage） | ✅ 已实现 |
| `oauth-needed` | MCP OAuth 授权请求 | 🔲 预留 |

> `hitl-pending` / `ask-user`：实际走 broker JSON-RPC（`session/request_permission` / `elicitation/create`），从未作为 `peri/unstable-event` 产出，已从事件目录移除。

#### JSON 结构

```json
// rewind-preview
{ "event": "rewind-preview", "data": {
    "files": [{ "path": "src/main.rs", "change_type": "modified", "diff": null }],
    "messages": [{ "id": "uuid", "role": "assistant", "preview": "I'll edit the fi..." }]
} }

// oauth-needed (预留)
{ "event": "oauth-needed", "data": { "server_name": "github", "auth_url": "https://..." } }
```

### §4.6 结构事件（控制消息区布局）

| 事件名 | 作用 | 状态 |
|--------|------|------|
| `subagent-started` | SubAgent 创建，TUI 打开折叠组 | ✅ 已实现（走 `peri/agent_event`，非 `peri/unstable-event` router） |
| `subagent-stopped` | SubAgent 退出，TUI 关闭组 | ✅ 已实现（同上） |

> **决策（2026-07-08）**：SubAgent 事件不再走 `peri/unstable-event` router（已从 router.rs 删除），改走 `peri/agent_event`（mapper.rs → AcpEvent）通道。router.rs 仅保留 3 个分支：`budget-warning` + `rewind-preview` + `turn-suspended`。

#### JSON 结构

```json
// subagent-started
{ "event": "subagent-started", "data": { "agent_id": "sa-1", "agent_name": "explorer" } }

// subagent-stopped
{ "event": "subagent-stopped", "data": { "agent_id": "sa-1" } }
```

### §4.7 后台任务事件（TUI 端扩展）

| 事件名 | 作用 | 状态 |
|--------|------|------|
| `bg-task-started` | 后台任务启动 | ✅ 已实现 |
| `bg-task-completed` | 后台任务完成 | ✅ 已实现 |
| `bg-task-cancelled` | 后台任务取消 | ✅ 已实现 |
| `bg-task-snapshot` | 活跃后台任务快照列表 | ✅ 已实现 |
| `bg-callback-user-message` | 后台 agent 完成后在消息区插入用户气泡（`AcpEventData::BgCallbackBubble`） | ✅ 已实现 |

#### JSON 结构

```json
// bg-task-started
{ "event": "bg-task-started", "data": {
    "task_id": "bg-1",
    "kind": "subagent",
    "summary": "code review in progress",
    "started_at": "2026-07-07T12:00:00Z"
} }

// bg-task-completed
{ "event": "bg-task-completed", "data": {
    "task_id": "bg-1",
    "success": true,
    "output_preview": "Approved: 0 issues found",
    "duration_ms": 15000
} }

// bg-task-cancelled
{ "event": "bg-task-cancelled", "data": { "task_id": "bg-1", "reason": "session closed" } }

// bg-task-snapshot
{ "event": "bg-task-snapshot", "data": [
    { "task_id": "bg-1", "kind": "subagent", "summary": "running...", "started_at": "..." }
] }

// bg-callback-user-message
{ "event": "bg-callback-user-message", "data": { "text": "Background agent completed: 0 issues found" } }
```

### §4.8 Agent Event Extensions（`peri/agent_event` 通道）

> 以下事件通过 `peri/agent_event` 通道（mapper.rs → AcpEvent）传递，不经过 `peri/unstable-event` router。TUI 侧在 `acp_types.rs:739-785` 定义为 `AcpEventData` 变体。

| 事件名 | 作用 | 状态 |
|--------|------|------|
| `turn-committed` | ReAct 迭代提交信号 | ✅ 已实现 |
| `compact-started` | 上下文压缩开始 | ✅ 已实现 |
| `compact-completed` | 上下文压缩完成（含 summary/files/skills） | ✅ 已实现 |
| `compact-error` | 上下文压缩失败 | ✅ 已实现 |
| `background-task-completed` | 后台 agent 任务完成 | ✅ 已实现 |
| `agent-execution-failed` | agent 执行失败 | ✅ 已实现 |
| `workflow-progress` | 工作流进度更新 | ✅ 已实现 |

### §4.9 Plugin 事件

> Plugin 事件在 TUI 侧 `acp_types.rs:788-793` 定义，`acp_events.rs:713-768` 有渲染逻辑。

| 事件名 | 作用 | 状态 |
|--------|------|------|
| `plugin-snapshot` | 插件列表全量快照 | ✅ 已实现 |
| `plugin-action-result` | 插件操作结果通知 | ✅ 已实现 |
| `plugin-search-result` | Discover 搜索返回 | ✅ 已实现 |

---

## 三、TUI 内部渲染单元（8 种）—— TuiRenderUnit

> **决策（2026-07-08）**：ViewModel 从 `peri-acp-types` 共享 crate 中物理删除，改为 TUI 内部类型 `TuiRenderUnit`（定义于 `peri-tui/src/kit/tui_render_unit.rs`）。不再跨 crate 共享，不再参与 wire 序列化。

| TuiRenderUnit 变体 | 对应渲染用途 | 数据来源 |
|-----------|------|------|
| `TuiUserBubble` | 用户消息气泡 | `session/update` → `user_message_chunk`（replay） |
| `TuiAssistantBubble` | AI 回复气泡（含 reasoning 折叠） | `session/update` → `agent_message_chunk` + `agent_thought_chunk` |
| `TuiToolCard` | 工具调用卡片（含 diff、运行时长） | `session/update` → `tool_call` + `tool_call_update` |
| `TuiSystemNote` | 系统提示/通知（Info/Warning/Error），内含 `ReminderType` 子分类（10 种：ChannelMessage、CronReminder、BgTaskCompleted、ForkMode、ContextCompacted、ContinuationHint、TrustBoundary、ToolReminder、SubagentResult、GenericReminder） | TUI 端从 `config_option_update` / `session_info_update` 等派生 |
| `TuiSubAgentGroup` | SubAgent 折叠组（含内嵌 TuiRenderUnit[]） | `peri/agent_event` → SubagentStarted/Stopped + 流式事件 |
| `TuiCollapsedGroup` | 通用折叠组 | TUI 端连续同类事件合并逻辑 |
| `TuiDivider` | 分隔线（可选 label） | TUI 端在 TurnDone 后自动插入 |
| `TuiAskUserBlock` | 用户问答表单块 | `elicitation/create`（标准 ACP broker JSON-RPC） |

> 原 `peri-acp-types/src/view_model.rs` 已删除（427 行）。DiffBlock / ReasoningBlock / Hunk 等辅助类型同步内部化，加 `Tui` 前缀。render_bridge 从 VIEW_MODELS atom 读取 TuiRenderUnit，render_v2_vm 逐变体渲染。

---

## 四、TUI 专属通知通道

| 通知名 | 作用 | 通道类型 |
|--------|------|----------|
| `peri/agent_event` | AcpEvent DTO 推送（SubAgent/Compact/LSP/BgTask/WorkflowProgress 等，详见 §4.8） | notification |
| `peri/agent_event_done` | Agent 执行结束信号（含 `stopReason` 字段） | notification |
| `peri/hitl_pending` | HITL 审批专用通道 | notification |
| `peri/observable` | SubAgent 启动/停止观测 | notification |

> Stdio 传输仅支持 `session/update`（标准 SessionUpdate），不支持上述 `peri/*` 通知。

---

## 五、传输层

| 组件 | 作用 |
|------|------|
| `AcpTransport` trait | 4 方法：send_request / send_notification / recv / send_response |
| `MpscTransport` | 内存通道对（TUI 内嵌 ACP），含后台 pump task |
| `StdioTransport` | stdin/stdout 新行分隔 JSON-RPC 2.0（IDE 插件），含后台 4 路分路 pump |
| `IncomingMessage` 枚举 | Request { id, method, params } / Notification { method, params } / Response { id, result } |
| `AcpError` | code: i64, message: String, data: Option\<Value\> |

---

## 六、已丢弃的内部事件（不产出自定义事件）

`LlmRetrying` / `LspDiagnostics` / `CompactStarted` / `CompactCompleted` / `CompactError` / `LlmCallStart` / `LlmRequestPayload` / `MessageAdded` / `StateSnapshot` / `StateSnapshotMeta` / `BackgroundTaskCompleted` / `BgToolStep` / `WorkflowProgress` / `TodoUpdate`

---

**总结**：标准 ACP 方法 17 个 = 全部已实现 | `peri/unstable-event` 路由器仅剩 3 个（`budget-warning` + `rewind-preview` + `turn-suspended`）| `turn-done` / `turn-interrupted` 改用标准 `session/prompt` StopReason | ViewModel 已内部化为 TUI 端 TuiRenderUnit，不再跨 crate 共享 | §4.8 Agent Event Extensions 通过 `peri/agent_event` 通道传递 7 种事件 | §4.9 Plugin 事件 3 种。
