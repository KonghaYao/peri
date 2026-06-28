# peri-acp 协议设计

> 日期：2026-06-28

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

### 2.2 交互

| 方法 | 参数 | 返回值 | 语义 |
|------|------|--------|------|
| `session/prompt` | `{ session_id, content, attachments? }` | `{}` | 提交用户输入 |
| `session/cancel` | `{ session_id }` | `{}` | 中断当前 Agent |
| `session/execute-command` | `{ session_id, command, args }` | `{}` | 执行 Slash 命令 |

### 2.3 交互应答

| 方法 | 参数 | 返回值 | 语义 |
|------|------|--------|------|
| `session/approve` | `{ session_id, tool_ids?, approved }` | `{}` | HITL 审批应答 |
| `session/answer` | `{ session_id, answers }` | `{}` | AskUser 问答应答 |

### 2.4 查询与控制

| 方法 | 参数 | 返回值 | 语义 |
|------|------|--------|------|
| `session/query` | `{ resource, params? }` | `{ data }` | 面板数据查询 |
| `session/suggest-files` | `{ prefix }` | `{ files }` | @ 提及文件搜索 |
| `config/update` | `{ key, value }` | `{}` | 更新配置项 |
| `session/switch-model` | `{ session_id, model_alias }` | `{}` | 切换模型 |
| `session/switch-provider` | `{ session_id, provider_id }` | `{}` | 切换 Provider |

---

## 3. 自定义事件（Agent → TUI）

Agent 产出的事件经 ACP 事件路由器转换后，通过 `peri/unstable-event` 通道推入 TUI。消息格式为 `{event: 事件名, data: 事件数据}`。

### 3.1 消息格式

```json
{
  "event": "<事件名>",
  "data": <事件数据>
}
```

- `event` — kebab-case 字符串，全局唯一。
- `data` — 每个事件名对应一个特定的 JSON 结构。流式事件的 data 小（几十字节），边界事件的 data 大（可能数十 KB）。

### 3.2 设计原则

1. **字符串事件名，非类型化枚举**：事件名是字符串，不在 Rust 类型系统中定义枚举。新增事件只需约定事件名和 data 结构。
2. **消费端各自保证类型安全**：ACP 事件路由器负责 AgentEvent → `{event, data}` 的映射正确性。TUI 状态机负责按事件名解析 data。通道本身不做类型校验。
3. **高频事件轻量，边界事件完整**：流式事件 data 仅携带原始文本片段。边界事件 data 携带完整 ViewModel 列表。
4. **传输无关**：开发环境用 MpscTransport，生产环境可换 StdioTransport。事件格式不变。

---

## 4. 事件目录

### 4.1 流式事件（高频，每秒数十次）

data 仅携带原始数据，TUI 自行维护 CurrentTurn 增量结构。

| 事件名 | data 结构 | 语义 |
|--------|----------|------|
| `"text-chunk"` | `{ text: string, agent_id?: string }` | 当前气泡或 SubAgentGroup 内追加文本 |
| `"reasoning-chunk"` | `{ text: string, agent_id?: string }` | 推理区域追加文本 |
| `"tool-started"` | `{ tool_id: string, tool_name: string, input_summary: string, agent_id?: string }` | 创建执行中的工具卡片 |
| `"tool-ended"` | `{ tool_id: string, output_summary: string, is_error: bool, agent_id?: string }` | 填充工具卡片结果 |

`agent_id` 字段可选——有值时表示此事件属于子 Agent，TUI 路由到对应的 SubAgentGroup 内渲染。无值时属于主 Agent。

### 4.2 边界事件（低频）

data 携带完整结构或标志状态切换。跳过节流立即渲染。

| 事件名 | data 结构 | 语义 |
|--------|----------|------|
| `"view-commit"` | `{ view_models: ViewModel[] }` | 完整 ViewModel 列表，TUI 全量替换 |
| `"turn-done"` | `{}` | Agent 本轮结束，Streaming → Idle |
| `"turn-interrupted"` | `{ reason: string }` | Agent 被中断（用户取消或超时） |

### 4.3 状态事件（更新状态栏，不触发消息区变化）

| 事件名 | data 结构 | 语义 |
|--------|----------|------|
| `"token-usage"` | `{ input: number, output: number }` | 本轮 token 消耗 |
| `"tool-count"` | `{ count: number }` | 本轮工具调用次数 |
| `"progress"` | `{ percent: number, label: string }` | 进度百分比 |
| `"budget-warning"` | `{ used: number, limit: number, threshold: string }` | 上下文预算警告 |
| `"system-notification"` | `{ text: string, level: string }` | 系统通知文本 |

### 4.4 输入辅助事件

| 事件名 | data 结构 | 语义 |
|--------|----------|------|
| `"prediction"` | `{ text: string }` | 输入预测建议，灰色占位符 |
| `"file-suggestions"` | `{ files: string[] }` | @ 提及文件补全候选 |

### 4.5 交互请求事件（需要用户决策）

| 事件名 | data 结构 | 语义 |
|--------|----------|------|
| `"hitl-pending"` | `{ tool_name: string, tool_input: Value, batch: ToolApproval[] \| null }` | HITL 工具审批 |
| `"ask-user"` | `{ questions: Question[] }` | Agent 发起的多问题表单 |
| `"rewind-preview"` | `{ files: FileChange[], messages: RewindMessage[] }` | 回滚变更预览 |
| `"oauth-needed"` | `{ server_name: string, auth_url: string }` | MCP 服务授权 |

### 4.6 结构事件（控制消息区布局）

| 事件名 | data 结构 | 语义 |
|--------|----------|------|
| `"subagent-started"` | `{ agent_id: string, agent_name: string }` | 子 Agent 创建，TUI 据此创建可折叠 SubAgentGroup |
| `"subagent-stopped"` | `{ agent_id: string }` | 子 Agent 退出，TUI 关闭对应 SubAgentGroup |

子 Agent 的流式事件（`"text-chunk"`、`"tool-started"` 等）通过 `agent_id` 字段标识归属。TUI 将其路由到对应的 SubAgentGroup 内渲染——不合并到父 Agent 的输出流中。

---

## 5. 事件路由器

ACP 层持有事件路由器——将 Agent 层的 AgentEvent 映射为自定义事件名，通过 `peri/unstable-event` 通道推送。映射关系：

| AgentEvent | 自定义事件名 | 备注 |
|-----------|-----------|------|
| TextChunk | `"text-chunk"` | data 仅含 text 字段 |
| ThinkingChunk | `"reasoning-chunk"` | data 仅含 text 字段 |
| ToolStarted | `"tool-started"` | data 含 tool_id、tool_name、input_summary |
| ToolEnded | `"tool-ended"` | data 含 tool_id、output_summary、is_error |
| TurnCompleted | `"view-commit"` | 经视图映射器转换为 ViewModel 列表后作为 data 携带 |
| TurnError（Interrupted / Timeout） | `"turn-interrupted"` | data 含 reason 字符串 |
| TurnError（LlmFailure / RateLimit） | `"system-notification"` | data 含 text 和 level |
| BudgetWarning | `"budget-warning"` | — |
| TokenUsage | `"token-usage"` | — |
| ToolCount | `"tool-count"` | — |
| HitlPending | `"hitl-pending"` | data 含完整审批信息 |
| AskUserQuestion | `"ask-user"` | data 含问题列表 |
| RewindCompleted | `"rewind-preview"` | data 含文件和消息变更 |
| OAuthAuthorizationNeeded | `"oauth-needed"` | data 含 server_name 和 auth_url |
| SubagentStarted | `"subagent-started"` | data 含 agent_id、agent_name。TUI 据此创建 SubAgentGroup |
| SubagentStopped | `"subagent-stopped"` | data 含 agent_id。TUI 据此关闭 SubAgentGroup |

子 Agent 产出的事件（TextChunk、ToolStarted 等）携带 `agent_id`，TUI 据此路由到对应的 SubAgentGroup 内渲染。子 Agent 的输出不合并到父流——在消息区中以独立的可折叠组呈现。

### 5.1 丢弃的 AgentEvent

以下 AgentEvent 仅 Agent 内部有意义，事件路由器不产出任何事件：

- **LlmRetrying** — LLM 重试是 Agent 内部行为，TUI 不需要展示
- **LspDiagnostics** — LSP 诊断仅在工具执行上下文中使用
- **CompactStarted / CompactCompleted** — 上下文压缩对用户透明

---

## 6. 传输层

标准 ACP 方法和自定义事件共享同一传输通道。传输层根据消息格式自动分流——带 `method` 字段的消息走标准 ACP JSON-RPC，带 `event` 字段的消息走 `peri/unstable-event` 自定义事件。

- **开发环境（TUI 内嵌 ACP）**：MpscTransport。同一进程内通过 tokio mpsc 通道传递消息。
- **生产环境（IDE 插件、远程代理）**：StdioTransport。stdin/stdout 传递 JSON-RPC 消息。标准方法走 method/params 格式，自定义事件走 `{event, data}` 格式。

传输层职责限于消息搬动——不做事件过滤、不做事物流、不做重试。

---

## 7. 稳定性

自定义事件通道名为 `peri/unstable-event`，永久保持此名称——不改为 `stable` 或版本化命名。事件名和 data 结构在 v2 开发期间可能变化。标准 ACP 方法按 ACP 协议版本管理。

不稳定期的约束：

- 新增事件名——随时允许
- 修改已有事件的 data 结构——破坏性变更，需同步更新 ACP 和 TUI 两侧的解析代码
- 删除事件名——需确认两侧不再使用
