# ACP 协议功能清单

> 生成日期：2026-07-07 | 数据来源：`peri-acp/`、`peri-acp-types/`、`peri-tui/src/kit/acp_types.rs`

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
| `session/replay` | 加载会话时重放完整历史（UserMessageChunk + AgentMessageChunk） | ✅ 已实现 |

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
| `view-commit` | 完整 ViewModel 列表全量替换 UI | 🗑 废弃（改用 `session/update` 增量 + `turn-done` 边界） |
| `turn-done` | Agent 本轮结束，Streaming → Idle | ✅ 已实现 |
| `turn-interrupted` | Agent 被中断（取消/超时） | ✅ 已实现 |

#### JSON 结构

```json
// turn-done
{ "event": "turn-done", "data": {} }

// turn-interrupted
{ "event": "turn-interrupted", "data": { "reason": "user cancelled" } }
```

### §4.3 状态事件（更新状态栏）

| 事件名 | 作用 | 状态 |
|--------|------|------|
| `token-usage` | 本轮 token 消耗 | 🗑 废弃（`usage_update` meta 已含 input/output/model） |
| `tool-count` | 本轮工具调用次数 | ⚠️ 放入 `usage_update` meta，不单独建事件 |
| `progress` | 进度百分比 + 文本 | 🔲 预留 |
| `budget-warning` | 上下文预算警告（阈值 0.70/0.85） | ✅ 已实现 |
| `system-notification` | 系统通知文本 + 级别 | 🔲 预留 |

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
| `subagent-started` | SubAgent 创建，TUI 打开折叠组 | ✅ 已实现 |
| `subagent-stopped` | SubAgent 退出，TUI 关闭组 | ✅ 已实现 |

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
```

---

## 三、ViewModel 渲染原子（8 种）

| type 标签 | 作用 |
|-----------|------|
| `user-bubble` | 用户消息气泡 |
| `assistant-bubble` | AI 回复气泡（含 reasoning 折叠 + diff 预览） |
| `tool-card` | 工具调用卡片（含 diff 块、运行时长） |
| `system-note` | 系统提示/通知（Info/Warning/Error） |
| `sub-agent-group` | SubAgent 折叠组（含内嵌 ViewModel[]） |
| `collapsed-group` | 通用折叠组 |
| `divider` | 分隔线（可选 label） |
| `ask-user-block` | 用户问答表单块 |

#### JSON 结构

```json
// user-bubble
{ "type": "user-bubble", "text": "hello", "is_system_reminder": false }

// assistant-bubble
{ "type": "assistant-bubble",
  "text": "你好，我可以帮助你。",
  "reasoning": { "text": "用户想打招呼...", "collapsed": true },
  "tool_card_ids": ["tc-1", "tc-2"]
}

// tool-card
{ "type": "tool-card",
  "tool_id": "tc-1",
  "tool_name": "Bash",
  "input_summary": "cargo build",
  "output_summary": "Finished dev [unoptimized] target(s) in 2.34s",
  "is_error": false,
  "is_running": false,
  "running_duration_ms": 2340,
  "diff": {
    "path": "Cargo.lock",
    "hunks": [{ "old_start": 1, "old_count": 3, "new_start": 1, "new_count": 3,
                "lines": [{ "kind": "context", "text": "..." }, ...] }],
    "is_binary": false,
    "is_too_large": false
  }
}

// system-note
{ "type": "system-note", "text": "上下文已压缩", "level": "info" }

// sub-agent-group
{ "type": "sub-agent-group",
  "agent_id": "sa-1",
  "agent_name": "explorer",
  "view_models": [{ "type": "user-bubble", "text": "find ACP code" }, ...],
  "collapsed": false,
  "is_running": true
}

// collapsed-group
{ "type": "collapsed-group",
  "title": "批量工具调用",
  "count": 3,
  "view_models": [{ "type": "tool-card", ... }, ...]
}

// divider
{ "type": "divider", "label": "Round 3" }

// ask-user-block
{ "type": "ask-user-block",
  "items": [{ "header": "确认删除", "answer": "是" }],
  "is_error": false
}
```

### DiffBlock 结构

```json
{
  "path": "src/main.rs",
  "hunks": [{
    "old_start": 10, "old_count": 3,
    "new_start": 10, "new_count": 5,
    "lines": [
      { "kind": "context", "text": "fn main() {" },
      { "kind": "removed", "text": "-    println!(\"old\");" },
      { "kind": "added", "text": "+    println!(\"new\");" },
      { "kind": "context", "text": "}" }
    ]
  }],
  "is_binary": false,
  "is_too_large": false,
  "new_file_preview": null
}
```

### ReasoningBlock 结构

```json
{
  "text": "用户想了解 ACP 协议...",
  "collapsed": true
}
```

---

## 四、TUI 专属通知通道

| 通知名 | 作用 | 通道类型 |
|--------|------|----------|
| `peri/agent_event` | AcpEvent DTO 全量推送（16 种变体，TUI-only） | notification |
| `peri/agent_event_done` | Agent 执行结束信号 | notification |
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

**总结**：标准 ACP 方法 17 个 = 全部已实现 | 自定义事件 13 个（6 已实现 + 5 预留 + 2 待合并到 `usage_update` meta）| 已废弃 11 个（含流式 4 + `view-commit` + `token-usage` + broker 2 + 其他）| ViewModel 8 种。
