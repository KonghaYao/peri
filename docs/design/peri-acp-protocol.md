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
| `session/load` | `{ sessionId, cwd?, mcpServers? }`；History V2 连接另带 `_meta["peri.history"]` 的 `initial` 请求 | `{ ... }` | 加载指定会话并重放历史；响应可含 modes、models、configOptions，不创建新身份。协商到 History V2 的连接以 `initial` 首屏替代标准回放（见 §2.1.1）；未声明 `peri.history` 的连接仍走标准完整回放。旧顶层 `cursor`/`limit`/`history`/`loadGeneration` 不再解释，一律 `-32602` |
| `session/resume` | `{ sessionId, cwd? }` | `{}` | 复用已有 session_id 继续会话；经统一 host lifecycle handler 处理 |
| `session/close` | `{ sessionId }` | `{}` | 关闭会话 |
| `session/fork` | `{ sessionId, cwd?, mcpServers? }` | `{ sessionId, ... }` | 指定源会话并复制到新线程；返回值中的 sessionId 是新身份 |
| `session/list` | `{ cwd? }` | `{ sessions: SessionInfo[] }` | 列出会话（可按 cwd 过滤） |
| `session/rename` | `{ sessionId, title }` | `{ sessionId, title }` | 重命名会话并持久化标题 |

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
兜底（A25 保留③，实现见 `peri-acp/src/session/caps.rs`）。History V2 相反：是否协商、预算与
窗口都按**发起连接**判定（`ConnectionFacts`），重连后重新协商。工作区扩展表达的是本 host 的
装配事实；同进程多连接接管的部署形态一旦出现，两类门控都按连接判定。

标准 `session/load` 语义保持不变：未声明 `_meta["peri.history"]` 的连接仍得到标准完整回放。
`caps.replay` 下标准回放用 `_meta.periReplay` 标识；`loadGeneration` 与
`_meta.periLoadGeneration` 已删除，History V2 initial 用 `requestKey`/`deliveryId`/`liveBoundary`
配对。历史重放投递失败使 `session/load` 失败（`-32603`），不返回半回放的成功响应；已装配
会话态保持完整，留给下一次 load 原位修复。

#### 2.1.1 History V2（本地唯一历史路径）

History V2 是本地 SQLite 后端与 TUI 的唯一历史扩展，方法名 `_peri/history`，协商键
`_meta["peri.history"]`。它不修改标准 ACP 方法：未声明该键的连接仍走标准完整回放；声明后
服务端必须显式选择 `version=2` 与 `selectedProfile="windowed"`，否则 `initialize` 失败关闭。
TUI 始终请求 V2，未协商到时启动失败，不发送旧私有请求，也不回退标准全量回放。

**协商与预算**

```json
// initialize.clientCapabilities._meta["peri.history"]
{ "versions": [2], "requestedProfile": "windowed" }

// initialize.agentCapabilities._meta["peri.history"]
{ "version": 2, "selectedProfile": "windowed",
  "method": "_peri/history", "notification": "_peri/history/changed",
  "limits": { "defaultItems": 50, "maxItems": 100,
              "targetPageBytes": 524288, "maxResponseBytes": 1048576,
              "defaultBodyChunkBytes": 65536, "maxBodyChunkBytes": 262144,
              "maxWindowsPerConnection": 8 } }
```

- `maxItems` 上限 100；`targetPageBytes` 范围 64 KiB–512 KiB；`maxResponseBytes` 不得小于
  `targetPageBytes`；正文块上限 256 KiB；每连接窗口数至少 1。
- 客户端只请求 V2 + `windowed`；服务端只回显自己兑现的能力。后端 `supports_history_v2()`
  为 false（remote/emscripten）时返回领域错误 `unsupported_version`，不回显半截能力。
- 客户端同时显式声明任一已删除的旧私有历史能力（值为 `true`），`initialize` 返回 `-32000`
  `unsupported_version`，不产生任何 server capability。
- initial 与只读 RPC 只在协商成功后可用；任何失败都不静默降级为 V1 或标准模式。

**initial（`session/load` 首屏）**

```json
// session/load.params._meta["peri.history"]
{ "version": 2, "action": "initial", "requestKey": "1",
  "limit": 50, "targetBytes": 524288 }
```

成功结果位于同一 `_meta` 键：`{ version, kind:"initial", requestKey, executionReady,
runtimeGeneration, deliveryId, liveBoundary:{streamId,afterSeq}, page }`。`page` 与
`_peri/history` 的页面同形。`executionReady=false` 只表示执行环境未就绪，历史页仍完整可读。

- 准入发生在会话恢复、MCP 装配与历史读取之前。V2 连接缺少 `_meta["peri.history"]` 返回
  `-32602`；未协商连接携带 initial 返回领域错误 `protocol_not_negotiated`。
- 同一 `requestKey` 重放返回字节等价结果且不更换 `deliveryId`；窗口已关闭或交付记录已淘汰
  返回 `delivery_expired`。请求参数不同而复用同一 key 返回 `request_conflict`。
- 同一会话的新 initial 先准备候选窗口/页面/预算与候选实时流，提交成功后才关闭旧窗口并切换
  实时边界；准备、预算、序列化、取消失败都保留旧窗口与旧 `streamId`/`afterSeq`。
- 窗口配额已满时，initial 可用一个替换预留槽（`max + 1`）完成候选读取；成功后关闭旧窗口，
  失败或被取消时候选资源全部回收。

**只读 RPC `_peri/history`**

请求公共字段：`version`、`sessionId`、`requestKey`（十进制字符串，≥1）。根对象以 `action`
判别，每个变体都拒绝未知字段；序列化参数上限 64 KiB。`open/page/seek/body/close` 共用 10 秒
总期限。接收循环内同步完成解码、参数校验与 requestKey 账本登记；后端读取在连接所有权任务中
执行。同时执行的历史读上限为 4，饱和时立即返回可重试的 `capacity_exceeded`；`close` 走快速
路径，不占配额，也不等待慢读。

| action | 参数 | 结果 |
| --- | --- | --- |
| `open` | `anchor`（`latest` 或 `entry{entryId,source}`）、`before`、`after`、`targetBytes` | 新窗口 + 首页 |
| `page` | `windowId`、`snapshotToken`、`readTicket`、`cursor?`、`direction`、`limit`、`targetBytes` | 同窗口一页 |
| `seek` | `windowId`、`snapshotToken`、`readTicket`、`anchor{entryId,source}`、`before`、`after`、`targetBytes` | 重定位后的页 |
| `body` | `windowId`、`snapshotToken`、`readTicket`、`bodyRef`、`offset?`、`maxBytes?` | 正文块 |
| `close` | `windowId`、`snapshotToken`、`readTicket` | `{version,kind:"closed",windowId}` |

- `open(latest)` 要求 `after = 0`；entry 锚点同时给 `before`/`after`，且
  `before + after + 1 ≤ maxItems`。`latest` 不要求客户端预带 ticket/snapshot。
- `page` 的 `cursor` 缺省表示该方向的 home 边界；`direction` 必须与 cursor 方向一致。游标是
  带每连接 MAC 的不透明值：损坏返回 `invalid_cursor`，来自其他窗口返回
  `cross_window_cursor`。
- `seek` 只接受 entry 锚点；锚点不存在、被 rewind 删除、来源不符或越出冻结范围返回
  `anchor_unavailable`，不得从 latest 静默降级。
- `body` 的 `bodyRef` 必须由本窗口签发；`maxBytes` 不得超过协商的正文块上限。own 与
  inherited 都可读取，是否内联到页面不影响正文可达性。
- `close` 幂等；已关闭窗口再次 `close` 重放原成功响应。关闭后 `page`/`seek`/`body` 返回
  `window_closed`。

`source` 的 wire 形状是 `"own"` 或
`{"inherited":{"snapshotId":"...","ancestorSessionId":"..."}}`；继承条目只读子会话冻结快照，
不读取父会话当前历史。

**页面、条目与正文**

页面响应是轻量元信息 + 有序条目：

```json
{
  "version": 2, "kind": "window", "windowId": "w", "snapshotToken": "st",
  "readTicket": "rt", "historyRevision": "42", "committedTail": "41",
  "projectionVersion": 1, "beforeCursor": "opaque-or-null",
  "afterCursor": "opaque-or-null", "items": [],
  "coverage": { "kind": "interval",
    "from": { "kind": "head" },
    "to": { "kind": "position", "region": "own", "ordinal": "42", "side": "before" } },
  "progress": { "reason": "complete", "scannedEntries": 50, "consumedEntries": 50 }
}
```

- `beforeCursor`/`afterCursor` 分别锚定本页最旧/最新条目；`null` 只表示该方向在当前快照内
  没有更多可读条目。游标、`snapshotToken`、`readTicket` 都是不透明值，只能原样回传。
- `historyRevision` 是 own 存储序 tail position，不是持久结构版本；`committedTail` 固定本次
  读取的持久上界；`projectionVersion` 标识展示口径，读取与返回时必须一致。
- `items[]` 含稳定 `entryId`、`source`、`contentVersion`、`durability`、`order` 与
  `subItems[]`；子项变体为 text/reasoning/toolCall/toolResult/reminder/summary/attachment/
  opaque。`entryId` 与标准回放共用 `<sessionId>:<messageId>` 格式。
- `coverage` 描述已消费的半开区间；不得由 items 首尾推导连续。`progress` 显式给出扫描预算
  是否耗尽；连续空页必须推进游标，不能伪装成同一位置。
- 超预算条目在页面上只返回 `HistorySubItem::Opaque` 与内联 `body` 描述符
  （`bodyRef`/`representation`/`mediaType`/`totalBytes`/`chunkBytes`），不内联正文。page
  描述符的 `totalBytes` 是 canonical payload 的字节预估；body 响应的 `totalBytes` 才是
  投影展示长度的权威，客户端在首块到达后修正滚动范围。
- `body` 成功响应为 `{version,kind:"body",sessionId,windowId,snapshotToken,bodyRef,
  contentVersion,representation,encoding:"base64",offset,data,nextOffset,complete,
  totalBytes}`。`complete=true` 时 `nextOffset` 必为 null；未完成时必须严格前进。块边界可
  落在多字节 UTF-8 字符中间，客户端按原始字节重组后再解码。
- body 定位只使用窗口签发的 `bodyRef` 与独立条目元数据读取，不依赖该条目在某一页是内联
  还是 opaque；own/inherited 使用同一表示规则，descriptor 与 body 的
  `representation`/`mediaType` 必须一致。

**预算与幂等**

- 单页最终序列化结果不得超过协商的 `maxResponseBytes`；超限返回 `budget_exceeded` 并保留
  窗口，不返回半页。正文按块读取，单条 canonical payload 超过实现上限（本地 8 MiB）返回
  `budget_exceeded`，不静默截断。
- requestKey 账本是连接级的：成功响应原样重放（上限 256 条 / 8 MiB / 30 分钟），高水位在
  淘汰后仍保留。同键同指纹重放；同键异指纹 `request_conflict`；仍在执行返回
  `operation_in_progress`（120 秒后可按同一 key 重试）；记录被淘汰后返回
  `operation_expired`，initial 返回 `delivery_expired`。
- 参数/形状错误在占用 requestKey 之前返回；失败不写入成功账本。

**错误域与拒绝边界**

领域失败统一使用 JSON-RPC `-32000`，`error.data` 为
`{domain:"peri.history",version:2,reason,recovery,effect:{execution,history,runtimeGeneration}}`；
格式、字段与预算校验失败使用 `-32602`；未知方法（含已删除的 `session/history/body`）使用
`-32601`。错误文本与 `data` 不回显 ticket、cursor 或正文。

| `reason` | `recovery` | 条件 |
| --- | --- | --- |
| `protocol_not_negotiated` | `renegotiate` | 未协商 V2 却发送 initial 或 `_peri/history` |
| `unsupported_version` | `renegotiate` | 版本/profile 不受支持、后端未兑现能力或混入旧能力 |
| `invalid_cursor` / `cross_window_cursor` | `none` | 游标损坏或不属于本窗口 |
| `snapshot_invalidated` / `window_expired` / `window_closed` | `reopen` | 快照、期限或窗口已失效 |
| `ticket_invalid` / `permission_denied` | `reopen` / `none` | 票据不匹配或窗口不属于本会话 |
| `anchor_unavailable` / `not_found` / `index_not_ready` / `source_unavailable` | `none` / `retry` | 锚点、条目或来源不可读 |
| `capacity_exceeded` / `budget_exceeded` | `retry` / `reduce_scope` | 窗口/在途配额或字节预算超限 |
| `backend_unavailable` / `timeout` | `retry` | 后端不可用或总期限耗尽 |
| `request_conflict` / `operation_expired` / `operation_in_progress` / `delivery_expired` | `none` / `retry` | 幂等账本冲突、淘汰或并发 |

在途配额饱和使用 `capacity_exceeded`；类型层另有 `busy`，当前服务端不单独发送。

`effect.execution` 为 `notReady`/`ready`/`unchanged`，`effect.history` 为
`notPublished`/`published`/`unchanged`；`execution=ready` 必须携带 `runtimeGeneration`。
initial 失败时执行事实如实取自恢复结果，历史固定为 `notPublished`。

| 输入 | 预期 |
| --- | --- |
| initialize 只声明已删除的旧私有历史能力 | 不选择、不回显 V2；不把它当作 V2 请求 |
| V2 客户端同时显式声明旧私有能力（true） | `-32000` `unsupported_version`，不产生混合能力 |
| `session/load` 顶层带 `cursor`/`limit`/`loadGeneration`/`history` | `-32602`，发生在恢复、MCP 装配与历史读取之前 |
| V2 连接的 `session/load` 缺少 `_meta["peri.history"]` | `-32602` `history_initial_required` |
| 未协商连接携带 initial | `-32000` `protocol_not_negotiated` |
| `session/history/body` | `-32601` method not found |
| V2 字段中放入旧 token/cursor | 按 V2 身份与 MAC 规则拒绝，无旧解析器回退 |
| TUI 未协商到 V2 | 启动失败；不发送旧分页请求，也不启动标准全量回放 |

**通知、失效与后端边界**

- `_peri/history/changed` 只在协商结果中作为通知名回显，当前服务端不发送；客户端以
  `window_closed`/`window_expired`/`snapshot_invalidated` 响应错误作为失效事实并重新
  `open`。不得把 `CompactCompleted` 当作历史提交或窗口封印通知。
- `page`/`seek`/`body` 在读取前与返回前复核窗口身份、snapshot、ticket、`historyRevision`、
  `committedTail` 与 `projectionVersion`；关闭、删除、rewind 或会话关闭优先使旧窗口失效。
  连接断开时取消在途读并回收窗口与实时流，迟到结果不发布。
- 窗口期限：5 分钟空闲 + 30 分钟绝对期限；`Opening` 占位 60 秒回收；每连接最多 8 个窗口；
  initial 替换允许 `max + 1` 候选槽。窗口与实时流都是进程内状态，服务端重启即清空。
- 本机 SQLite 实现窗口读取、独立条目元数据读取、有界正文读取与 inherited 冻结读取；不新增
  表或列。remote/emscripten 的 `supports_history_v2()` 为 false，协商阶段显式失败，不返回
  空页；跨实例失效不在本协议承诺内。
- 旧私有历史协议已删除：`peri.historyPagination`、`peri.historyWindowV1`、
  `history-window-v1`、`session/load + cursor/limit`、`session/history/body`、
  `peri/history_committed` 与 `_meta.periLoadGeneration` 不再有可执行实现。共用读取与详情基础
  已迁入中性模块 `peri-acp-types::history_common`、`session_resources::history_window` 与
  TUI `history_detail_binding`；回滚只能回退软件版本或撤销提交，不能通过配置开关重新启用
  旧协议。

类型事实源为 `peri-acp-types::history_v2` 与 `peri-acp-types::history_common`；身份、恢复与执行准入约束见
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
- History V2 的实时 transcript 更新仍是标准 `session/update`，只在通知顶层附带
  `_meta["peri.delivery"] = {streamId,deliveryId,eventSeq}`；`_peri/history/changed`
  当前不发送，历史变更由请求错误与重新 `open` 收敛。字段与边界见 §2.1.1。
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
| 历史实时交付 | `session/update` 的 `_meta["peri.delivery"]` | 每连接每会话一条有序流；首屏装配期间有界暂停缓冲，`load` 响应后按序续号；缺口显式暴露 |
| 上下文使用量 | `session/update` 的 usage 更新与状态快照 | 当前没有单独的用户可见 `budget-warning` 生产路径 |
| compact、subagent、后台任务、workflow | `peri/agent_event` | 由 TUI 专用 DTO 消费 |

HITL 与 AskUser 通过标准交互协议（`UserInteractionBroker`、`RequestPermission`、`Elicitation`）往返；它们不是通用事件目录的一部分。

### 4.3 兼容与扩展事件

`peri/unstable_event` 不再承载 v2 Agent 事件映射。它仅保留给兼容或特定扩展用途；新增 Agent 事件必须经 ARC-EVENT-001 规定的 `session/update` 或 `peri/agent_event` 链路接入。

---

## 5. 事件映射职责

事件映射收敛在 `peri-acp/src/event/` 的 EventSink 与协议化层：负责把 Agent v2 事件转换为标准更新、TUI 专用 DTO 或终止通知。transport 的 router 只负责消息编解码与分发，不应作为 Agent 事件映射事实源。History V2 的实时交付不新增事件类型：`live_delivery` 在标准 transcript 通知上附加 `_meta["peri.delivery"]`，不经过第二套事件通道，也不改变 ARC-EVENT-001 的 Agent 事件链。

---

## 6. 传输层

标准 ACP 方法和自定义事件共享同一传输通道。传输层根据 `method` 字段分流——不同 method name 对应不同语义通道。

### 6.1 传输通道

| 通道 | method | 格式 | 语义 |
|------|--------|------|------|
| 标准 ACP JSON-RPC | `session/new`、`session/prompt` 等 | `{method, params, id?}` | TUI → 服务请求或 notification |
| 标准 ACP notification | `session/update` | `{method: "session/update", params: {sessionId, update}}` | 服务 → 客户端流式与使用量更新 |
| TUI 专用 DTO | `peri/agent_event` | `{method: "peri/agent_event", params: {sessionId, event_json}}` | 服务 → TUI 低频状态、结构与扩展事件 |
| 历史实时交付 | `session/update` | 标准通知 + 顶层 `_meta["peri.delivery"] = {streamId, deliveryId, eventSeq}` | 服务 → 客户端；每连接每会话一条有序流，暂停期有界缓冲，缺口显式 |
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
