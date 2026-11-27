# Peri Swarm 本地开发宿主

> 状态：**已实现单机开发宿主；生产拓扑为已批准目标**。本地链路验收见
> [基础链路](../verification/peri-swarm-local-dev.md)与
> [同会话追问、空闲回收及宿主中断](../verification/peri-swarm-local-followup-2026-11-23.md)。
> 真实模型、多租户边界、跨实例在途接管、生产 TLS 与规模未验收。本文只写稳定结构与边界。

Peri Swarm 是服务端部署的实现主线，不是生产就绪声明：Rivet actor 接线、会话托管与沙箱编排由本 crate 唯一实现。
早期试验性 TS 适配层（`npm-packages/peri-rivet`、`npm-packages/agent-builder`）已于
2026-11-22 移除，不再作为部署入口。

## 1. 范围与非目标

本地部署把一条 HTTP 调用链固定下来：客户端 → `peri-swarm`（axum 网关 + Rivet
actor）→ 进程内 Peri ACP Host。Hosted 工具经远程 workspace MCP 使用 Bun 托管的
沙箱；Peri 使用 PostgreSQL 会话库，Swarm 使用自己独立的 PostgreSQL 状态库。客户端断连
不终止执行，重连可按序号续读。Rust 宿主退出后的恢复由新输入触发，详见 §7。

非目标：

- 不做跨机调度、租约或 fencing；网关默认只绑定 loopback，非 loopback 绑定（含
  `0.0.0.0`）需要 `PERI_SWARM_BIND_ALLOW_NON_LOOPBACK` 显式开关并输出 WARN；鉴权仍是
  单个部署 Bearer，TLS 必须由该入口终止。
- 副本组共享同一 engine、namespace/pool 与会话库与状态库；多副本不改变本机的恢复
  语义（§7），也不提供跨实例在途 turn 接管。
- 不新增 ACP 方法名或协议版本，不改 Peri 的通用 SQL 执行面。
- 不把 `session/load` 的只读浏览路径变成命令重放入口。
- 不承诺整批输入原子的受理语义（见 §4.4）。
- 不提供生产 TLS、多租户隔离或跨实例接管。

代码入口：`peri-swarm`（crate 根、`src/http`、`src/actor`、`src/store`、
`src/gateway`、`src/sandbox`），运行说明见 [peri-swarm README](../../peri-swarm/README.md)，
沙箱实现见 [Sandbox 设计](../../npm-packages/peri-sandbox/peri-sandbox-design.md)。

## 2. PostgreSQL 会话库与 PostgreSQL 状态库

一个 `peri-swarm` 进程同时拥有两个 PostgreSQL 数据库：Peri 会话库与独立的 Swarm 状态库
（同一实例、不同数据库即可）。没有本地 SQLite 回退，也没有第二种存储引擎。

| 库 | 所有者 | 版本 | 内容 |
| --- | --- | --- | --- |
| Peri 会话库 | Peri 会话存储（`SessionStoreDeployment`） | `peri_store_meta.schema_version=1`，contract `peri.session.postgres/v1` | 线程、消息、MQ、workspace 登记等 Peri 事实；不得出现 `swarm_*` 对象 |
| Swarm 状态库 | `peri-swarm`（`store::postgres::SwarmStateStore`） | `swarm_meta.version=1`，engine `postgres`，contract `peri.swarm.postgres/v1` | `swarm_sessions`、`swarm_items`、`swarm_inputs`、`swarm_bindings`、`swarm_turns` 与索引 |

- 两个 DSN 都是必填且必须指向不同数据库（比较 host/port/database，忽略凭据与 query）；
  同一数据库、缺失 DSN 或任一库打不开都会让进程启动失败，不复用另一库、不降级。
- Peri 会话库在打开时检查 PostgreSQL schema，首次写打开在事务内初始化；未知或更高版本 fail closed。
- Swarm 状态库自带 DDL 与版本判定，初始化在一个 `pg_advisory_xact_lock` 事务里完成：
  - 完全空的 schema：建 6 张表 + 2 个索引并写入 `swarm_meta` 身份行；
  - 有 `swarm_meta`：校验 engine/contract/version，身份行不重写；只重建派生对象（索引），
    缺权威表或列一律 fail closed（重建空表会丢 items、幂等收据、turn 与绑定），列形状用
    `information_schema` 复核；
  - 非空但无 `swarm_meta`：拒绝接管；命中 `peri_store_meta` 时给出「这是 Peri 会话库」
    的专门文案；
  - 版本更高或未知：`SchemaVersion{found,expected}` fail closed。
- 跨引擎（Loom→PostgreSQL）不做自动迁移，也不保留 Loom/Hrana 运行时依赖：割接时停写、
  取旧库只读 SQLite 快照，用一次性离线脚本
  [`peri-swarm/scripts/import_loom_state.py`](../../peri-swarm/scripts/import_loom_state.py)
  导入空的专用库并对账；网关本身只认 PostgreSQL。步骤与限制见
  [peri-swarm README](../../peri-swarm/README.md#升级与回退状态库)。
- 工作区归属仍由 Peri 的远端工作区实现（`RemoteWorkspaceEnvironment`）决定：`cwd`
  必须落在虚拟根内，workspace 登记与准入语义不在 `peri-swarm` 复制一份；相关边界见
  [存储 v2](storage-v2-machine-workspace-session.md)。`peri-swarm` 不新增通用 SQL
  端口，只通过窄的 `SwarmStore` 端口读写 Swarm 事实。
- 每个 `peri-swarm` 进程只有一份会话存储门面/池：bootstrap 打开的部署级业务句柄由所有
  ACP child 共享，部署持有唯一关闭权，child 不关闭共享存储；本地 SQLite 部署仍按 child
  各自打开。见 [1000 并发验收记录](../verification/peri-swarm-1000-concurrent-sessions-2026-11-27.md)。

## 3. 身份

| 身份 | 产生与稳定性 | 归属 |
| --- | --- | --- |
| `logicalId` | 调用方提供 `^[A-Za-z0-9_-]{1,128}$`；省略时服务端生成 `agent-<uuidv7>`；`POST /sessions` 的 `Idempotency-Key` 派生 `agent-<16 hex>`，重试命中同一会话 | 对外会话身份、URL 路径段 |
| `rootSessionId` | 网关生成 UUIDv7，经 `session/new` 的 `_meta["peri.rootSessionId"]` 交给 Peri；宿主在该部署显式开启 `reserved_root_session_id` 时才接受，否则以 `-32602` 拒绝 | Peri ACP 会话与持久历史；恢复时原值复用 |
| Rivet actor key | 名称 `swarm-session`，key `["agent", logicalId]`；`swarm_sessions.actor_key` 持久化为 `agent/<logicalId>` | actor 路由；actor 只信任 key，激活输入只是部署提示 |
| `logicalEnvId` | 调用方经 `environment.environment_id` 复用，或由网关生成 `env_<uuidv7>` | Bun 逻辑环境；同一会话的 sandbox 绑定键 |
| `sandboxId` / `runId` | `sandboxId` 为 `sbx_<32 hex>`，由绑定或 `logicalEnvId` 派生；`runId` 每次拉起都不同 | Bun 进程与持久目录；`runId` 不跨回收复用 |

显式共享在创建事务里核对原绑定的 `sandboxId` 和 `workspacePath`。若最后一个引用已经删除或身份有变，创建失败，防止复活失效的绑定。

绑定关系以 Swarm 状态库的 `swarm_bindings` 为权威，Bun 侧 `bindings.json` 是派生
缓存：只含 `sandboxId`、`workspacePath`、`lastRunId` 与使用时间，绝不保存 token。
恢复时先读状态库绑定，缺失即失败，不静默新建环境；`sandboxId` 或 `workspacePath`
发生变化按 `sandbox_binding_changed` 拒绝。

`logicalId` 与 `rootSessionId` 是两层身份：前者属于调用方并决定 actor 路由，后者
属于 Peri 的持久历史。创建顺序固定为「先落 ID、后建 Peri 会话」：网关先校验或分配
`rootSessionId`（调用方预留优先，否则 UUIDv7；非 UUIDv7 在写库前以 `400` 拒绝），
向 Bun 预留 workspace，在同一个 Swarm 事务内登记绑定和 `state=creating` 的会话行，之后才执行
`session/new`；成功后同一行改为 `idle`。行写入失败即不启动 Peri 会话——没有可寻址的
`logicalId → rootSessionId` 映射，就没有要创建的会话。

进程在两次写入之间退出时，行留在 `creating`，`GET` 如实投影该状态。重试同一
`logicalId` 先按行内 `rootSessionId` 执行 `session/load`；只有当 Peri 会话确实不存在
（上一次死在 `session/new` 之前）才用同一 root id 建会话；`session/new` 遇到已存在
的 id 会拒绝（`-32010`），此时再 attach 一次。两条路都失败时返回
`502 create_recovery_failed`（`details.attach`/`details.spawn` 带契约错误码），行保留
`creating`，可继续重试。恢复路径从不重新生成 root id，也不把「预留 root id」当幂等
写。

## 4. HTTP 契约

### 4.1 方法面与门禁

路由只在 `peri-swarm/src/http` 定义，`/healthz` 是唯一不鉴权的常量探针。

| 方法 | 路径 | 语义 |
| --- | --- | --- |
| `POST` | `/v1/agents/sessions` | 创建或复用会话；`stream:true` 先建立订阅并构造/返回 SSE 响应（初始 `session` 帧），再调度首轮；首轮事件受广播容量约束（§4.3） |
| `GET` | `/v1/agents/sessions` | 官方 `sessions.list`：按 `logicalId` 的 keyset 分页（`after` 排他、`order` 默认 `desc`），可选 `agent_id` 与 `metadata[key]=value` 过滤 |
| `GET` | `/v1/agents/sessions/{logicalId}` | 读取会话投影；`status` 是官方 `idle`/`in_progress`/`failed` |
| `POST` | `/v1/agents/sessions/{logicalId}` | 官方 `sessions.update`：替换 model / MCP tools / metadata / prompt cache；live 运行中返回 `409 turn_in_progress`，有 admitted slot 时详情指向官方 cancel 入口，恢复窗口/无 slot 时改为 `retry_later`（§4.4）；无 live runtime 但持久 turn 未完成时返回 `502`（`details.operation=session_resume_required`） |
| `DELETE` | `/v1/agents/sessions/{logicalId}` | 官方 `sessions.delete`：删除该会话的全部 Swarm 行并失效运行态；与 update 同一准入、`suggest` 判定与 `502` 规则 |
| `POST` | `/v1/agents/sessions/{logicalId}/events` | 提交一条用户输入或控制输入；受理持久化后 `202`。官方信封接受单条纯文本 message、cancel 或 MCP 审批答复；cancel 不创建 turn 或 item |
| `GET` | `/v1/agents/sessions/{logicalId}/events` | SSE：先补 backlog 再转实时；含官方 `agent.session.*` 事件 |
| `GET` | `/v1/agents/sessions/{logicalId}/items` | 按条目边界分页；`data` 是官方 `AgentSessionItem` 投影，`items` 是原始行（§4.2） |
| `GET` | `/v1/agents/sessions/{logicalId}/turns` | 官方 `sessions.turns.list`：按 `(created_at, turn_id)` 的 keyset 分页，`order` 默认 `desc` |
| `GET` | `/v1/agents/sessions/{logicalId}/turns/{turnId}` | 官方 `sessions.turns.retrieve`；turn 不属于该会话时 `404 unknown_agent` |
| `GET` | `.../turns/{turnId}/items`、`.../subagents/{id}/items`、`.../subagents/{id}/turns[/{turnId}][/items]`、`.../traces`、`.../artifacts[/{id}][/content]`、`.../subagents[/{id}]` | 无法映射的官方资源；注册路由并以网关 JSON 错误返回 `404 unsupported_contract`，消息指出资源名 |

约定：

- `Authorization: Bearer <部署 key>` 常量时间比较；失败 `401` 并带
  `WWW-Authenticate: Bearer`。
- `OpenAI-Beta: agents=v1` 可选，其他值 `400 unsupported_beta`；请求可用
  `x-peri-swarm-expect: 1` 探测版本，所有响应回写 `x-peri-swarm-contract: 1`。
- 请求体上限 8 MiB；`text` 1 MiB、`instructions` 64 KiB、`model` 256 B 由显式检查
  返回 `400`。错误体使用统一信封与稳定 `code`（`invalid_input`、`input_conflict`、
  `session_resume_required`、`upstream_unavailable` 等）。
- `POST /events` 的官方 `{events:[...]}` 信封只接受单条事件：`agent.session.input.message`
  要求一条 `role:"user"` 的消息及纯 `input_text` content，按 `kind=user, mode=turn` 受理；
  `agent.session.input.cancel` 只带 `type`，按 `kind=control, mode=turn, text=cancel` 受理；
  MCP 审批答复见 §4.2。批量、与原生字段混用、`tool_result`、
  `computer_use_approval_request_result`、`input_image` 等不支持的输入都返回
  `400 invalid_input`，`details.field` 指向具体字段，不静默忽略。
- `cwd` 缺省为 `<workspaceRoot>/<logicalId>`；显式 `cwd` 必须存在且规范化，落在
  虚拟 workspace 根内。
- `sessions.list` 的排序键是公开会话 id `logicalId`（唯一、稳定全序），不是
  `createdAt`；同一秒创建的会话不会因时钟并列而乱序。`after` 是上一页最后一项的
  `id`，在所选顺序上排他；`limit` 是会话条数上限（默认 20，范围 1..=500），不做
  行预算，`has_more` 基于**过滤后**的结果。响应是 `{object:"list", data, has_more,
  hasMore, nextCursor}`，官方 `CursorPage` 可用最后一项的 `id` 自动翻页。
  `metadata[k]=v` 最多 16 对、key 1..=64 字符、value ≤512 字符，全部精确匹配；
  `cursor` 等未知查询参数、越界 limit 与畸形 order 都是 `400 invalid_query`。
- `agent_id` 过滤按网关唯一的根 agent 约定：所有会话的根 agent 都是 `root`
  （与官方 turn/session 帧的 `agent.id` 相同），因此 `agent_id=root` 返回全部，
  其他值返回空页而不是静默忽略。
- `sessions.update` 以官方 `SessionUpdateParams` 为输入：`agent.model` 写 `model`
  （官方 model 是 string；显式 `null` 返回 `400 invalid_request`，
  `details.field="agent.model"`），`agent.tools` 走与 create 相同的 MCP 规范化
  （`null`/`[]` 清空），`metadata` 整体替换（`null` 或 `{}` 清空），
  `prompt_cache_options` 整体替换（`null` 清空，`{}` 使用官方默认 `30m`）。省略字段保持不变。`review`、
  `agent.reasoning`、`agent.service_tier` 没有本地映射，显式 `400 invalid_request`
  （`details.field` 指向官方字段），不静默接受。model 或 MCP tools 变化会停止当前
  ACP host；下一次交互按新配置重新 attach，避免返回一个下一轮不会使用的旧配置。
- 官方 `update(sessionID, body?: SessionUpdateParams | null | undefined)`
  （`sessions.d.ts:76`）允许省略 body：官方 SDK 省略时发送 `{}`（`application/json`），
  只有显式 `null` 才不发 body 也不发 content-type。网关把「无 body」（含无
  content-type、`Content-Length: 0`）与 JSON `null` 都视为 **空更新**：返回当前
  `SessionView`，不写任何行（含 `last_activity_at`），也不执行在途/恢复准入判定；
  省略 body 时的 `{}` 同样不写行。只有非空 body 才按上一条解析：非 JSON content-type
  返回 `415 invalid_content_type`，JSON 语法错误返回 `400 invalid_request`；
  两者都不会被降级成 no-op。
- `sessions.delete` 在 `swarm_sessions`、`swarm_items`、`swarm_inputs`、
  `swarm_turns` 上清除该会话的行。共享环境的绑定转交给仍引用它的会话，最后一个引用删除后才移除绑定。PostgreSQL 实现先按会话的 hosted 环境对 binding 行取 `FOR UPDATE`（同一环境的并发删除因此串行），再把六条语句放在同一个写事务中，并清理指向已删除会话的悬挂 binding；内存实现使用同一把锁。事务提交后网关停止 ACP host、移除进程内 channel，Rivet
  actor 随后销毁，因此用同一 `logicalId` 重新 create 会得到新 `rootSessionId`、
  空 items 与空 turns。删除失败不返回成功，行仍可重试。
- 官方 `sessions.turns.list` 按 `(created_at, turn_id)` 稳定全序分页；`after` 是
  turn id 且排他，未知 turn id 返回 `400 invalid_query`。`turns.retrieve` 的
  `usage` 与 `subagent_id` 恒为 `null`（网关不做 token 记账，也没有 subagent
  turn），`agent_id` 恒为 `root`。


### 4.2 SSE 与 items

- 每个 item **至多持久化为一行**，`seq` 由 Swarm 状态库单调分配；先落库后广播，订阅者
  不存在也不影响执行。assistant 文本流在实时段逐 chunk 广播：自定义 `delta` 帧之外，
  每个 chunk 还发送官方 `agent.session.turn.output_text.delta`；首 chunk 先补
  `item.added`/`content_part.added`。实时帧不会晚于该流的完成行；若引入帧没有发给订阅
  者，完成行补发引入帧。实时帧都没有 `seq`、不落库。turn 终止时把整条流写成一条
  `complete:true` 合并行；回放没有原始 chunk 边界，只从该完成行合成
  **一条全文** `output_text.delta`。推理流仍只有自定义 `delta` 帧，不发官方
  `reasoning_summary_text.delta`。`tool_call`、`tool_result`、`approval`、user/receipt
  每帧一行。历史数据仍可能是「每 chunk 一行 + complete 行」的多行 item（§4.2.2），
  新写入不再产生该形状。
- 用户输入在 `swarm_inputs` 受理（`disposition=inserted`）后写成一条单帧
  `message` item：`payload={role:"user",text,inputId}`，`id`/`itemId` 为
  `itim_input_<inputId>`，没有 `delta`/`complete` 字段。host 的
  `user_message_chunk` 回显不投影，因此同一 `inputId` 重试不会重复；`create` 的
  `input` 同样写一条（每次 create 调用各一条；对已 `idle` 会话的重试 create 仍按
  既有语义重跑该轮，`creating` 行的恢复不重放输入，见 §4.4）。
  官方 `events` 信封同样只落这一条 user message item（多段 `input_text` 按顺序
  以换行拼接成一个 `text`）。
  assistant 实时帧仍按 `payload.messageId` 聚合；turn 终止时同一
  `(agentId, messageId, kind)` 只写一条
  `payload={role:"assistant",text,complete:true,phase,outputIndex}` 合并行。流键含
  `kind`，所以 answer 与 reasoning 共享 `messageId` 时是两条 item（各自稳定
  `itemId`），不是一条。
- item 投影同时给出 `id` 与 `itemId`（同值，`itim_...`）；`seq` 仍是唯一分页与续读
  游标。新写入的数据每个 `itemId` 只有一条 `complete` 行，回放不会重复；历史多行
  item 在回放中仍按 `seq` 逐行发出 `item` 帧，客户端需按 `itemId` 自行折叠
  （§4.2.2）。实时 `delta` 帧与官方 `output_text.delta` 同合并行共用 `itemId`，但都
  没有 `seq`，不能作为游标，也不参与 `id` 去重。
- SSE 帧只有**持久化 item** 带 `id`（等于 `seq`）；实时 `delta`、`turn`、官方
  `agent.session.*` 与 `gap` 不持久化，不推进 `Last-Event-ID`。实时段的 `turn` 先发
  `started`，终止帧是 `completed` 或 `failed`；官方帧是增量兼容层，既有 `item`/`turn`
  帧保持不变。
- 订阅游标接受 `Last-Event-ID` 头、`?afterSeq=` 或 `?after=`（三者等价，同时给出
  必须一致，否则 `400 conflicting_cursor`）。查询还接受官方
  `include_obfuscation`（`true`/`false`）：网关不生成混淆填充，值被接受并忽略，
  不改变实时或回放帧。**没有任何游标时从当前末尾开始，只
  投递实时事件**：官方 `sessions.stream` 先订阅再 POST，若回放旧 turn 的终态，
  SDK 会在旧 turn 的 `idle` 上结束。显式 `afterSeq=0` 仍回放全部（既有客户端语义
  不变）。无游标订阅不读会话行：`POST /sessions?stream=true` 在行尚未创建时先
  打开订阅，行检查只属于显式游标（未知会话仍是 `404 unknown_agent`），否则流式
  create 会在行尚不存在时得到已关闭的订阅。
- 带游标时先补 `seq > after` 的 backlog，再转实时广播。心跳间隔由
  `PERI_SWARM_SSE_HEARTBEAT_MS` 控制。心跳 payload 的 `lastItemSeq` 是该会话当前
  `MAX(seq)`（与 item 写入方共享），只由持久化 item 推进——长 turn 期间若只产生实时
  delta，它可能整轮不变。它只表示进度，不能代替已投递 item 的 `seq` 作为恢复游标。
- 回放（backlog）与实时同序：先发该行的 `item` 帧，紧接由同一行推导的官方帧
  （§4.2.1）；上一 turn 的终态先于下一 turn 的 `turn.created`。因此重连看到的官方
  序列与实时一致；`turn` 控制帧仍只出现在实时段。客户端的回放终止条件是目标
  message 的 `payload.complete=true`。
- 回放边界由 lookback 决定：窗口首行不是 turn opener（assistant/tool 帧），或首行
  是 steer user item 时，服务端向游标之前回看，找到仍打开该 turn 的 opener（user
  item），用它的 `turn_id` 播种；不重发 `turn.created`（重发会让新订阅的
  `TurnState` 选中旧 turn 并在其终态结束）。窗口为空且会话行已 `idle`/`failed`
  时，从最后一批 item 推导最后一个 turn 并补发终态（`turn.completed`+`idle` 或
  `turn.failed`+`session.failed`）；该 turn 此刻仍在实时执行（存在在途 turn）时
  不补，终态由实时流给出。只有 tool 帧、没有 assistant 完成帧的 turn 由同一
  lookback 播种，并按「已闭合且无输出 → `completed`」补终态。
- lookback 的已知边界：它按「打开的 turn 已落 assistant 完成帧则视为已闭合」的
  行边界规则推断。真实 steer 发生在该 turn 已有 assistant 完成帧之后时，实时流
  把新输入挂到原 turn，回放却会把它当作新 turn（多一个 `turn.created`）。行里
  没有 turn 终态标记，无法消除；游标落在 steer 之前且该 turn 尚无完成帧的常见
  情况已由 lookback 修复。
- turn 内未 flush 的 assistant/thought 流在 turn 终止时一次写入状态库；进程在
  turn 执行中崩溃会丢失该 turn 尚未 flush 的文本（已落库的 user/tool/approval 行不受
  影响），恢复只按行重放，不逐 chunk 重播增量；assistant 完成行在回放时合成一条全文
  delta。完成组按存储批次提交：失败批次不广播，
  已提交批次先广播；任何未持久化的完成组都会让非取消 turn 以 `turn.failed` 收尾
  （不静默成功）。取消 turn 的完成行落库失败只记录日志，终态仍是
  `turn.completed(status="cancelled")`，会话回到 `idle`。合并 payload 超过
  `MAX_PAYLOAD_BYTES`（1 MiB）时按 `{_truncated:true, originalBytes}` 存储并置
  `truncated:true`，`seq`/`itemId` 不变；实时 delta 不受该截断影响。
- 恢复期写入的 root 文本没有 gateway turn：合并行带 `payload.recovered=true`，且不含
  `phase`/`outputIndex`。回放把它当自定义 item——只发 `item` 帧，不产生任何
  `turn.item.*`/`content_part.*`，也不归属前后任何已结束 turn；实时侧同样不发官方帧。
- 慢订阅者：广播容量耗尽（Lagged）时发一帧 `gap` 后关闭该订阅。丢失的事件可能
  包含终态，官方 SDK 因而以 EOF 识别中断；原生客户端用最后一个已渲染 item 的
  `seq` 重连恢复。`gap.toSeq` 是**被丢弃持久化行的 `seq` 上界**（按本流最后已投递
  item 加上丢弃帧数推算），可能高于会话当前游标：不落库的 delta 不占 `seq`，因此
  不能按差值推算丢弃行数。流式 create 的首轮走同一通道：订阅先于首轮建立，容量内的首轮
  事件实时送达；首轮突发或客户端读取过慢超过容量时同样 `gap` + EOF，不静默丢弃
  （§4.3）。
- MCP 审批使用同一 item 游标：`kind=approval` 的行记录 `state=pending|settled`、
  `requestId`（公开不透明 ID `appr_<uuidv7>`）、`turnId`、`serverLabel`、`name`、
  `arguments` 与结算 `decision`/`reason`。pending 行落库后广播
  `agent.session.requires_action`（`event_id=evt_item_<seq>_requires_action`，只含
  当前有效待办）；结算行再次发出同类型快照，剩余集合为空即表示“无待办”。公开
  投影与官方 `openai@7.60.0` 一致：每条为
  `{type:"mcp_approval_request",request_id,turn_id,request:{type:"mcp_tool_call",
  server_label,name,arguments,message}}`；`request_id`/`turn_id` 由待办事实派生，
  `name`/`arguments` 缺失分别为 `null`，`message` 只描述待审批动作，不伪造工具输出。
  来源无法归属时 `server_label` 为 `""`：官方该字段是不可空的 `string`，空串是唯一
  可投影值，语义是“来源未知”，`message` 明确写 “could not be determined”；host
  给出空工具名时按缺失处理，投影 `name: null`（不造 `name: ""`）。
  重连补 backlog 时按行折叠：pending 加入、settled 移除，已结算动作不会复活。
  订阅建立后还会补一帧当前快照（同一 `event_id`，按 id 幂等），覆盖 opener 在游标
  之前的情况。ACP `subscribe` 与生产 actor 的 `GET /events` 共用同一个
  backlog 装配：先结算已过期的待办，再读游标之后的回放窗口，最后追加当前快照；
  两条路径不再各自实现。审批不产生官方 item，也不终结 turn。
- 答复 body 接受官方
  `{type:"agent.session.input.mcp_approval_request_result",request_id,
  response:{type:"mcp_tool_call",decision}}`；`response.type` 必须是
  `mcp_tool_call`，`decision ∈ approve|deny|cancel`。顶层 `decision` 与
  `requestId`/`approval_request_id` 只作为原生客户端别名保留，官方形状不会 400。
  错误 details 指向具体字段（`events[0].response.type`、
  `events[0].response.decision` 等）。
- 答复采用两阶段结算：先认领待办并送 ACP，成功后才写 `settled` 并返回成功回执；
  送达失败释放认领、待办仍可答复并返回 `502 upstream_unavailable`。并发答复只有
  一个认领成功，其余得到“正在送达”的冲突，不会在送达前删除待办。结算行持久化
  **先于**内存待办删除：存储失败时返回 `503 required_action_settlement_failed`
  （`details.requestId`），释放认领并保留待答复事实；不会出现“回执成功但耐久日志
  仍 pending”或“回执失败但内存已删除”。
- 硬崩溃恢复：actor 重建时扫描 `state=pending` 的 approval 行，追加结算行
  （turn 仍打开为 `actor_lost`，turn 已结束为 `turn_ended`）；回放、retrieve 与
  list 因此不再投影旧代次无法答复的动作。
- SSE 顺序：工具调用帧（若有）→ `requires_action` → 答复后工具结果帧 →
  `turn.completed` → `session.idle`。结算快照先于终态发布；`deny`/`cancel` 只结算
  那一次工具调用，turn 仍由 host 结果收尾。
- `GET /items` 按 **item 分组**分页（§4.2.2）。`items` 是当页选中条目的全部原始行，
  包含 `id`/`itemId`、`seq`、`kind`、`source` 和 `payload`。新数据每 item 一行，
  只有历史数据可能包含逐 chunk delta 行；新写入的实时增量走 SSE。回放没有原始 chunk
  行，SSE 对每条 assistant 完成行合成一条全文 `output_text.delta`（§4.2.1），
  `/items` 的条目投影与文本折叠规则不变。`data` 是官方 `AgentSessionItem` 投影，有
  `type`、条目 `id`（等于 `itemId`）、`turn_id`，没有 `seq`。同一 `itemId` 投影至多
  一次；`data` 来自扫描到的首个有投影的存储页，其条目都在本次响应的 `items` 中（§4.2.2）。

| 持久条目 | `data` 投影 |
| --- | --- |
| 根会话用户消息 | `message`，`content` 为 `input_text`，`phase:null`，`status:completed` |
| 根会话助手消息 | `message`，`content` 为 `output_text`；有 `complete:true` 时取**首条**完成帧的 `text`，只追加其后（`seq` 更大）**delta** 帧的 `text`；后续完成帧只是该代 delta 的累计文本，不再追加（否则两代会拼成 `hellolo`）。`status:completed`，缺少合法 `phase` 时用 `final_answer`。无完成帧时按 `seq` 拼接历史 delta（单帧直接取文本），缺少合法 `phase` 时为 `null`；所归属 turn 已终止时为 `incomplete`，否则为 `in_progress` |
| `thought` | `reasoning`，`summary` 为 `summary_text`，`status:null`；有完成帧时与助手同一规则（首条完成帧 + 其后 delta），否则拼接历史 delta；空文本不投影。`summary_text` 承载 Peri 的**原始** thought 文本，不是官方语义的推理摘要 |
| `tool_call` / `tool_result` | `function_call` / `function_call_output`，以 `toolCallId` 作为 `call_id` 跨页关联。`title` 用作 `name`（依赖宿主把工具名写进 ACP `title`；`projection.rs` 只读该字段），`input` 或 `body` 用作 `arguments`；失败文本进 `error`，其他输出进 `output`（非字符串序列化为 JSON 文本）。同页输出状态可覆盖调用状态；无同页输出时，所属 turn 已终止（`completed`/`cancelled`/`failed`）为 `incomplete`，仍在运行为 `in_progress`，跨页仍不回填——客户端应结合同 `call_id` 的 `function_call_output` 判断真实结果。不发 `mcp_call` |

`receipt`、`approval`、`tool_update`、子 agent 条目、所需帧已截断的条目、缺少工具标识
（或调用名称）、恢复期根输出（`payload.recovered=true`）以及无法确定 turn 的条目只保留在
`items`。恢复行是真实持久行，但不属于任何 gateway turn：
它既不进入 `data`，也不开启/关闭 turn，不置“已有输出”。turn 归属与 SSE 回放共用开
turn 规则：回扫页首最小 `seq` 之前的历史取种子，跨过未入页的原始行时补查其边界。
首条用户消息、已出现助手完成帧后到达的用户消息、或有对应持久 turn 行的用户消息开启
turn；steer 沿用 opener 的 `turn_id`。可识别的边界（payload 未被截断）不因该条目未进入
`data` 而失效；真实截断行的 payload 只剩 `{_truncated, originalBytes}`，边界信息已丢失，
只能依赖持久 turn 行。原始组的归属不受投影影响，响应页边界见 §4.2.2。

页内 `data` 的顺序取自条目在该页的原始顺序（即 item 的 `MAX(seq)`），不是所选完成帧的
`seq`；迟到 delta 让两者不一致时，按 `MAX(seq)` 的顺序稳定，正文仍按上文规则折叠。

页内 `seq` 缺口需要回扫历史确认 turn 归属。相同缺口边界只回扫一次；每个参与投影的
原始页最多发起 8 次 `turn_at_cursor` 回扫，每次最多读 10 页原始行。达到任一上限仍需
回扫时，该组及其后的组只在 `items`，不把未验证的 turn 归属写进 `data`。遇到缺口过多
时可减小 `limit` 或按 `afterSeq` 分段读取；SSE 回放的历史查找仍不截断。

`ItemPage` 同时是 HTTP body 和 actor `ListItems` action 的 Rivet wire 类型（`data` 从原始
行改为官方投影是跨版本硬契约变化）。升级必须把 actor 与 gateway 作为同一版本整体重启；
混合版本时网关反序列化旧 `data` 失败，`/items` 返回 `502 upstream_unavailable`
（`details.operation=swarm_actor`）直到 actor 重启。

#### 4.2.2 item 级分页

**多行 item 是历史数据形状**：早期数据把每个 assistant delta 写成一行，`delta` 与
`complete` 共享 `itemId`、`seq` 各不相同；新写入每 item 至多一行，但 `data` 仍只包含
可映射的条目。以下多行规则只为历史数据保留，`after=<itemId>` 不能按行理解：

- item 归属按该 item 的最大 `seq` 判定，分页单位是 item；升序页是
  `MAX(seq) > after` 的前若干个 item，降序页是 `MAX(seq) < after` 的前若干个
  item。`limit=1` 且首条消息有 3 帧时，`items` 返回 3 行、可映射时 `data` 有 1 项。
- 分组在存储层完成：PostgreSQL 用 `GROUP BY logical_id, item_id` + `MAX(seq)` 选页，
  再按 `item_id` join 回全部行；内存 fake 用同一 `grouped_item_page` 规则，两者
  不会给出不同的页边界。
- **单次存储页行预算**：`limit` 同时是 item 数上限和聚合行预算。按 item 顺序累加原始
  行数，达到或超过 `limit` 行后不再纳入下一个 item；跨过行预算的组也必须完整读取，
  不截断。响应 `items` 跟随可投影条目的页边界：逐页读取并各投影一次，首个非空 `data`
  即返回，不再探页；如果一直无投影，读到确证的末尾才返回 `data=[]`（`has_more=false`）。
  `items` 包含所有纳入的组，按 `seq` 随 `order` 排序，行数可以超过 `limit`；`data` 按
  item id 去重且最多 `limit` 项。`has_more`/`hasMore` 和 `nextCursor` 描述最后纳入的存储页。
- **请求级预算**：含首个存储页最多读取 100 页（`MAX_ITEM_PAGE_READS`）。2000 条
  （`MAX_SELECTED_RAW_ROWS`）是**纳入**上限，不是读取上限：PostgreSQL 会先取 `limit+1`
  个候选组的全部帧，再丢弃未纳入的组；历史回扫读取的行不计入这 2000 条。纳入按 `seq`
  保持集合语义，组因并发追加而后移并被再次选中时，已纳入的帧不重复计数。
  若仍需续查但纳入预算不足，或探页游标无法严格推进，返回 `422 items_scan_limit`，不返回
  部分页或游标。上游读失败一律返回 `502 upstream_unavailable`，不降级为成功空页。
- **升序观察上界**：探页遇到下一页游标 `>= lastItemSeq` 时停止，返回已累积的页而不是错误。
  `lastItemSeq` 是请求开始时的观察上界；首读之后追加的条目可能属于并发写入，升序不追尾，
  但首读本身仍可能包含越过该值的并发帧。降序不受此限。守卫命中时仍返回 `200`：形状为
  `data=[]`、`has_more=true`、`nextCursor` 为本页原始游标（探到的下一页游标达到或越过
  `lastItemSeq`，且该页没有可投影条目）。原生客户端可沿该游标继续；SDK 因空 `data` 停止，
  因此在该边界可能比原生游标遍历少看到条目，但不是错误。
- `nextCursor` 是页内最后一个 item 的最大 `seq`，仅在还有条目时给出。升序下一页严格
  前进；降序页的游标是页内最旧组的最大 `seq`，下一页只取更小的组。`after=<itemId>`
  解析为该 item 的最大 `seq`。十进制 `after`/`afterSeq` 同样按 item 边界解释：游标
  落在多帧 item 中间时，要么按组最大 `seq` 选入整个 item（含游标之前的帧），要么
  整组排除。畸形或未知 item id 返回 `400 invalid_query`，与 `afterSeq` 不一致返回
  `400 conflicting_cursor`。
- 官方 SDK 以 `data` 末项 `id` 自动翻页。只有在**每次续页都正常返回非空 `data`**、
  且**已返回的 item 不再增长**时，它才与 `nextCursor` 覆盖同一批可投影条目；此时 SDK
  续页会从该 `id` 之后重扫内部原始行，因此在 100 页/2000 行预算边界上，SDK 可能比原生
  `nextCursor` 更早触发 `items_scan_limit`。命中升序观察上界而 `data=[]` 时同样产生分叉，
  item 未增长也不例外：例如已返回 `seq=1`，续页的分页边界停在 `seq=2` 的内部条目并
  命中上界（`data=[]`、游标 2），并发写入的 `seq=3` 可投影条目对 SDK 不可见，
  原生从游标 2 继续则能读到。
  已返回的 item 若又追加了帧，`after=<itemId>` 按请求时该 item 的**当前** `MAX(seq)` 解析，
  可能越过中间尚未读到的可投影 item：例如 SDK 末项 `seq=2`、`nextCursor=3`、`seq=4`
  有可投影条目，末项增长到 `seq=5` 后 `after=<itemId>` 解析为 5，跳过了 `seq=4`，而数字
  游标 `nextCursor=3` 仍会包含它。两者差异不只在重扫成本或扫描预算。升序响应也可能包含
  `lastItemSeq` 之后的并发追加帧。完整原始行遍历应使用 `nextCursor`/`afterSeq` 并读
  `items`，不要用 `data` 的末项或是否为空决定原始分页。
- **增长中的 item**：`after=<itemId>` 在请求时解析为该 item 的当前 `MAX(seq)`；
  之后追加的帧都落在这个 `seq` 之后，不会再由该游标返回（其他最大 `seq` 低于该值
  的 item 也一并被越过）。需要按最后已渲染行续读时用 `afterSeq=<seq>`：它同样按
  item 边界判定，会把包含该行的 item 整条重新给出（含新追加的帧），因此能看到
  最终帧。分页游标始终严格前进、不会循环。
- `order` 默认 `desc`，与官方 `ItemListParams.order` 一致；需要升序时显式
  `order=asc`。分页游标 `after`/`afterSeq` 在所选顺序下排他。

| HTTP | `/items` 错误码 | 含义 |
| --- | --- | --- |
| 422 | `items_scan_limit` | 扫描预算耗尽或游标不能推进；恢复方式取决于成因：目标 item 之前累计 2000 条内部行时，用 `afterSeq`（取这些行的最大 `seq`）或 `after=<内部 item id>` 移到它们之后、目标 item 之前再读，不要越过目标 item；单个 item 自身超 2000 条时只能跳过该 item 再读（存储层整组返回、无法纳入，其帧不会出现在响应里，减小 `limit` 无效）；100 页读取预算触顶时减小 `limit` 只会更快耗尽，应改用更近的游标；游标不能严格推进时重试或改用 `afterSeq`。 |
| 502 | `upstream_unavailable` | 存储页或投影依赖读取失败；`details.operation` 指明操作，不返回部分页。 |

#### 4.2.1 官方事件映射

官方 `openai@7.60.0` Agents API 的事件帧是附加层：SSE `event:` 名与 JSON `type`
相同，均为 `agent.session.*`；既有 `item`/`turn` 客户端不受影响。

| 官方事件 | 发出时机 | 载荷来源 | `event_id` |
| --- | --- | --- | --- |
| `agent.session.created` | **仅** `POST /sessions?stream=true` 的流式 create 响应，作为第一个官方帧 | create 结果的 `AgentSession` 投影（与 `idle`/`failed` 同为 `OfficialSession`） | `evt_<sessionId>_session_created` |
| `agent.session.in_progress` | turn 真正开始，**先于** `turn.created` | 会话行快照（`in_progress`） | `evt_<turnId>_session_in_progress` |
| `agent.session.turn.created` | turn 真正开始（`begin_turn`），**不是**受理时 | 打开 turn 的 user item | `evt_<turnId>_created` |
| `agent.session.turn.in_progress` | 紧跟 `turn.created` | 同一 turn 快照（`in_progress`） | `evt_<turnId>_in_progress` |
| `agent.session.turn.item.added` | user item 开启 turn/steer 时；assistant 实时首 chunk，或回放每条完成行 | user：持久化 `seq`、`itemId`、text；assistant：稳定 `itemId` 与首 chunk 文本 | user `evt_item_<seq>_added`；assistant `evt_item_<itemId>_added` |
| `agent.session.turn.content_part.added` | assistant 首 chunk（实时）或完成行（回放），紧跟 `item.added` | 首 chunk 文本 / 完成行全文、`outputIndex`；`content_index=0` | `evt_item_<itemId>_part_added` |
| `agent.session.turn.output_text.delta` | 实时每个 chunk；回放每条 assistant 完成行**一条全文** | 该 chunk 文本 / 完成行全文；`content_index=0`，`outputIndex` 与合并行一致 | 实时 `evt_item_<itemId>_delta_<chunk>`；回放 `evt_item_<seq>_text_delta`；**两者不保证一致** |
| `agent.session.turn.output_text.done` | assistant 完成行落库后 | 完成行全文 | `evt_item_<seq>_text_done` |
| `agent.session.turn.content_part.done` | 同上 | 同上 | `evt_item_<seq>_part_done` |
| `agent.session.turn.item.done` | 同上 | 同一完成 item（`phase` 来自 payload） | `evt_item_<seq>_done` |
| `agent.session.turn.completed` | 成功终态 | turn 的 `created_at` 与完成时刻 | `evt_<turnId>_completed` |
| `agent.session.turn.failed` | 失败终态 | 同上 | `evt_<turnId>_failed` |
| `error` | 失败 turn：`turn.failed` 之后、`session.failed` 之前 | `SessionError{code:"internal_error",type:"internal_error",message}` | `evt_<turnId>_error` |
| `agent.session.idle` | **紧接** `turn.completed` 之后 | 会话行快照 | `evt_<turnId>_idle` |
| `agent.session.failed` | **紧接** `error` 之后 | 会话行快照 + 错误 | `evt_<turnId>_session_failed` |

- assistant 的 `item.added`/`content_part.added` 在实时首 chunk 就发出，完成行落库后
  只补 `output_text.done` → `content_part.done` → `item.done`，不再重复 added；回放没有
  原始 chunk，仍从完成行按 `item.added` → `content_part.added` → 全文
  `output_text.delta` → done 发出。没有实时 chunk 的完成行（如内存 fake 或 chunk 全被
  drop）保持 added 开头的原序列。`Thought`、child 行与 `recovered` 行不产生官方文本帧；
  历史多行 item 只在完成行上投影（§4.2.2），新数据同一 item 只有一条完成行。
- `turnId = turn_<打开 turn 的 user itemId 去掉 itim_ 前缀>`。它由已持久化的 user
  item 派生，不用 ACP `messageId`，不含时钟或随机源；重连后由同一行重算，值不变。
- `event_id` 由 `turnId`、`sessionId`、持久化 item `seq` 或 assistant 的稳定 `itemId`
  派生。user `item.added`、done 系列与 turn/session 帧在实时投递与 backlog 重放中
  相同；assistant 的 `item.added`/`content_part.added` 也因按 `itemId` 派生而相同。
  唯一例外是 `output_text.delta`：实时用进程内 chunk 序号，回放用完成行 `seq`，两者
  **不保证一致**，这是不持久化 chunk 的直接结果；跨重连按 `event_id` 去重时必须把
  delta 排除，或接受同一文本的两次 delta 事件。
- 受理只持久化 user item，不发 `turn.created`/`turn.item.added`。turn 真正开始时
  官方帧按 `session.in_progress` → `turn.created` → `turn.in_progress` →
  `turn.item.added`（user）发出，随后是自定义 `turn: started`；从未执行的 turn 不会
  留下只有 created 没有终态的悬空 turn。实时与回放使用同一顺序。
- 实时 assistant 有 chunk 时，官方序列是首 chunk 的 `turn.item.added` →
  `content_part.added` → 每个 chunk 的 `output_text.delta`，turn 终止后的
  `output_text.done` → `content_part.done` → `turn.item.done`。回放每条 assistant 完成
  行的序列是 `turn.item.added` → `content_part.added` → 一条全文
  `output_text.delta` → `output_text.done` → `content_part.done` → `turn.item.done`。
  `item.done.item` 是 `AgentSessionAssistantMessage`（`status:"completed"`，`content`
  为单个 `output_text`），`content_index` 恒为 `0`。
- 同一模型调用的 reasoning 与 answer 共享 `messageId`。流键是
  `(agentId, messageId, kind)`，`Message` 与 `Thought` 分开累积、各自拥有稳定
  `itemId`；自定义 delta 只走实时，turn 终止时各自写一条合并行，reasoning 先到不会把
  answer 并进 Thought。reasoning 官方帧本轮**未实现**：`Thought` 只投影为自定义
  `item`（`reasoning.summary`），没有 `reasoning_summary_text.delta`/done。
- assistant `phase`/`outputIndex` **只对 root 的 `Message` 完成行计算**：同一 turn 内
  按**首次到达顺序**（首次打开该流时的 `MessageStream.order`）排序，最后一条 root
  Message 是 `final_answer`，其余是
  `commentary`。`finish_turn` 把 `phase` 和 `outputIndex` 写进该完成 item 的 payload；
  实时帧与回放都只读 payload，不维护第二套推导。`output_index` 是 turn 内 root
  assistant 输出消息序号（从 0 开始），user item、`Thought` 与 child 行不参与。
  `Thought`/child 完成行照常持久化，但不带 `phase`/`outputIndex`。旧 payload 没有
  这两个键时，`phase` 回放为 `final_answer`，`outputIndex` 按该 turn 内完成行顺序
  推导；旧记录仍可回放。
- 官方 item 帧只投影 **root** assistant `Message`（以及 root user item）：
  `official_frames` 先检查 `source.agent_id == "root"`，`projection` 里的
  `child:<id>` 行、`Thought`、tool/receipt 帧都只留在网关自定义 `item` 面。
  `turn.item.added` 因此不会出现 child 或 reasoning item；`turn.item.added.output_index`
  仍为 `null`，完成序号只由 `item.done` 承载。回放按持久化的 `source.agent_id`
  做同一过滤，child 行不开启/关闭 turn，也不消耗 `outputIndex`。
- backlog 按行重建 turn 边界：user item 在「没有打开的 turn」或「打开的 turn 已落
  assistant 完成帧」时开启新 turn，否则视为该 turn 的 steer；终态在下一 turn 的
  `turn.created` 之前或回放末尾发出。最后一个 turn 仅在会话行状态仍为 `failed`
  时补 `turn.failed` + `error` + `agent.session.failed`。回放仍只从 item 行重建，
  不读取 `swarm_turns`：因此重放的失败 `error` 与 `completed_at` 保持 `null`，不把
  turn 行的事实混进既有帧；实时失败帧的 `turn.error` 由失败消息填充
  （`{code:"internal_error", message}`）。`error.error.code`/`type` 固定为
  `internal_error`（持久 turn 行只写这一分类）；实时帧的 `message` 是真实失败文本，
  回放没有持久文本时留空字符串而不是编造。`turns.list`/`turns.retrieve` 读的是
  持久 turn 行，失败 turn 在那里带 `error` 与 `completed_at`。若该 turn 此刻仍在
  实时执行，回放不补终态，终态由实时流给出，避免 SDK 提前结束。
- 重连游标落在 turn 中间时，lookback 从游标（含）之前回看：跳过 steer user item，
  直到找到一个未被 assistant 完成帧与更早 user item 分隔的 opener，取其 `turn_id`
  播种，`turn.item.added` 仍带同一 `turn_id`，不重复 `turn.created`。首行是 steer
  时，实时与回放给出同一 `turn_id`。只有 tool 帧的窗口同样由 lookback 播种：能判定
  已闭合（会话行 `idle`）时按「无输出 → `completed`」补终态；会话行 `failed` 时按
  `turn.failed`。
- `afterSeq` 等于会话末尾时窗口为空：会话行 `idle`/`failed` 时仍按最后一批 item
  补发该 turn 的终态与 `agent.session.idle`/`agent.session.failed`（冗余终态按
  `event_id` 幂等）；有在途 turn 时不补。
- SSE 回放仍不读 `swarm_turns`：会话行只保留最新状态，更早的失败 turn 在回放中
  显示为 `completed`。这是回放层的既有边界，`turns.list`/`turns.retrieve` 不受
  影响（它们以 turn 行为权威）。
- turn 生命周期同时持久化到 `swarm_turns`：`begin_turn` 写 `in_progress`（`created_at`
  是打开 turn 的 Unix 秒，`started_at` 是执行开始时刻），`finish_turn` 写
  `completed`/`cancelled`/`failed` 与 `completed_at`，失败还写
  `error_json={code:"internal_error", message}`。取消路径的 `turn.completed` 帧内
  turn 的官方 `status` 是 `cancelled`（`Turn.status` 的官方取值），持久行与
  `turns.list`/`turns.retrieve` 使用同一状态。受理但未执行
  （`abandon_pending_turn`）不写行：它与「不发 `turn.created`」一致，因此不会留下
  `queued`/`in_progress` 的 phantom 行。
  非 steer 的 `session/prompt` 回包只表示 agent 接受了输入，不代表事件流已终止：
  网关在回包后等待该 turn 的终止事件（`wait_turn_closed`，1800 秒兜底），只有
  超时才记录 `agent turn reported no terminal` 并以失败关闭。因此 turn 终结与
  最后一批 item 的持久化同批发生，不会先关 turn 再把剩余流按完成态冲刷。
- `agent.session.in_progress`/`idle`/`failed` 的 `metadata` 来自会话行；官方
  `AgentSession.prompt_cache_options` 是可选项，网关只在有持久值时发送它（没有值
  时字段不出现，而不是伪造 `null`）。实时与回放使用同一会话行投影，字段不静默分叉。
- `usage` 恒为 `null`；`requires_action` 只对声明了 MCP 审批的 MCP 绑定发送
  （见 §4.5 与 `spec/issues/2026-11-21-peri-swarm-requires-action-design.md`）；
  `turn.cancelled` 不存在：取消折叠为 `turn.completed(status="cancelled")`；
  budget、subagent 与官方 `function_call` item 不在本层。

  审批范围只由网关扩展 `agent.tools[].require_approval` 触发，匹配依据是绑定身份
  （`server_label` + wire 工具名），不是工具名；会话级 `PermissionMode` 仍是
  `Bypass`，只有命中声明的 server 进入审批通道。无法归属到声明的调用 fail-closed：
  进入待办等待，不默认放行。
- 本批**不覆盖**的官方事件（本地没有对应持久事实）：四种
  `reasoning_summary.*`、八种 `environment.*`、三种 `subagent.*`。assistant 的
  `output_text.delta` 已实现（实时逐 chunk、回放单条全文）；reasoning delta 仍未实现。
  `AgentSession.environment` 已随会话行投影（§4.5）；快照只包含网关实际持久化的
  字段，未知环境为 `{"type":"none"}`。


### 4.3 创建与首轮

`POST /sessions` 接受 OpenAI Agents API 形状的 `model`/`instructions`（顶层与
`agent` 内同名字段必须一致）、`input`、`environment`、`tools` 与 `stream`。
`stream:true` 时网关先为 `logicalId` 打开订阅；service 受理初始输入后立即返回
（actor 在 action reply 前用 `ctx.wait_until` 注册首轮，in-process fake 由 handler
`tokio::spawn`，与 `POST /events` 的 `202` 相同），网关再构造并返回 SSE 响应
（headers、首个官方 `agent.session.created` 帧与自定义 `session` 帧）。官方帧在
任何 turn 帧之前；`events` 流的实时与回放都不发 `agent.session.created`。幂等
attach（`created=false`）只发自定义 `session` 帧，不伪造“刚创建”的官方帧。首轮事件
在广播容量内实时送达；客户端读取过慢或
首轮突发超过容量时，按 §4.2 的 `gap` + EOF 语义显式失败，客户端可按最后已渲染
item 的 `seq` 重连。
actor 路径的 action reply 与 gateway 响应构造之间仍有一个短窗口：`wait_until` 在
action reply 后即可开始执行，因此不承诺「首轮一定晚于客户端读到响应」，只承诺
订阅先于首轮建立、响应构造不等待首轮完成。延迟的不是受理：初始 user item 与其
`turn_id` 已在 service 内持久化，执行归属仍由 service/actor 持有，因此 `creating`
行恢复时「已有 item 不重放」的至多一次语义不变。必须延迟的原因有二：广播 receiver
只在响应到达客户端后才被轮询，而通道容量有界（gateway 默认 256）；若在返回响应前
跑完首轮，超过容量的首轮会让首次读取得到 `Lagged`，SSE 发一帧 `gap` 后关闭，整轮
帧丢失。即便未溢出，首轮也会在结束后一次性投递，而不是实时流。
订阅在会话行写入之前建立：tail 订阅不读会话行（见 §4.2），gateway 的
`open_stream` 在 actor 接受该订阅后转发真实活跃流，不会退回已关闭的广播。
命中已存在行（含 `creating`）的 create 走与 `accept_input` 相同的 admission 规则：
若该会话已有运行中的 turn，或已有受理但尚未开始的 turn（user item 已落库、执行尚未
启动），`create_session` 在写入初始 user item **之前**返回 `409 turn_in_progress`，
`defer=true` 与 `defer=false` 一致；`creating` 行恢复在 `last_seq == 0` 窗口内同样
由该检查拒绝，不静默跳过。被拒绝的 create 不写 user item，因此不会留下没有终态的
stray opener。判定依据是进程内在途 turn（`is_running`/`has_open_turn`），不是持久化
的行状态。这些检查只适用于已有运行时的会话。宿主崩溃后的下一条新输入按 §7 恢复。
非流式创建不延迟：首轮仍在 `create_session` 内执行，错误仍以 HTTP 状态返回；
流式创建的首轮错误只出现在流上。

首次创建时先向 Bun 懒预留 hosted 环境，再用一次 Swarm 写事务登记绑定及
`state=creating` 的完整会话行（含 `rootSessionId`、`cwd`、model、instructions、
`mcp_tools`、environment 与时间戳）→ `session/new` → 同一行改 `idle` → 首轮
（流式创建在 `idle` 之后先建立订阅并构造/返回 SSE 响应，再调度首轮，见上）。
`created=true` 只出现在首次创建；命中已存在行（无论 `idle` 还是 `creating`）都返回
`created=false`。行状态语义：`creating` 表示身份已持久化但 Peri 会话尚未确认，
`idle`/`running` 表示 host 已就绪；`creating` 不是「创建失败」，而是可重试的中间态。

### 4.4 幂等语义

- 输入受理需要稳定 `inputId`。原生 body 的 `inputId` 必须是 UUID；`Idempotency-Key`
  接受任意非空 ASCII 字符串（区分大小写、不 trim，空值视为未提供）。UUID 键原样用作
  `inputId`；其他键派生为 `UUIDv5(namespace, logicalId + "\0" + key)`，namespace 冻结为
  `76663edb-72b5-58df-9b4c-b1e3c8f8d418`。header 与 body 同时给出时，body 必须等于
  解析后的 header 值。字面 UUID 与派生 UUID 共用同一收据键空间；非 UUID 原键不落库、不回显。
  UUID 文本按**字面**保留为 `inputId`（花括号、无连字符、大写等非规范写法都接受），
  同一逻辑键写成两种串会得到两条收据：重试请复用同一字节串。
  原生路径两者都未提供仍返回 `400`；官方 message/cancel 无键时生成一次性 UUIDv7，
  不保证无键重试幂等。MCP 审批答复给出 header 时使用同一派生规则。
- 同一 `inputId` 与同一内容重放返回 `disposition=existing`，不重复追加用户 item；同一
  `inputId` 不同内容返回 `409 input_conflict`，且不入队。官方信封只接受单条事件，不承诺
  批量原子受理。
- `POST /sessions` 的 `Idempotency-Key` 只用于稳定 `logicalId` 并复用已存在会话，
  不是跨请求的事务承诺。
- `POST /sessions/{logicalId}/events` 的用户输入 `202` 是**受理**回执：input receipt、user
  item 与 turn 预留都持久化后才返回。actor 串行处理普通 turn 的准入检查与上述写入，
  不让第二条输入在第一条占住待运行 turn 之前通过检查。turn 在响应之后执行（actor 路径经
  `wait_until` 调度，HTTP 路径用独立任务；与流式 create 一样存在 action reply 与
  响应构造之间的短窗口，见 §4.3）。响应中的 `lastItemSeq` 是受理时刻的
  `MAX(seq)`，通常只含本条 user item；assistant item 与终态经 SSE 投递。因此
  「先建流、后 POST」仍是观察本轮的可靠方式，但 202 不再等待模型。
- 官方 cancel 的 `202` 沿用 `InputOutcome`，只写控制收据，不为取消本身写 item 或建
  turn。**收据在下发之后写**：只会向准入时已绑定的 slot 发送 `session/cancel`，空闲或
  尚无 slot 时是 no-op；两种下发失败分开返回：
  - 首次下发失败（ACP 拒绝或传输关闭，收据不存在或已回滚）不留收据，返回 `503`
    （`error.code=queue_unavailable`，`details.operation=session/cancel`），文案是“取消未
    送达、未写收据、可用同键重试”，同键重试因此可以重新受理。
  - 同键重放的**重发**失败（收据已存在）返回 `503 cancel_redelivery_failed`
    （`details.receipt=accepted`），文案明确“取消收据已存在，但本次重发未送达，可再次用
    同一键重试”；不能沿用“未写收据”的首次失败措辞。

  下发成功但宿主在等待窗口（30s）内没有退出 slot 时仍是 `202`：这只表示取消已送达，不表示
  已取消，终态以宿主为准。此时同键重放且该 turn 仍持有 slot，会**重新下发**
  `session/cancel` 并返回 `disposition=existing`，不新增 item；目标始终是受理时绑定的
  turn，因此不会误取消后来启动的 turn（目标 turn 已离开 slot 时重放只是 no-op）。无 live
  runtime 且持久 turn 未结束时，不写新收据，返回 `502`
  （`details.operation=session_resume_required`）。宿主确认 `cancelled` 才按该状态落库并
  发送 `turn.completed`；终态仍按既有 `turn_ended` 规则结算该 turn 的待办。网关不把受理
  等同于宿主停止，也不新增 `turn.cancelled` 帧。
- 取消重试的已知边界：重发目标表 `accepted_cancels` 是**进程内**状态，actor 重启后同键
  重放命中持久收据并返回 `202 existing`，但不会重发 `session/cancel`；收据写入失败发生在
  `session/cancel` 已送达之后时，已送达的取消不会回滚，同键重试可能再次下发同一取消。
  两者只记录为边界，不引入跨进程重发或补偿事务。
- 同步失败仍按原语义返回：空输入/畸形请求 `400`，同 key 不同内容
  `409 input_conflict`，恢复期间的冲突或上游故障返回对应的 409 或 502
  （`details.operation` 指向具体阶段）。turn 执行失败在流上以 `turn.failed` +
  `agent.session.failed` 呈现，不追认改已返回的 `202`。
- 创建中断的恢复是**按已持久化的 root id 核对**，不是重新生成：`creating` 行重试
  先 `session/load`、必要时才用同一 root id `session/new`，返回 `created=false`；
  初始输入至多执行一次（行内已有 item 时不重放；`last_seq == 0` 但已有在途 turn
  时按 §4.3 返回 `409 turn_in_progress`，不静默跳过）。两次尝试都失败返回可诊断的
  `502`（`details.operation=create_recovery_failed`），行与 root id 保留，可继续
  重试。ACP 返回的 session id 与行内 root id 不一致时以 `session_id_mismatch`
  失败，不接管该身份。
- `sessions.update` 与 `sessions.delete` 使用管理操作的准入判定：
  - 有 live runtime 且 turn 正在运行（持有 admitted slot）：`409 turn_in_progress`，详情
    包含 `suggest:"cancel"`、`event:"agent.session.input.cancel"` 和会话的 `POST /events`
    入口；这不改变普通用户输入的 `suggest:"steer"`；
  - 有 live runtime 但**没有** admitted slot：同样 `409 turn_in_progress`，详情改为
    `suggest:"retry_later"`、`reason:"recovery_pending"`，不宣传 cancel——该状态下
    `session/cancel` 没有可达目标，只是一次 202 no-op。两个子状态的推进方式不同：
    `child.recovering()` 的恢复窗口里，新用户输入会等待恢复终态（最长 5s），超时同样
    409，并让下一次输入尝试 `session/resume`，因此建议等待恢复；已 `begin_turn` 而
    slot 已释放且不在恢复中时，新用户输入不会触发任何恢复，只会命中开放 turn 的
    `409 turn_in_progress`（`suggest:"steer"`），只能等宿主终态落库。之后再重试
    修改/删除；
  - 无 live runtime，但持久会话行 `state=running` 或 `swarm_turns` 仍有开放行
    （`queued`/`in_progress`/`waiting`）：`502`，`details.operation =
    "session_resume_required"`，与 `ensure_live` 的恢复拒绝一致；更新（官方 POST）
    与删除都不会先写后报错；
  - 正常 idle 行不受影响。

  live runtime 路径的准入判定在受理互斥内完成，更新/删除不会与该 actor 的并发
  受理交错；无 live runtime 的拒绝路径只读持久状态（会话行与
  `swarm_has_open_turn`），不写 `state`/turn 行，但并发 attach 仍可能在该判定之后
  开始新 turn——拒绝不是跨进程的锁。删除成功后的 get/events/items/turns 都是
  `404 unknown_agent`；同一 `logicalId` 重新 create 视为新会话，不复用旧 root id。

### 4.5 会话投影与环境

`GET /sessions/{logicalId}`（以及创建响应）的 `environment` 按官方类型投影：
`none` 只有 `{"type":"none"}`，不带 `id`；hosted 是 `{type,id}`，`id` 是
logicalEnvId。早期持久行即使为 `none` 存过 logical id，投影时也归一化为
`id=None`，旧数据不会泄露额外字段。hosted 会话另有 `sandbox` 投影，字段来自
`swarm_bindings`：`logicalEnvId`、`sandboxId`、`runId`、`workspacePath`。`runId`
在绑定记录后如实填充；`state` 固定为 `null`：网关只持久化绑定，不跟踪 Bun 的实时
SRT 启停，表里也没有该列，所以不伪造 `reserved`/`running`。实时状态以 Bun
`GET /v1/sandboxes/{logicalEnvId}` 为准。会话 `cwd` 是网关工作目录，不是工具执行
目录；工具在 `sandbox.workspacePath` 下执行。

`status` 由会话行、进程内在途 turn 与**当前有效待办**投影：有当前待办为
`requires_action`；否则有在途 turn（含已受理未开始）为 `in_progress`；行状态
`failed` 为 `failed`；其余 `idle`。有效性的唯一判定点是 `ItemLog::required_actions`：
待办未结算、未超过 host 审批等待期限（`peri_acp_types::interaction::APPROVAL_WAIT`，
与 `BROKER_TIMEOUT` 同源），且所属 `session/prompt` 仍是当前执行代次。
`sessions.retrieve`、SSE 快照、`sessions.list`（对 live actor 覆盖）与回放共用它。
持久行可以继续是 `running`；`deny`/审批 `cancel` 不提前终止 turn。
硬崩溃遗留的 pending 行不属于新 actor 的代次：恢复时按 turn 行状态结算为
`actor_lost`（turn 未结束）或 `turn_ended`（turn 已结束），追加 settled 行后再参与
折叠，因此 retrieve/list/replay 不会展示不可答复的动作。live actor 的 read/SSE
路径在读回放窗口前先结算已过期待办；过期或已结算的行不会被当作当前待办。
答复送达与结算按两阶段执行：送达失败时待办保留、返回 502；只有送达成功才写
`answered` 并返回成功回执。settled 行先落库再删除内存待办：落库失败返回
`503 required_action_settlement_failed` 并保留待办，回执成功只描述已持久化的结算。
`reason` 的取值是 `answered|expired|turn_ended|actor_lost`；ACP 传输关闭按送达
失败（502）处理，不单独伪造一个持久结算原因。

会话投影同时携带官方默认字段：`required_actions`（当前待办，通常为 `[]`）、
`vault_ids=[]`、`usage=null`、`error=null`。vault、token 记账与会话级错误没有本地
事实，不伪造非空值。`OfficialSession.environment` 使用与 REST 相同的
`{type,id}` 投影：hosted 带 logicalEnvId，`none` 省略 id，未记录时为
`{"type":"none"}`。反序列化对四个字段使用 serde 默认值，
旧的持久/测试 JSON 不因缺字段失败。

会话时间字段使用官方 `AgentSession.created_at` / `last_active_at` 的 snake_case
拼写，类型是 Unix 秒 number；投影由持久行的 RFC3339 `created_at` /
`last_activity_at` 列转换。响应不再返回 `createdAt` / `lastActivityAt` camelCase
时间字段，其余 Swarm 扩展字段仍为 camelCase。

会话投影同时携带官方 `metadata`（`string -> string`，空对象表示无）与
`prompt_cache_options`（`{ttl:"30m"|"12h"}` 或 `null`）。`prompt_cache_options` 是
官方兼容字段，使用官方 snake_case 拼写（`AgentSession.prompt_cache_options`）；
其余 Swarm 扩展字段仍为 camelCase。create 接受两者：metadata 最多 16 对、
key 1..=64 字符、value ≤512 字符；`prompt_cache_options.ttl` 只接受 `30m`/`12h`，
对象省略 `ttl`（含 `{}`）按官方默认取 `30m`；`null` 清空，字段省略保持行内值。
重复 create 命中已存在行时，省略的 metadata/prompt cache 保持行内值，不参与冲突
判定；显式提供的值（含 `{}`/`null`）会先归一化（`{}` 即 `30m`）再与行内值比较，
不一致按身份冲突 `409 identity_conflict` 拒绝，不静默覆盖。`agent.model` 是官方
string；显式 `null` 返回 `400 invalid_request`（`details.field="agent.model"`），
不作为省略处理。`agent.tools: null` 仍是合法的清空声明。


## 5. MCP 注入

### 5.1 调用方声明

调用方在 `POST /sessions` 用官方 `tools`（或 `agent.tools`，两者同时给出必须相等）
声明 MCP server。网关规范化为 `McpToolSpec` 并按 canonical JSON 写入
`swarm_sessions.mcp_json`；`headers` 统一为按名称稳定排序的 `{name,value}` 数组，
`require_approval`、`allowed_tools` 与未知官方扩展字段原样保留。

注入发生在 ACP `session/new`（首建）与 `session/load`（恢复）的 `mcpServers`：

- 每条声明映射为 `{type:"http", name:server_label, url:server_url, headers}`；
  `require_approval`、`allowed_tools` 与扩展字段进入该条目的 `_meta`。
- 这些字段只被记录与转发，网关不解释审批策略、不执行工具过滤。
- 非 `mcp` 的官方 tool 类型返回 `422 unsupported_tool`，因为本地没有执行面。

### 5.2 内部 workspace

Bun 沙箱以内部 MCP 条目注入：名称固定 `workspace`，URL 为
`<bun>/sandboxes/<logicalEnvId>/mcp`，`Authorization` 只在内存中拼装，条目带
`_meta.peri.swarmSandbox=true`。Peri 将「`workspace` + http url」识别为
`WorkspaceRemote` 系统 MCP；条目顺序为 workspace 在前、调用方声明随后，空集合时
省略 `mcpServers` 键。

宿主装配固定 `bare=false` 且关闭 builtin MCP、stdio MCP、cron、plugins 与 settings
hooks；远程 workspace 与调用方声明的 server 是唯一的 MCP 面。宿主同时显式声明
`lazy_workspace: true`（部署级 opt-in）：这是本部署唯一的 cold 语义来源，TUI / print /
stdio 与普通 ACP 客户端保持 `false`，远端 workspace 的资源发现失败仍拒绝初连。

### 5.3 冷态工具面

Bun 端在沙箱启动前就能回答 MCP 握手：`initialize`、`server/discover`、`tools/list`
与 `ping` 来自生成的冷快照，通知直接 `202`，不启动 SRT；首个 `tools/call` 才以
single-flight 方式拉起实例。工具清单与协议版本来自快照，provider 版本不符只在
`_meta.peri.swarmDiagnostics` 中报告，不拒绝工具面。

### 5.4 资源探测延后

远端 workspace 在冷态不参与启动期探测：不请求 `resources/list`、不读工作区指令或
meta、不做 legacy 资源模板/技能投影、不扫描 `shell_task_list`、git 归因返回
`Unavailable`。该延迟只对宿主显式 `lazy_workspace` 且来源是 `WorkspaceRemote` 的
会话生效。首个真实 `tools/call` 置位 `workspace_remote_started`，随后只做一次
启动后 `resources/list` 重发现；发现失败只 WARN，不改变连接、不失败会话。

已记录边界：这次重发现的结果不会回写 `McpClientHandle.resources` 快照，面板与按
句柄投影的资源面在会话内仍是冷快照。

## 6. 运行

构建与启动：

- Rust 二进制必须经仓库补丁脚本构建/运行，因为 `Cargo.lock` 指向打过
  task-subscriptions 补丁的 `rmcp`：
  `./scripts/cargo-rmcp-patched.sh build --locked -p peri-swarm` 后执行
  `./target/debug/peri-swarm`；等价入口是
  `./scripts/cargo-rmcp-patched.sh run -p peri-swarm`。`Cargo.lock` 记录的是本地补丁
  crate，未注入补丁配置的 cargo 在解析阶段就失败（`cargo metadata --locked` 可复现）。
- 网关环境变量以 [peri-swarm README](../../peri-swarm/README.md) 为操作事实源：必填
  `PERI_SWARM_API_KEY`、`PERI_SWARM_SESSION_STORE`、`PERI_SWARM_STATE_STORE`、
  `PERI_SWARM_MACHINE_ID`、`PERI_SWARM_WORKSPACE_ROOT`；Bun 桥 `PERI_SWARM_BUN_URL`（或
  `PERI_SWARM_BUN_BASE_URL`）与 `PERI_SWARM_BUN_TOKEN` 可选，未配置时
  `environment.type=openai_hosted` 在创建时显式失败，`none` 不受影响。

PostgreSQL：

- 两个库都由部署提供 PostgreSQL 连接串：会话库交给 Peri 会话存储，状态库交给
  `store::postgres::SwarmStateStore`。DSN 可带 TLS 参数（SQLx rustls），日志与错误只
  渲染 `postgres://host:port/db`，不含凭据。
- 会话库连接池上限由部署选择：单 facade 默认 **2**（TUI/CLI/stdio 行为不变），Swarm
  传 **32**。默认 `max_connections=100` 下两副本预算为
  `2 × (32 + 8) + 3 = 83 ≤ 100`（8 是状态库池上限，3 是 PostgreSQL 保留槽）。32 是
  上限不是预留，`min_connections(0)` 使空闲池回落到 0；重连沿用打开时的策略。
- 状态库首次启动自动建表；`swarm_meta` 记录 engine `postgres` 与 contract
  `peri.swarm.postgres/v1`，因此把 Loom/SQLite 文件或会话库误指过来都会显式失败。
- 从 Loom 割接不做自动迁移，网关不读取旧库文件；需要保留旧状态库内容时停写后取只读
  SQLite 快照，按 README 的离线导入工具（`import_loom_state.py`）写入空的专用库并
  对账。导入只在离线阶段发生，运行时不引入 Loom/Turso 依赖。

Rivet：

- 本地 engine 为 Rivet 3.1.1（`~/.rivetkit/engine/v3`）；默认 endpoint
  `http://127.0.0.1:6420`、namespace `default`、pool `peri-swarm`，默认
  `PERI_SWARM_RIVET_ENGINE_SPAWN=always` 在 `RIVETKIT_ENGINE_AUTO_DOWNLOAD=1` 时按需
  拉起本地 engine。副本组的进程只有共享同一 engine、namespace、pool、会话库与状态库
  与部署身份时才看到同一批 actor。
- 配置面：`PERI_SWARM_RIVET_ENDPOINT`（仅明文 `http`，必须显式端口：写下默认端口
  `http://host:80`、`http://[::1]:80` 也算显式；`https` 显式拒绝，engine TLS 终止需
  另行设计）。字面地址只接受 loopback（`127.0.0.0/8`、`::1`）、RFC1918（`10/8`、
  `172.16/12`、`192.168/16`）、ULA（`fc00::/7`）与 link-local（`169.254/16`、
  `fe80::/10`）；公网、`0.0.0.0`、`::`、组播与广播字面地址在解析阶段失败。域名不做
  DNS 解析而直接接受，由部署保证指向私网或环回。其余配置面：
  `PERI_SWARM_RIVET_TOKEN_ENV`（**变量名**，不是值）、
  `PERI_SWARM_RIVET_NAMESPACE`、`PERI_SWARM_RIVET_POOL`、
  `PERI_SWARM_RIVET_ENGINE_SPAWN`（`always`/`auto`/`never`）。token 值只在启动时
  解析一次；启动日志只记录 endpoint/namespace/pool/spawn。
- namespace 与 pool 是部署身份：一个环境一套；多个环境共用一个 engine 时必须使用
  不同值，否则 actor 会串台。
- 外置 engine 必须 `never` 且提供 token 变量名；`always`/`auto` 只允许 loopback
  endpoint，矛盾在解析阶段失败。`never` 时进程在打开会话库与状态库之前做一次 5 秒
  TCP 预检；失败即启动失败，消息写明 endpoint 且说明不会拉起本地 engine。预检只
  证明 TCP 可达，engine 握手与健康仍由 registry 任务报告。
- 未验证：engine 多节点/集群入口、engine 重启后的客户端重连、逐副本 engine 凭据
  与 engine TLS。

Bun：

- `npm-packages/peri-sandbox` 的 `bun run dev:swarm` 启动本地服务，默认
  `127.0.0.1:8081`；根目录默认 `~/.peri-swarm/sandboxes`，空闲回收阈值
  `PERI_SWARM_IDLE_MS` 默认 30 分钟，`0` 关闭。沙箱内的 Peri 可执行文件由
  `PERI_PATH` 指定。Swarm 启动沙箱时显式传 `allowPeriBinaryDirectory: true`，
  只把该实例 profile 的 `runtimeReadPaths` 扩到 `dirname(periPath)`
  （macOS `sandbox-exec` 需要父目录 search 权限）；公共 `peri-sandbox` policy
  默认不放行该目录。

模型：

- 部署级模型来自 `PERI_SWARM_MODEL` 与 `PERI_SWARM_SETTINGS_JSON`（可信 settings
  文档：`config.active_alias` + providers/profiles）。`session/new` 的 `model` 参数
  没有宿主消费点，因此同一 actor 内所有会话共用进程级模型；多模型需要多 actor。
- 本地验收使用确定性 stub 模型；真实供应商与工具选择质量未验收。

## 7. 恢复边界

允许的故障域是「只杀 Rust 宿主」：两个 PostgreSQL 库、Bun、SRT、模型与 Rivet engine 保留。

- 新进程启动时，actor 的 `on_start` 只恢复只读视图（会话行与 `MAX(seq)`）；`GET`
  读取不装载 host、不触达 Bun。
- 空闲会话在下一次交互时用持久的 `rootSessionId`、`cwd` 与 MCP 配置执行 `session/load`。
  工作区先从状态库读取绑定，再由 Bun 懒预留。丢失绑定或目录身份改变时向调用方报告错误。
- 宿主丢失时，历史查询可以看到上一次持久化的开放 turn。新的用户输入按幂等键去重后才触发恢复。
  网关先核对 Peri 持久工作状态，再调用 ACP `session/resume`。上次同步工具的结果若无法确认，
  旧 Swarm turn 记录为中断失败，保留历史，不重发原提示或工具调用。
- 若 Peri 的持久 MQ 有待处理结果，ACP 负责恢复。网关在根会话的恢复工作结束后接纳新的 turn。
  查询 session、items 和 turns，以及重复提交已经受理的输入，都不会再次启动旧命令。
- `sessions.update`/`sessions.delete` 在无 live runtime 时使用同一判定：持久行
  `running` 或存在开放 turn 行都返回 `502 session_resume_required`，不会先修改或
  删除再报错；只有 live runtime 内的在途 turn 返回 `409 turn_in_progress`。
- 创建中断（行状态 `creating`）走独立的核对路径：先 `session/load`，只有当 Peri
  会话确实不存在时才用同一 root id `session/new`，冲突后再试一次 attach；两条路
  都失败返回 `502 create_recovery_failed` 并保留行。`ensure_live` 不把 `creating`
  当作 `running` 拒绝，创建的重试沿用原身份。

历史只读不等于命令重放：恢复后的 `session/load` 会执行装配期探测（Bun 日志可见
`resources/list`），但不会执行任何历史工具命令；旧 workspace 文件的哈希与 mtime
保持不变是直接证据。

副本组：

- actor 无状态且空闲 300s 后由 engine 回收，下一次交互可能在**任一**副本唤醒；
  跨副本重建沿用上面的读历史与交互恢复规则。
- 因此所有副本必须使用同一 `PERI_SWARM_MACHINE_ID`、同一绝对路径的
  `PERI_SWARM_WORKSPACE_ROOT`（共享卷或同路径挂载）、可写的会话库与状态库，并保证
  Bun（hosted）从每个副本可达；actor 重建副本上的 `cwd` 必须本机存在。
- 滚动部署与多副本接管的验收范围见 §8。

## 8. 未完成项与风险

1. **沙箱任务与进程边界**：Bun 在同一次服务运行内跟踪 MCP 后台任务，终态后重新计算空闲时间。
   自动回收保留懒预留，下一次工具调用可唤醒原环境。Bun 自身重启后的任务身份与进程重建另行验收。
2. **跨故障域恢复**：这里覆盖仅有 Rust 宿主退出的情形。Bun、PostgreSQL 与宿主同时失效以及滚动部署仍需独立验证。
3. **真实部署模型任务**：模型侧全部为确定性 stub；真实供应商的 tool call、长任务
   与多轮副作用未验收。
4. **1000 Agent 规模与单 engine 容量**：已有 1000 个空 session 的实测：修复前
   943/1000 创建失败（502，PostgreSQL 连接耗尽），根因是每个 ACP child 独立打开会话
   存储；修复改为每进程共享门面/池并把 Swarm 池上限设为 32，见
   [验收记录](../verification/peri-swarm-1000-concurrent-sessions-2026-11-27.md)。
   修复后复测：1000/1000 成功，7.08s，网关会话库连接 32+32、未出现
   `too many clients already`；旧 943 个 `creating` 未清理。actor 的并发 SSE 观看者、
   单 engine 节点容量、1000 个同时执行 turn 的 Agent 吞吐与长时内存仍无证据。
5. **生产 TLS 与入口鉴权**：网关默认只绑定 loopback；非 loopback 绑定（含
   `0.0.0.0`）需要 `PERI_SWARM_BIND_ALLOW_NON_LOOPBACK` 显式开关，启动时输出 WARN；
   Bearer 是唯一鉴权、TLS 必须由受信任入口终止，组合后的链路未验收。Rivet endpoint
   只接受私网/环回字面地址或由部署保证指向私网/环回的域名，且只支持明文 HTTP；两个
   PostgreSQL 库按 DSN 的 TLS 参数连接，未在真实 CA 环境验收。生产 TLS 与证书轮换未实现。
6. **Linux SRT**：本地 Linux 验收覆盖 Bun 空闲回收，并以 Bubblewrap 启动原生 Peri MCP
   执行工作区命令。当前执行机禁止 SRT 的 Unix socket，且无法使用 systemd 用户总线；
   生产 `systemd-run` 与 SRT 组合仍待验证。
7. **真实模型工具调用**：工具 wire、冷/热分界与结果回填已在 stub 驱动下验证；
   真实模型是否会选择工具、如何重试、失败恢复及流式生成中断仍未验证。
8. **调用方第三方 MCP**：注入形状与 `session/load` 复用有集成测试，但没有真实第三方
   MCP server 的端到端运行。
9. **冷态可见性降级**：hosted 会话冻结的 system prompt 不含远端 workspace 指令与
   覆盖（冷期不可得），启动后资源发现也不刷新句柄快照。
10. **整批原子性**：逐条受理的语义固定，不提供批量事务或整批回执。
11. **readiness 与滚动发布**：没有独立的 readiness 探针、drain 或滚动发布流程；
    停副本时在途 turn 的实际行为未定义，也没有自动处置。
12. **逐副本 engine 凭据**：Rivet 当前是单一 admin token 模型，未实现逐副本或最小
    权限凭据。
13. **engine 重启后的客户端重连**：运行中停/启 engine 时进程是退出、请求 502 还是
    自动重连，未验证。
14. **官方会话资源面**：既有库升级与双副本并发迁移、PostgreSQL 状态库的 metadata 过滤
    （已有真实 PG 契约测试）、删除事务中断、SDK 完整资源面仍需独立验收；已覆盖范围见
    [官方 SDK 探针](../verification/peri-swarm-session-resources-2026-11-21.md#5-真实官方-sdk-探针)。
15. **超时分相**：写入前的有界阶段超时即回滚（可安全重试）；`COMMIT` 已发出但无回包时
    结果未知（可能已提交），调用方按 key/seq 重读对账，不盲目重放。
16. **输入受理与恢复**：receipt、user item 和 turn 不是一个原子提交；不能从同键重试的
    202 推断已执行。无 owner/lease/fencing 时不得让多个宿主同时服务同一 logicalId；
    即使部署支持节点放置，也未证明旧宿主已停止。

## 9. 已批准目标：事件日志粒度与回放置位（已实施）

> 状态：2026-11-26 裁决，**已实施**（同日）。验收矩阵与证据边界见
> [active spec](../../spec/issues/2026-11-26-swarm-event-log-granularity.md)；
> 实现位于 `peri-swarm/src/actor/reader.rs` 的 staged flush，ILOG-1..7 用例见
> `peri-swarm/src/actor/reader_test.rs`、`service_test.rs`（ILOG-4 的审批折叠与合并行
> 交错回放用例）与 `peri-swarm/tests/it/`。§4.2 已按本节契约改写。

1. **持久化粒度按 item 折叠**：同一 assistant 文本流或推理流的 chunk 合并为一条 item 行，
   turn 终止再写终态标记；不再为每个 chunk 分配 `seq`。`tool_call`、`tool_result`、
   `approval` 保持一帧一行。`seq` 仍是唯一分页与续读游标，但不再逐 chunk 推进。
2. **回放真源固定为 PostgreSQL 状态库**：跨进程重启与跨副本续读都依赖它；不引入 Redis
   或第二个有状态存储。actor 内可有有界环形缓冲只服务短窗口续读，溢出仍发 `gap`，
   不作为真相。
3. **清理策略不变**：仍只有显式 `DELETE /v1/agents/sessions/{logicalId}` 级联删除。
   保留期/TTL 是独立可选项，默认关闭，未批准实施。
4. **不变项**：官方事件序列（C=2 后为 16 种）、`/items` 分页与 `after` 语义、审批
   `pending`/`settled` 折叠、崩溃恢复的 initial input 幂等。
5. **兼容与回退**：无 schema 变更；历史 chunk 行与新 item 行按 `seq` 单调共存，
   回放不重写数据；回退只需恢复原写入路径。
6. **已知代价（C=2 更新）**：回放不保留原始 chunk 边界；assistant 完成行只合成
   **一条全文** `output_text.delta`，实时仍逐 chunk。实时 chunk 没有持久 `seq`，所以
   实时 delta 的 `event_id` 与回放 delta 不保证一致；assistant 的 `item.added`/
   `content_part.added` 已改为按稳定 `itemId` 派生，跨投递一致。reasoning delta 未实现。
7. **取代关系**：本节第 1 条已取代 §4.2 中逐 chunk 行的表述（“不能用 `id` 去重
   delta 帧”等）；§4.2 现在描述合并行、实时逐 chunk delta 与回放单条全文 delta 的
   现行契约，历史多行数据只在 §4.2.2 保留说明。

**2026-11-26 C=2 实施（非持久化）**：`agent.session.turn.output_text.delta` 已在实时
逐 chunk 发送，首 chunk 先发 `item.added`/`content_part.added`；回放从合并行合成一条
全文 delta。`reasoning_summary_text.delta` 未实现。chunk 不入库、不新增 schema/字段，
`swarm_items` 仍是一条 assistant 合并行，§9 第 1 条的合并落库裁决继续有效；本段不是
回退，也不改变 `/items` 投影。
