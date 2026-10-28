# peri-acp 协议设计

> 已批准变更（2026-10-07，实施中）：移除持久执行恢复及 Work query/resolve、SDK admission/entered/settlement 和 durable control 协议。普通 prompt、输入队列与取消不依赖这些能力；history list/load/resume/replay 保留。状态见[剥离计划](../../spec/issues/2026-10-07-remove-execution-recovery-plan.md)，撤销接口明确 method-not-found，不保留成功 shim。

> 状态：现行设计
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
| `session/new` | `{ cwd?, mcpServers? }` | `{ sessionId, ... }` | 创建新会话；可返回 modes、models、configOptions 等协商字段 |
| `session/load` | `{ sessionId, cwd?, mcpServers? }`；分页时 `{ sessionId, cursor, limit? }`；窗口时 `{ sessionId, history: { kind: "open" \| "page" \| "seek", ... } }` | `{ ... }` | 加载指定会话并重放历史；响应可含 modes、models、configOptions，不创建新身份。带 `cursor` 时是旧历史页模式，带 `history` 时是 `peri.historyWindowV1` 只读窗口模式（两者互斥，见 §2.1.1）。声明窗口能力的连接不带 `history` 的 load **不回放历史**，首屏由 `history.open` 提供 |
| `session/resume` | `{ sessionId, cwd? }` | `{}` | 复用已有 session_id 继续会话；经统一 host lifecycle handler 处理 |
| `session/close` | `{ sessionId }` | `{}` | 关闭会话 |
| `session/fork` | `{ sessionId, cwd?, mcpServers? }` | `{ sessionId, ... }` | 指定源会话并复制到新线程；返回值中的 sessionId 是新身份 |
| `session/list` | `{ cwd? }` | `{ sessions: SessionInfo[] }` | 列出会话（可按 cwd 过滤） |
| `session/rename` | `{ sessionId, title }` | `{ sessionId, title }` | 重命名会话并持久化标题 |

会话级 `mcpServers` 条目可通过 `_meta["peri.subscriptions"]` 声明 `subscriptions/listen`，
形状与 `.mcp.json` 的 `subscriptions` 相同；HTTP 与 stdio 生效，非法值返回 `-32602`，
`type: "acp"` 暂不支持。

会话工作区扩展由 initialize 的 `peri.sessionWorkspaceV1: true` 显式协商：

- `peri/session_context` 接受互斥的 `{ cwd }` 或 `{ sessionId }`，返回 `version: 1`、
  已校验 `workspace`，会话查询额外返回保存的 `binding`。不取得执行权。
- `session/list` 的 `_meta["peri.sessionWorkspaceV1"]` 承载 `ScopedThreadQuery`，
  返回相同扩展键中的 `threads` 与 `nextCursor`。scope 支持项目、工作区、精确目录
  和全局；普通 `cwd` 字段仍表示精确目录。
- new/load/resume/fork 的响应扩展投影绑定。按 ID load/resume 使用保存的 binding，
  请求 cwd 不作为路径认领或 mismatch 拒绝门槛；保存目录、机器环境与绑定完整性
  仍参与执行准入，不能在当前宿主同名目录静默装配另一环境。
- `session/load` / `session/resume` 不取得或校验执行所有权，不返回所有权只读准入或前任 owner 警告。唯一执行者与跨实例协调由 `peri-sdk` 管理；Peri 保留环境、绑定、frozen 与持久化完整性检查，历史通过独立只读入口查询。

工作区扩展的门控事实源是**进程级协商值**（`SessionManager::effective_host_caps`）：同一 host 内
完成过 initialize 的连接所属请求面共享该值，从未 initialize 的连接按 [`PeriCaps::all_enabled`]
兜底（A25 保留③，实现见 `peri-acp/src/session/caps.rs`）。历史分页与历史窗口相反：按**发起连接**的
`ConnectionFacts::history_pagination()` / `history_window_v1()` 失败关闭判定。两者差异是有意的——
分页与窗口决定这次 `session/load` 回放什么内容，必须与连接绑定；工作区扩展表达的是本 host 的
装配事实。同进程多连接接管的部署形态一旦出现，工作区门控需与分页、窗口一起改为按连接判定。

历史分页由 initialize 的 `peri.historyPagination: true` 显式协商，**不改标准 `session/load`
语义**：未协商的连接始终得到标准完整回放。协商过的连接可带非空 `cursor` 请求更早一页，
此时 `session/load` 不重新装配会话（不占执行生命周期锁、不做任务绑定 / MCP 接纳 / 命令广播 /
continuation 调度），响应改为 `{ sessionId, snapshotToken, nextCursor, items }`；`nextCursor`
为 `null` 只表示更早方向没有可展示内容。能力按**当前连接**的协商结果判定，重连后重新分流；
未协商却带 `cursor`/`limit`、游标坏 / 跨会话 / 已失效、`cursor` 与 setup 参数同现都明确拒绝，
不降级为初始 load。字段语义、页预算与条目身份的事实源是 `peri-acp-types::history_page` 与
`docs/code-index/peri-acp.md`。初始 load 后补写 `nextCursor`/`snapshotToken` 的既有语义未改变。

初始 `session/load` 可带可选 `loadGeneration`（非空字符串，缺省由服务端生成）：它在协商过
`replay` 的连接上以 `_meta.periLoadGeneration` 同时出现在成功响应与本次全部重放通知（含
reminder 出口），客户端据此把通知与那次 load 配对；历史页带该字段是坏请求。历史重放投递
失败使 `session/load` 失败（`-32603`），不返回半回放的「成功」响应；服务端已装配的会话态
保持完整，留给下一次 load 原位修复。

#### 2.1.1 历史窗口 `peri.historyWindowV1`（默认关闭）

窗口协议是 `session/load` 上的只读扩展，用于双向浏览与按需回读；它与旧 `cursor` 分页是两套
独立语义。`PeriCaps::history_window_v1` 的 wire key 为 `peri.historyWindowV1`，只接受显式
`true`，缺省/非布尔值为 false；准入按**当前连接**判定，失败关闭。TUI 另以内部开关
`HISTORY_WINDOW_V1`（来自 settings beta `history-window-v1`；未配置视为开启、显式 false 关闭）决定是否采用窗口模型；
协议协商成功不等于启用淘汰。旧 `historyPagination` 路径不因本能力改变。

**load 不回放**：声明 `peri.historyWindowV1` 的连接，`session/load`（不带 `history`）只装配
会话，不产生任何历史回放；首屏由随后的一次 `history.open` 提供。这是协议契约而非可选优化：
客户端声明能力却不 open 会看到空首屏。`historyPagination` 与本能力独立、不能互相替代——
是否跳过回放只看本连接的 `historyWindowV1` 协商结果。

**请求分流**

`session/load` 带 `history` 即进入窗口分类；未协商该能力返回 `-32601`
（`data.reason = "history_window_not_negotiated"`）。`sessionId` 必填；会话不存在
`-32602`，关闭中 `-32010`。`history` 与旧顶层 `cursor`、`limit`、
`loadGeneration`、`cwd`、`mcpServers` 互斥；格式错误、未知 `kind`、未知字段直接 `-32602`，
绝不降级为初始 load。窗口请求只读：不装配会话、不取执行锁、不广播命令、不重连订阅。

`history.kind` 为 `"open"`、`"page"`、`"seek"`：

```json
{ "sessionId": "s", "history": { "kind": "open", "anchor": { "kind": "latest" },
  "direction": "around", "limit": 50, "beforeLimit": 50, "afterLimit": 50,
  "byteBudget": 524288 } }
```

- `open`：`direction` 必填（`around`/`before`/`after`）；`anchor` 缺省 `latest`，或
  `{ "kind": "entry", "entryId": "...", "source": ... }`。`limit` 缺省 50，范围 1–100；
  `beforeLimit`/`afterLimit` 优先；`byteBudget` 范围 1–4 MiB，缺省 512 KiB。`open` 不要求
  客户端携带 ticket/snapshot。
- `page`：`snapshotToken`、`readTicket`、`limit` 必填；`beforeCursor` 与 `afterCursor` 必须
  恰好一个非空。
- `seek`：`snapshotToken`、`readTicket`、`anchor.entryId`、`anchor.source` 必填；
  `beforeLimit`/`afterLimit` 可选。锚点不存在、已被 rewind 删除、来源不符或越出冻结范围返回
  `-32016`，不得从 latest 静默降级。

`source` 的 wire 形状是 `"own"` 或
`{"inherited":{"frozenSnapshotId":"...","ancestorThreadId":"..."}}`。旧 frozen 事实没有
`ancestorThreadId` 时省略该字段，不伪造。

**响应**

成功响应是轻量元信息 + 条目投影：

```json
{
  "sessionId": "s",
  "sessionGeneration": "opaque-live-instance",
  "structureVersion": 7,
  "snapshotToken": "opaque-window",
  "committedTail": "opaque-tail-or-null",
  "checkpointId": "opaque-checkpoint-or-null",
  "readTicket": "opaque-ticket",
  "beforeCursor": "opaque-or-null",
  "afterCursor": "opaque-or-null",
  "items": [],
  "inherited": null,
  "inheritedUnsupported": false,
  "rewindInvalidation": { "status": "active", "reopenRequired": false }
}
```

- `committedTail`/`checkpointId` 可为 `null`；没有已确认 Full checkpoint 时不得伪造。
- `snapshotToken` 由服务端铸造，绑定投影/预算版本、source、范围身份、`committedTail` 与
  `sessionGeneration`；客户端不可解析、不可自造，只能原样回传。
- `beforeCursor`/`afterCursor` 必须出现，且分别锚定**本页最旧/最新行**：非空表示该邻接方向
  仍有可读行，`null` 只表示该方向在当前 snapshot 内没有更多可读行。两个游标可以同时非空；
  `open(latest, before)` 的页包含 durable tail 时 `afterCursor=null` 是预期结果。
  `beforeCursor`/`afterCursor`/`readTicket`/`committedTail` 均为不透明值，客户端只可原样回传，
  不得解释或跨 snapshot 比较。
- `items[]` 含 `entryId`、`source`、`position`（inherited 为 `null`）、`oversize`、
  `subItems[]`。`entryId` 与旧页/回放共用 `<threadId>:<messageId>` 格式；inherited 条目使用
  子会话 id 作为前缀。`position` 只在同一 `snapshotToken` 内可比较；客户端不得解释数值或跨快照比较。
- `rewindInvalidation` 正常为 `{status:"active",reopenRequired:false}`；若返回前发现窗口已被
  失效，应返回错误而不是用旧正文伪造 `reopenRequired` 成功响应。
- oversize 条目在 `items[].body` 内联描述符，字段为 `totalBytes`、`chunkSize`、`readTicket`、
  `snapshotToken`；**不存在顶层 `bodyDescriptors`**，也不得先解码完整正文再切片。
- `inherited` 是冻结前缀摘要（`snapshotId`、`ancestorThreadId`、`firstEntryId`、`lastEntryId`、
  `entryCount`、`supported`）；`snapshotId` 由子会话、字节数、条数、祖先与首尾条目做 SHA-256
  派生。`inheritedUnsupported=true` 表示当前来源未提供分段读取。继承条目只读子会话
  冻结快照，父会话后续 append/compact/rewind 不改变该视图。

**正文分块**

`session/history/body` 的请求为 `{ sessionId, entryId, source, offset?, maxBytes?, readTicket,
snapshotToken }`；`source` 参与客户端请求形状，服务端以 readTicket 绑定的 source/snapshot 为准，
不信任客户端自行改写。`offset` 缺省 0，`maxBytes` 缺省 64 KiB、最大 256 KiB。响应为
`{ entryId, offset, bytes: number[], nextOffset: null | number, complete: bool }`，`bytes` 是
JSON 数字数组（原始 UTF-8 字节），`complete=false` 时 `nextOffset` 必须等于 `offset + bytes.len()`。
分块按原始字节切分，块边界可能落在多字节字符中间；客户端负责跨块重组，并在展示/导出前按
UTF-8 边界裁剪（只允许跳过开头 1–3 字节的跨块残片并显式标记，真实损坏必须报错）。
读取受当前 snapshot tail 与本次 open/page/seek 返回的 oversize descriptor 白名单约束；
非 oversize 条目、越 tail 或非本 snapshot 的条目返回 `-32016`，不退回完整历史读取。未协商
`peri.historyWindowV1` 时该方法与 `history` 请求一样返回 `-32601`。

**提交通知**

持久 Full 确认后，Host 只向当前持票连接发送私有通知 `peri/history_committed`：

```json
{ "sessionId": "s", "sessionGeneration": "...", "structureVersion": 7,
  "checkpointId": "...", "summaryEntryId": "...", "committedTail": "...",
  "hint": { "kind": "open", "anchor": { "kind": "latest" } } }
```

通知按 `checkpointId` 幂等；`summaryEntryId` 可为 `null`。丢失不影响 durable commit，客户端用
`open(latest)` 重新对齐。`CompactCompleted` 不是提交事实，不触发窗口封印或释放。

**ticket 与失效**

- 每个 session 只有一个活动 `readTicket`，且绑定发起 open 的连接；新 open 成功才原子替换
  旧 ticket，失败保留旧 ticket/snapshot/generation 继续可用。
- `page`/`seek`/`body` 在进入存储前和返回前校验 `sessionGeneration`、`structureVersion`、
  ticket epoch、`snapshotToken` 与 `committedTail`；任一变化丢弃已读正文。
- rewind/精确删除 durable 成功恰好推进 `structureVersion + 1` 并撤销 ticket；失败不推进；
  持久化结果不确定撤销 ticket 并进入 `ReloadRequired`：旧 ticket 不再续用，但只读的
  `open`/`page`/`seek`/`body` 仍按 Store 事实服务（`HandedOff` 同样不因执行上下文不驻留而
  拒绝只读）。关闭后重新 load 必须改变 `sessionGeneration`，旧 ticket/window/事件全部丢弃。
- ticket 是进程内不透明值 + 精确匹配，绑定 connection/session/generation/structure/snapshot/
  source/tail/投影版本/时间/nonce；没有密码学签名，也不是访问授权。

**错误码**

| code | `data.reason` | 条件 |
|---:|---|---|
| `-32601` | `history_window_not_negotiated` | 未协商 V1 却带 `history` |
| `-32602` | `invalid_history_request` / `session_not_found` | schema、互斥字段、limit、会话不存在 |
| `-32010` | `session_closing` / `history_mutation_in_progress` | 关闭或 rewind 正在占用变更门 |
| `-32011` | `cross_session_cursor` | cursor/ticket 的 session 身份不符 |
| `-32012` | `history_window_invalidated` | generation/structure 变化、rewind/精确删除、关闭重开 |
| `-32014` | `history_read_ticket_invalid` / `history_read_ticket_expired` | ticket 不存在、过期、撤销或连接不匹配 |
| `-32015` | `history_snapshot_mismatch` | snapshot/tail/来源/投影版本不一致 |
| `-32016` | `history_anchor_unavailable` | seek 锚点不可用、body 非 oversize 或越 tail |
| `-32603` | `history_read_failed` | 存储损坏、不可用、超时或后端未实现 |

旧 `cursor` 分页继续使用既有 `-32011`/`-32012` 映射，不把 V1 错误码倒灌到旧 SDK。

**后端边界**

本机 SQLite 实现 `history_window_bounds`/`load_history_window_page`/`seek_history_window`/
`load_history_body_chunk` 与 inherited 冻结读取；canonical inherited 分页/seek 按冻结布局的
数组下标与原始 span 增量计算长度，正文分块经 blob 句柄增量读取；只有非 canonical 信封才
回退 `json_each` keyset 与 `substr`。`ancestor_thread_id` 取 `threads.parent_thread_id`（旧数据
无该字段时为 `null`）。remote/emscripten adapter 未覆写这些端口，默认返回 `Unsupported`
（出口 `-32603`），不得返回空页或空历史；能力协商成功不等于后端支持，因此这些后端不构成
可用的窗口/继承实现。窗口能力默认关闭；未启用时旧分页路径保持原行为。


协商过分页的连接在同一份 `_meta` 上还会拿到条目身份与位置：`periEntryId`（`{会话}:{payload}`，
与历史页 `items[].entryId` 共用 `dispatch::entry_id` 这一份格式）与 `periPosition`（该条目在**存储序**中的位置，`rowid` 原样给出的不透明整数；继承条目
在本线程没有行，因此只有身份、没有位置）。历史页 `items[].position` 给的是同一把尺子，
`subItems[].position` 则是该子项在本条 payload 投影序列中的下标。客户端只能在同一个
`snapshotToken` 内比较 `position`（用于排序与区间合并，把后到的页并进稀疏首屏的正确区间），
不得解释其数值、不得跨快照比较（rewind / 精确删除会删尾部行并让新行复用 rowid），也不要
把它当成时间戳或行号。页严格更早于上一页，页内按历史正序，合并契约的完整表述见
`peri-acp::host::history_page` 模块文档。

类型事实源为 `peri-acp-types::workspace`；身份、恢复与执行准入约束见
[会话身份与工作区设计](session-id-environment.md)。

### 2.2 交互

| 方法 | 参数 | 返回值 | 语义 |
|------|------|--------|------|
| `session/prompt` | `{ sessionId, message: { role: "user", content }, attachments?, bgResults?, requestId? }` | `{ stopReason }` | 提交用户输入（**request-response**，响应携带 `StopReason`；sessionId 同时支持 `session_id` 别名；`requestId` 为可选的本轮 turn 标识，服务器随 `peri/agent_event_done` 回带，供 TUI stale 事件配对）。长耗时 prompt 在服务端 spawn 后台执行，避免阻塞 `session/cancel` 等后续消息 |
| `session/cancel` | `{ sessionId, generation?, requestId? }` | — | 中断当前 Agent（notification）；待发送队列的运行可携成对 generation/requestId，旧身份不得取消新执行；缺省保留既有取消语义 |
| `session/input/*` | 见共享 `UserInput*Request` DTO | 队列快照或操作回执 | `peri.userInputQueue` 协商的 enqueue / dispatch / takeback / snapshot；短控制请求与实际运行分离，见[用户待发送设计](user-input-queue.md) |
| `session/execute-command` | — | — | **无生产调用者**。Slash 命令由 executor 入口的 `session/command/` 注册表（`CommandRegistry`）拦截处理，不走此 JSON-RPC 方法；HITL 审批和 AskUser 回答也不经过它（见 §2.3 注） |

### 2.3 查询与控制

| 方法 | 参数 | 返回值 | 语义 |
|------|------|--------|------|
| `session/set_mode` | `{ modeId?, sessionId? }` | `{}` | 切换**权限模式**（default / plan / acceptEdits 等） |
| `session/set_config_option` | `{ sessionId?, configId, value }` | `{}` | 更新配置项；`configId` 支持 `mode` / `model`（模型切换）/ `thinking_effort` / `context_1m`（有 session 时用 request，无 session 时用 `session/config_update` notification） |
| `session/update_config` | `{ sessionId?, config }` | `{ configOptions }` | 完整 `PeriConfig` 替换（校验 providers 非空与 profile→provider 引用），变更后推送 `config_option_update` 并 invalidate LLM 实例 |
| `session/switch-model` | — | — | 无服务端注册；模型切换走 `session/set_config_option` 的 `configId="model"` 分支；stdio 与 mpsc 共用统一 request dispatch |
| `session/switch-provider` | — | — | 未实现，无服务端注册 |
| `peri/mcp/control` | 严格 `{ sessionId, serverName, action, config? }` | `{ sessionId, serverName, action, status }` | 当前连接显式协商 `peri.mcpControlV1`，按会话环境注册/重启/关闭普通 MCP；完整 wire、生命周期与错误见 [ACP MCP 运行控制](acp-mcp-control.md) |
| `mcp/list` | `{ sessionId }` | `{ servers }` | 精确查询会话池并返回每个服务的 `control`；新控制能力独立允许查询，旧 OAuth/TUI 查询能力保留 |

> **已移除/不存在方法**：`session/approve` 和 `session/answer` 已废弃——HITL 审批经 broker JSON-RPC `session/request_permission` 往返，TUI 通过 `send_response`（JSON-RPC response）回传审批结果（`hitl_response.rs`），AskUser 同理走 `elicitation/create` + `send_response`（`ask_user_action.rs`），均不走 execute-command。`session/query` 和 `session/suggest-files` 未实现——文件补全走本地 `FILE_LIST` atom + `SkimMatcher`。

### 2.4 插件管理

| 方法 | 参数 | 返回值 | 语义 |
|------|------|--------|------|
| `plugin/search` | `{ query, sessionId? }` | `{ results }` | 搜索插件市场 |
| `marketplace/add` | `{ source, sessionId? }` | `{ success, name }` | 宿主全局 marketplace catalog 注册与刷新，经 PluginManagerPort 持久化；读取、刷新或保存失败返回 -32603 并记录日志 |
| `marketplace/remove` | `{ name, sessionId? }` | `{ success }` | 宿主全局 marketplace catalog 删除，经 PluginManagerPort 持久化并清理宿主缓存；不删除本地源目录；读取或保存失败返回 -32603 并记录日志 |
| `marketplace/refresh` | `{ name, sessionId? }` | `{ success, pluginCount }` | 按宿主 catalog 名称刷新；失败返回 -32603 |
| `plugin/list` | `{ sessionId? }` | `{ plugins, hooks }` | 当前会话装配插件投影，不扩展为宿主所有安装插件；来源与可写安装范围分开 |
| `plugin/install` | `{ name, marketplace, scope?, sessionId? }` | `{ success, plugin }` | scope 仅 user/project/local，缺省 user；未知值或非字符串返回 -32602；project/local 必须取有效 session 的绝对执行目录 |
| `plugin/uninstall` | `{ pluginId, scope?, sessionId? }` | `{ success }` | scope 缺省 user；显式安装范围贯穿生产端口与 installer，以 ID + scope + projectPath 精确匹配唯一记录；校验在记录修改前完成 |
| `plugin/toggle` | `{ pluginId, enable, scope, sessionId? }` | `{ success }` | 宿主统一持久化启用/禁用；project/local scope 必须取有效 session 的执行目录，不回退用户级配置 |
| `cron/list` | `{ sessionId }` | `{ jobs }` | 查询实际会话环境的定时任务；无能力显式报错 |
| `cron/toggle` | `{ sessionId, id }` | `{ id, success }` | 切换会话环境中的任务；校验 session/environment/workspace scope |
| `cron/remove` | `{ sessionId, id }` | `{ id, success }` | 删除会话环境中的任务；不存在或不可用显式失败 |
| `plugin/update` | `{ pluginId, scope?, sessionId? }` | `{ success, plugin }` | scope 缺省 user；与卸载共用精确安装身份，不根据 session cwd 猜范围；结果同时推送 `plugin-action-result` / `plugin-snapshot` 通知 |

`marketplace/add`、`marketplace/remove`、`marketplace/refresh` 操作宿主全局 catalog，不按 project 或 session 重定位配置；可选 `sessionId` 仅供客户端响应 ticket 生命周期关联。TUI 经统一 PluginOperation 展示 pending/error，成功响应后刷新本地只读浏览缓存；服务端错误不被吞掉。

`plugin/list` 的 `source="session"` 只表示展示来源，不能作为写入 scope。`install_scope` 从匹配插件 ID（name + marketplace）及安装根的真实记录核实，只返回 user/project/local 或 null；`toggle_supported` 表示是否核实可写范围，不能确认（含同根多范围歧义、无安装记录的 managed 来源）时为 false，并提供 `management_error`。宿主 snapshot 的 `load_error` 保留为独立加载诊断，不用管理错误覆盖；不返回 pluginConfigs 或 MCP 配置正文。toggle 的生产端口还核对请求 scope/cwd 对应的安装记录，不能通过手填 user scope 获得未记录来源的写权限。

ACP install/update/uninstall 的 project/local 目录只来自受信 session 状态，不读取请求自带 project_dir，不回落进程 cwd。所有管理动作使用明确 InstallScope；update/uninstall 的外部请求缺省 user。CLI install/update/uninstall/enable/disable 统一解析 scope（缺省 user），仅 project/local 获取 CLI 进程真实 current_dir 作为 host-local 受信上下文；无法获取或目录无效时失败，不回退 user。user 范围的 projectPath 固定为空，无 session 可用，即使带 project session 也不选择项目记录；project/local 仅匹配受信 cwd 下同 ID、同 scope 的记录，不回退其他范围。同 ID 的 user/project 或 project/local 记录可以并存，选择一条不会修改另一条的安装记录、配置或独立 root。精确身份无记录或出现多条匹配均 fail closed；project/local 卸载不删除用户 pluginConfigs。目录缺失/非绝对、失效 session、关闭中 session 在复制或修改安装记录前失败；此校验不是跨文件事务保证，后续 I/O 失败仍可能留下部分成果，MCP 配置变更仍需下次装配。

### 2.5 后台任务、工作流与 rewind

> 本表方法与 `session/rename`、`plugin/*`、`session/cancel-bg-task`、`workflow/*`、`marketplace/*`、`mcp/*` 均在统一宿主（`host/requests.rs`）注册；stdio 与 TUI 共用同一 `run_acp_server` + `handle_request`（`host/stdio/mod.rs`），stdio 通道同样可用这些方法（wire 验证见 `host/stdio/run_server_integration_test.rs`）。stdio 特有差异仅剩命令面过滤：`stdio_command_filter=true` 时 `core:rewind` / `core:clear` 从命令列表/补全隐藏。

| 方法 | 参数 | 返回值 | 语义 |
|------|------|--------|------|
| `session/cancel-bg-task` | `{ sessionId, taskId }` | `{ success }` | 会话任务取消入口；owner 路由与结果语义见 [Session 异步任务架构](session-async-tasks.md) |
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
- `peri/history_committed` 是 `peri.historyWindowV1` 的私有提交通知，只发给当前持票连接；
  字段与幂等语义见 §2.1.1。
- `peri/agent_event_done` 承载本轮终止状态，并可带 `requestId` 关联提交请求。

不要为新 Agent 事件定义平行的 `{event, data}` 协议；需要新增或扩展时，按 ARC-EVENT-001 同步更新事件类型、ACP 映射与 TUI 消费。

### 3.3 设计原则

1. **事件链路单一**：新增事件经 v2 EventBus、ACP 映射和协议化面到达 TUI，不恢复 v2_tx 或其他直连通道。
2. **协议面类型化**：标准更新使用 ACP 类型，TUI 专用通知使用 `AcpEvent` DTO；两者的兼容性由映射层维护。
3. **终止可观测**：每个 terminal 事件必须使客户端离开 loading 状态。
4. **传输无关**：`MpscTransport` 与 ACP stdio 都进入统一 host 和 request dispatch；transport 不解释 Agent 业务逻辑。

---

## 4. 事件目录

`peri/agent_event` 的 `ExecutionStarted` DTO 包含 `generation`（当前 session mailbox 代际）与 `request_id`（实际宿主 attempt 的非空身份）。它先于该 attempt 的权限请求与提问送达；对应 `peri/agent_event_done.requestId` 只关闭相同身份。该开始事件不要求用户输入队列能力开启，不代表输入已 claim。内部 continuation 与定时审批也遵循此契约；定时审批和之后执行各有独立身份。managed 用户输入仍使用原有 `UserInputRunStarted`，两者归约到同一客户端执行生命周期。

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
| 历史窗口提交 | `peri/history_committed`（私有） | 只发当前持票连接；按 checkpointId 幂等封印 durable 段，丢失可用 `open` 对齐 |
| 上下文使用量 | `session/update` 的 usage 更新与状态快照 | 当前没有单独的用户可见 `budget-warning` 生产路径 |
| compact、subagent、后台任务、workflow | `peri/agent_event` | 由 TUI 专用 DTO 消费 |

HITL 与 AskUser 通过标准交互协议（`UserInteractionBroker`、`RequestPermission`、`Elicitation`）往返；它们不是通用事件目录的一部分。

### 4.3 兼容与扩展事件

`peri/unstable_event` 不再承载 v2 Agent 事件映射。它仅保留给兼容或特定扩展用途；新增 Agent 事件必须经 ARC-EVENT-001 规定的 `session/update` 或 `peri/agent_event` 链路接入。

---

## 5. 事件映射职责

事件映射收敛在 `peri-acp/src/event/` 的 EventSink 与协议化层：负责把 Agent v2 事件转换为标准更新、TUI 专用 DTO 或终止通知。transport 的 router 只负责消息编解码与分发，不应作为 Agent 事件映射事实源。`peri/history_committed` 是 Host 窗口协调器在 durable commit 确认后直接发送的私有通知，不经过 Agent v2 EventBus；它不是 `CompactCompleted` 的替代，也不改变 ARC-EVENT-001 的 Agent 事件链。

---

## 6. 传输层

标准 ACP 方法和自定义事件共享同一传输通道。传输层根据 `method` 字段分流——不同 method name 对应不同语义通道。

### 6.1 传输通道

| 通道 | method | 格式 | 语义 |
|------|--------|------|------|
| 标准 ACP JSON-RPC | `session/new`、`session/prompt` 等 | `{method, params, id?}` | TUI → 服务请求或 notification |
| 标准 ACP notification | `session/update` | `{method: "session/update", params: {sessionId, update}}` | 服务 → 客户端流式与使用量更新 |
| TUI 专用 DTO | `peri/agent_event` | `{method: "peri/agent_event", params: {sessionId, event_json}}` | 服务 → TUI 低频状态、结构与扩展事件 |
| 历史窗口提交 | `peri/history_committed` | `{method: "peri/history_committed", params: {sessionId, sessionGeneration, structureVersion, checkpointId, summaryEntryId, committedTail, hint}}` | 服务 → 当前持票连接；按 checkpoint 幂等，丢失不影响 durable commit |
| 标准交互 | `session/request_permission`、`elicitation/create` | ACP method/response | HITL 与 AskUser 往返 |
| 兼容/扩展 | `peri/unstable_event` | 兼容或特定扩展 payload | 不作为新 Agent 事件的默认通道 |
| 输入预测 | `peri/prediction_ready` | `{method: "peri/prediction_ready", params: {sessionId, text, actions}}` | 服务 → 客户端输入预测建议 |
| Turn 结束信号 | `peri/agent_event_done` | `{method: "peri/agent_event_done", params: {sessionId, stopReason, requestId?}}` | 服务 → 客户端 turn 完成（wire 字段为 camelCase `stopReason`；`requestId` 可选，prompt 携带时回带；待发送队列的服务端 run 始终与 RunStarted 身份配对） |

### 6.2 传输实现

- **TUI 内嵌 ACP**：`MpscTransport` 在同一进程内通过 tokio mpsc 搬运 wire 消息。
- **IDE / stdio**：`run_acp_stdio(StdioInput)` 完成 stdio 编解码与部署装配，然后进入 `run_acp_server_with_sessions`；它与 TUI 共用 `handle_request` 和 session/prompt 主路径。`session/new` response 必须先于首次 commands notification。legacy `type:cancel` 是兼容性的全 session 强停入口，不等于标准 `session/cancel`。

传输层职责限于消息搬动——不做事件过滤、不做事物流、不做重试。事件协议化与通道选择由 `peri-acp/src/event/` 的 EventSink 和映射层决定。

### 6.3 stdio 提问转发行为

stdio 路径与 TUI/notify（mpsc）共用同一 `AcpTransportBroker`（`broker/transport_broker.rs`）：broker 只依赖最小面 `RequestTransport` 契约（仅 `send_request`），mpsc 经 `AcpTransport` blanket 桥接（`AcpRequestBridge`），stdio 经 `ConnectionTo<Client>` 直连适配（`transport/mod.rs`），协议帧一致（`elicitation/create` + accept/cancel/decline，共享 `build_elicitation_params` / `parse_elicitation_response`）。stdio 装配差异仅由构造参数表达：`with_auto_approve()`（无审批 UI，审批分支无条件批准）+ `with_timeout()`（提问超时兜底）。行为限制：

- **`session/cancel` 不解除挂起提问**：挂起的 elicitation request 由 ACP transport 层管理，`session/cancel` 只中断 agent turn；客户端不响应时提问会一直挂起，直到超时或 transport 关闭。
- **非交互客户端的 cancel 声明**：客户端取消时可在响应 `_meta` 的 `peri.elicitationUnanswered` 声明「无人可作答」（`-p` 声明 `non_interactive_client`）→ `parse_elicitation_response` 产出 `InteractionResponse::Unanswered`，`AskUserQuestion` 以失败工具结果如实转述原因；取值无法识别收敛为 `Unknown`（只说无人可作答，不转述客户端文本）。缺声明的 cancel（用户撤销弹窗、owner 失效、投递失败等生命周期结算）保持旧语义的空答案，不得伪称非交互客户端（ARC-HITL-001）。
- **超时兜底**：`PERI_ASK_USER_TIMEOUT_SECS`（秒，缺省 300，`0` 表示不超时）——超时返回 `Rejected`（LLM 侧表现为 `ToolRejected`），不挂死 turn。
- **断连兜底**：transport 关闭（incoming EOF）时挂起请求自动失败，返回空答案，会话可继续。

---

## 7. 兼容性

`peri/unstable_event` 的遗留兼容 payload 可能随协议演进变化；新事件应优先使用 `session/update` 或 `peri/agent_event`。修改既有 wire payload 时，必须同步更新 ACP 与 TUI 两侧解析，并按 ARC-EVENT-001 验证完整事件链路。
