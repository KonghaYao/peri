# Peri Swarm 本地开发宿主

> 状态：**已批准目标设计**（2026-11-20）。单机本地链路（HTTP 面、Rivet actor、双
> Loom 库、Bun 沙箱、MCP 注入与 reattach）已落地，并以真实组件验收，证据见
> [本地开发验收记录](../verification/peri-swarm-local-dev.md)；跨机调度、生产 TLS、
> Linux SRT 与 1000 Agent 规模未验收。进度与剩余项以该记录的 `unverified` 清单和分支
> `feat/peri-swarm-local-dev` 为准。本文只写稳定结构与边界，不记录实施批次、测试数量
> 或运行日志。

## 1. 范围与非目标

本地部署把一条 HTTP 调用链固定下来：客户端 → `peri-swarm`（axum 网关 + Rivet
actor）→ 进程内 Peri ACP Host → 远程 workspace MCP（Bun 托管的沙箱）→ 两个 Loom
逻辑库。断连不终止执行；客户端按序号续读；Rust 宿主退出后，下一次交互经
`session/load` 恢复会话事实。

非目标：

- 不做跨机调度、租约或 fencing；网关默认只绑定 loopback，非 loopback 绑定（含
  `0.0.0.0`）需要 `PERI_SWARM_BIND_ALLOW_NON_LOOPBACK` 显式开关并输出 WARN；鉴权仍是
  单个部署 Bearer，TLS 必须由该入口终止。
- 副本组共享同一 engine、namespace/pool 与两个 Loom 库；多副本不改变本机的恢复
  语义（§7），也不提供跨实例在途 turn 接管。
- 不新增 ACP 方法名或协议版本，不改 Peri 的通用 SQL 执行面。
- 不把 `session/load` 的只读浏览路径变成命令重放入口。
- 不承诺整批输入原子的受理语义（见 §4.4）。
- 不提供生产 TLS、多租户隔离或跨实例接管。

代码入口：`peri-swarm`（crate 根、`src/http`、`src/actor`、`src/store`、
`src/gateway`、`src/sandbox`），运行说明见 [peri-swarm README](../../peri-swarm/README.md)，
沙箱实现见 [Sandbox 设计](../../npm-packages/peri-sandbox/peri-sandbox-design.md)。

## 2. 两个 Loom 逻辑库

一个 `peri-swarm` 进程同时拥有两个独立的远端 Loom 逻辑库；没有本地 SQLite 回退。

| 库 | 所有者 | 版本 | 内容 |
| --- | --- | --- | --- |
| Peri 会话库 | Peri 会话存储（`SessionStoreDeployment`） | `peri_store_meta.schema_version=15`，contract `peri.session.store/v9` | 线程、消息、MQ、workspace 登记等 Peri 事实；不得出现 `swarm_*` 对象 |
| Swarm 状态库 | `peri-swarm`（`store::SwarmStateStore`） | `swarm_meta.version=1` | `swarm_sessions`、`swarm_items`、`swarm_inputs`、`swarm_bindings`、`swarm_turns` 与索引 |

- 两个 locator 都是必填且必须不同；同一数据库、缺失 locator 或任一库打不开都会让
  进程启动失败，不复用另一库、不降级。
- Peri 会话库在打开时执行 Peri schema 检查/升级；未知或更高版本 fail closed。
- Swarm 状态库自带 DDL 与版本判定：已存在 `swarm_*` 表但没有 `swarm_meta` 时拒绝
  接管，未知版本直接中止。版本内新增的列是加法式迁移：`PRAGMA table_info` 判断缺列后
  执行 `ALTER TABLE ... ADD COLUMN`，新库由 `CREATE TABLE` 一次建好；版本号不因加列
  改变，重复打开不会再次执行 `ALTER`。`swarm_turns` 同样由幂等的
  `CREATE TABLE IF NOT EXISTS` 补齐。
- 工作区归属仍由 Peri 的远端工作区实现（`RemoteWorkspaceEnvironment`）决定：`cwd`
  必须落在虚拟根内，workspace 登记与准入语义不在 `peri-swarm` 复制一份；相关边界见
  [存储 v2](storage-v2-machine-workspace-session.md)。`peri-swarm` 不新增通用 SQL
  端口，只通过窄的 `SwarmStore` 端口读写 Swarm 事实。

## 3. 身份

| 身份 | 产生与稳定性 | 归属 |
| --- | --- | --- |
| `logicalId` | 调用方提供 `^[A-Za-z0-9_-]{1,128}$`；省略时服务端生成 `agent-<uuidv7>`；`POST /sessions` 的 `Idempotency-Key` 派生 `agent-<16 hex>`，重试命中同一会话 | 对外会话身份、URL 路径段 |
| `rootSessionId` | 网关生成 UUIDv7，经 `session/new` 的 `_meta["peri.rootSessionId"]` 交给 Peri；宿主在该部署显式开启 `reserved_root_session_id` 时才接受，否则以 `-32602` 拒绝 | Peri ACP 会话与持久历史；恢复时原值复用 |
| Rivet actor key | 名称 `swarm-session`，key `["agent", logicalId]`；`swarm_sessions.actor_key` 持久化为 `agent/<logicalId>` | actor 路由；actor 只信任 key，激活输入只是部署提示 |
| `logicalEnvId` | 调用方经 `environment.environment_id` 复用，或由网关生成 `env_<uuidv7>` | Bun 逻辑环境；同一会话的 sandbox 绑定键 |
| `sandboxId` / `runId` | `sandboxId` 为 `sbx_<32 hex>`，由绑定或 `logicalEnvId` 派生；`runId` 每次拉起都不同 | Bun 进程与持久目录；`runId` 不跨回收复用 |

绑定关系以 Swarm 状态库的 `swarm_bindings` 为权威，Bun 侧 `bindings.json` 是派生
缓存：只含 `sandboxId`、`workspacePath`、`lastRunId` 与使用时间，绝不保存 token。
恢复时先读 Loom 绑定，缺失即失败，不静默新建环境；`sandboxId` 或 `workspacePath`
发生变化按 `sandbox_binding_changed` 拒绝。

`logicalId` 与 `rootSessionId` 是两层身份：前者属于调用方并决定 actor 路由，后者
属于 Peri 的持久历史。创建顺序固定为「先落 ID、后建 Peri 会话」：网关先校验或分配
`rootSessionId`（调用方预留优先，否则 UUIDv7；非 UUIDv7 在写库前以 `400` 拒绝），
预留 workspace 绑定，然后以 `state=creating` 写入完整会话行，之后才执行
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
| `POST` | `/v1/agents/sessions/{logicalId}` | 官方 `sessions.update`（官方 SDK 用 `POST /agents/sessions/{id}`）：替换 model / MCP tools / metadata / prompt cache；live 运行中返回 `409 turn_in_progress`，无 live runtime 但持久行仍有未完成 turn 时返回 `502 session_resume_required` |
| `DELETE` | `/v1/agents/sessions/{logicalId}` | 官方 `sessions.delete`：删除该会话的全部 Swarm 行并失效运行态；与 update 同一准入判定（`409 turn_in_progress` / `502 session_resume_required`） |
| `POST` | `/v1/agents/sessions/{logicalId}/events` | 提交一条输入（`mode=turn` 或控制输入）；受理持久化后 `202`；兼容官方 `events` 信封（单条 message、纯文本、受理为单条 input） |
| `GET` | `/v1/agents/sessions/{logicalId}/events` | SSE：先补 backlog 再转实时；含官方 `agent.session.*` 事件 |
| `GET` | `/v1/agents/sessions/{logicalId}/items` | 按 seq 增量读取；兼容官方 `CursorPage`（`data`/`has_more`、`after=<itemId>`） |
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
- `POST /events` 额外接受官方 Agents API 的 `{events:[...]}` 信封：本轮仅支持一条
  `agent.session.input.message`、一条 `role:"user"` message、纯 `input_text`
  content，受理为 `kind=user`、`mode=turn` 的单条 input。与原生字段混用、其他事件
  类型、`input_image` 或其他内容类型均 `400 invalid_input`，`details.field` 指向
  具体事件或 content 下标；不支持的能力显式报错，不静默忽略。
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
  `prompt_cache_options` 整体替换（`null` 清空）。省略字段保持不变。`review`、
  `agent.reasoning`、`agent.service_tier` 没有本地映射，显式 `400 invalid_request`
  （`details.field` 指向官方字段），不静默接受。model 或 MCP tools 变化会停止当前
  ACP host；下一次交互按新配置重新 attach，避免返回一个下一轮不会使用的旧配置。
- 官方 `update(sessionID, body?: SessionUpdateParams | null | undefined)`
  （`sessions.d.ts:76`）允许省略 body；SDK 此时不发 body 也不发 content-type。
  网关把「无 body」（含无 content-type、`Content-Length: 0`）与 JSON `null` 都视为
  **空更新**：返回当前 `SessionView`，不写任何行（含 `last_activity_at`），也不执行
  在途/恢复准入判定。只有非空 body 才按上一条解析：非 JSON content-type
  返回 `415 invalid_content_type`，JSON 语法错误返回 `400 invalid_request`；
  两者都不会被降级成 no-op。
- `sessions.delete` 在 `swarm_sessions`、`swarm_items`、`swarm_inputs`、
  `swarm_turns`、`swarm_bindings` 上删除该 `logicalId` 的全部行；Turso 用一次
  受管事务，内存实现同一语义。删除先停 ACP host 并移除进程内 channel，Rivet
  actor 随后销毁，因此用同一 `logicalId` 重新 create 会得到新 `rootSessionId`、
  空 items 与空 turns。删除失败不返回成功，行仍可重试。
- 官方 `sessions.turns.list` 按 `(created_at, turn_id)` 稳定全序分页；`after` 是
  turn id 且排他，未知 turn id 返回 `400 invalid_query`。`turns.retrieve` 的
  `usage` 与 `subagent_id` 恒为 `null`（网关不做 token 记账，也没有 subagent
  turn），`agent_id` 恒为 `root`。


### 4.2 SSE 与 items

- 每个 item 持久化为一行，`seq` 由 Swarm 状态库单调分配；先落库后广播，订阅者
  不存在也不影响执行。
- 用户输入在 `swarm_inputs` 受理（`disposition=inserted`）后写成一条单帧
  `message` item：`payload={role:"user",text,inputId}`，`id`/`itemId` 为
  `itim_input_<inputId>`，没有 `delta`/`complete` 字段。host 的
  `user_message_chunk` 回显不投影，因此同一 `inputId` 重试不会重复；`create` 的
  `input` 同样写一条（每次 create 调用各一条；对已 `idle` 会话的重试 create 仍按
  既有语义重跑该轮，`creating` 行的恢复不重放输入，见 §4.4）。
  官方 `events` 信封同样只落这一条 user message item（多段 `input_text` 按顺序
  以换行拼接成一个 `text`）。
  assistant 帧仍按 `payload.messageId` 聚合。
- item 投影同时给出 `id` 与 `itemId`（同值，`itim_...`）；`seq` 仍是唯一分页与续读
  游标，不能用 `id` 去重 delta 帧。
- SSE 帧只有 `item` 带 `id`（等于 `seq`）；`turn`、官方 `agent.session.*` 与 `gap`
  不持久化，不推进 `Last-Event-ID`。实时段的 `turn` 先发 `started`，终止帧是
  `completed` 或 `failed`；官方帧是增量兼容层，既有 `item`/`turn` 帧保持不变。
- 订阅游标接受 `Last-Event-ID` 头、`?afterSeq=` 或 `?after=`（三者等价，同时给出
  必须一致，否则 `400 conflicting_cursor`）。**没有任何游标时从当前末尾开始，只
  投递实时事件**：官方 `sessions.stream` 先订阅再 POST，若回放旧 turn 的终态，
  SDK 会在旧 turn 的 `idle` 上结束。显式 `afterSeq=0` 仍回放全部（既有客户端语义
  不变）。无游标订阅不读会话行：`POST /sessions?stream=true` 在行尚未创建时先
  打开订阅，行检查只属于显式游标（未知会话仍是 `404 unknown_agent`），否则流式
  create 会在行尚不存在时得到已关闭的订阅。
- 带游标时先补 `seq > after` 的 backlog，再转实时广播。心跳间隔由
  `PERI_SWARM_SSE_HEARTBEAT_MS` 控制。心跳 payload 的 `lastItemSeq` 是该会话当前
  `MAX(seq)`（与 item 写入方共享），只表示进度，不能代替已投递 item 的 `seq`
  作为恢复游标。
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
- 慢订阅者：广播容量耗尽（Lagged）时发一帧 `gap` 后关闭该订阅。丢失的事件可能
  包含终态，官方 SDK 因而以 EOF 识别中断；原生客户端用最后一个已渲染 item 的
  `seq` 重连恢复。流式 create 的首轮走同一通道：订阅先于首轮建立，容量内的首轮
  事件实时送达；首轮突发或客户端读取过慢超过容量时同样 `gap` + EOF，不静默丢弃
  （§4.3）。
- items 按 **item 分组**分页（§4.2.2）：`limit` 同时是 item 数上限与整页聚合行预算，
  `has_more` 按 item 计算，`nextCursor` 是本页最后一个 item 的最大 `seq`（仅
  `has_more=true` 时给出）。`items` 是本页这些 item 的全部原始行；`data` 是每个
  item 的最后一行（最大 `seq`）。`data` 通常比 `items` 短，两者数量允许不同；既有
  delta 消费者继续读 `items`。`after` 接受十进制 `seq`（与 `afterSeq` 同义），也
  接受官方 `after=<itemId>`：解析为该 id 的最大 `seq`，升序取 `MAX(seq) > after`、
  降序取 `MAX(seq) < after`；畸形 id 返回 `400 invalid_query`，与 `afterSeq` 不一致
  返回 `400 conflicting_cursor`。item 是事件级帧（user message、assistant delta、
  tool_call、tool_update、tool_result），不是官方高层 item；assistant 按
  `payload.messageId` 聚合，最终文本取 `payload.complete` 帧，`tool_call` 按
  `payload.toolCallId` 聚合（可能分带/不带 `input` 的两帧）。

#### 4.2.2 item 级分页

同一条 assistant 消息的 delta 与 `complete` 共享 `itemId`、`seq` 各不相同，所以
`after=<itemId>` 不能按行理解：

- item 归属按该 item 的最大 `seq` 判定，分页单位是 item；升序页是
  `MAX(seq) > after` 的前若干个 item，降序页是 `MAX(seq) < after` 的前若干个
  item。`limit=1` 且首条消息有 3 帧时，`items` 返回 3 行、`data` 返回 1 行——
  这正是「先取 `limit` 行再去重」会产生空页的场景。
- 分组在存储层完成：Turso 用 `GROUP BY logical_id, item_id` + `MAX(seq)` 选页，
  再按 `item_id` join 回全部行；内存 fake 用同一 `grouped_item_page` 规则，两者
  不会给出不同的页边界。
- **行预算**：`limit` 既是 item 数上限，也是整页聚合行预算。按 item 顺序累加原始
  行数，达到或超过 `limit` 行后不再纳入下一个 item；因此 `limit` 个 item 不保证
  都出现在一页里。单个 item 自身的行数超过预算时整条返回（至少纳入 1 个 item），
  否则会丢掉它的最终帧——这是已知边界；`items.list` 是 item 取向的接口，delta
  帧应走 SSE。
- `nextCursor` 是页内最后一个 item 的最大 `seq`，因此升序下一页严格前进；降序页
  的游标是页内最旧 item 的最大 `seq`，下一页只取更小的 item。`after=<itemId>`
  解析为该 item 的最大 `seq`，所以同一 item 不会被重复分页。十进制
  `after`/`afterSeq` 同样按 item 边界解释：游标落在多帧 item 中间时，该 item 整条
  出现在页里（`items` 含游标之前的帧，`data` 仍是它的最后一行）。
- **增长中的 item**：`after=<itemId>` 在请求时解析为该 item 的当前 `MAX(seq)`；
  之后追加的帧都落在这个 `seq` 之后，不会再由该游标返回（其他最大 `seq` 低于该值
  的 item 也一并被越过）。需要按最后已渲染行续读时用 `afterSeq=<seq>`：它同样按
  item 边界判定，会把包含该行的 item 整条重新给出（含新追加的帧），因此能看到
  最终帧。分页游标始终严格前进、不会循环。
- 默认 `order=asc` 与官方默认 `desc` 不同；保持既有客户端行为，不改默认值。

#### 4.2.1 官方事件映射

官方 `openai@7.60.0` Agents API 的事件帧是附加层：SSE `event:` 名与 JSON `type`
相同，均为 `agent.session.*`；既有 `item`/`turn` 客户端不受影响。

| 官方事件 | 发出时机 | 载荷来源 | `event_id` |
| --- | --- | --- | --- |
| `agent.session.turn.created` | turn 真正开始（`begin_turn`），**不是**受理时 | 打开 turn 的 user item | `evt_<turnId>_created` |
| `agent.session.turn.item.added` | user item 开启 turn/steer 时、assistant `complete:true` 落库后 | 持久化 item 的 `seq`、`itemId`、text | `evt_item_<seq>_added` |
| `agent.session.turn.completed` | 成功终态 | turn 的 `created_at` 与完成时刻 | `evt_<turnId>_completed` |
| `agent.session.turn.failed` | 失败终态 | 同上 | `evt_<turnId>_failed` |
| `agent.session.idle` | **紧接** `turn.completed` 之后 | 会话行快照 | `evt_<turnId>_idle` |
| `agent.session.failed` | **紧接** `turn.failed` 之后 | 会话行快照 + 错误 | `evt_<turnId>_session_failed` |

- `turnId = turn_<打开 turn 的 user itemId 去掉 itim_ 前缀>`。它由已持久化的 user
  item 派生，不用 ACP `messageId`，不含时钟或随机源；重连后由同一行重算，值不变。
- `event_id` 只由 `turnId` 或持久化 `seq` 派生，因此实时投递与 backlog 重放给出
  完全相同的 identity；客户端的 `event_id` 去重窗口可跨重连生效。
- 受理只持久化 user item，不发 `turn.created`/`turn.item.added`。两个帧在 turn 真正
  开始时按 `turn.created` → `turn.item.added` 顺序发出；从未执行的 turn 不会留下
  只有 created 没有终态的悬空 turn。
- `turn.item.added` 只投影 `message` item（user 与 assistant 的完成帧）；一个 turn
  可以落多条 assistant 完成帧（工具循环等），它们都属于同一个 `turn_id`。
  `receipt`、thought、tool 帧只存在于网关自定义面。`output_index` 恒为 `null`。
- backlog 按行重建 turn 边界：user item 在「没有打开的 turn」或「打开的 turn 已落
  assistant 完成帧」时开启新 turn，否则视为该 turn 的 steer；终态在下一 turn 的
  `turn.created` 之前或回放末尾发出。最后一个 turn 仅在会话行状态仍为 `failed`
  时补 `turn.failed` + `agent.session.failed`。回放仍只从 item 行重建，不读取
  `swarm_turns`：因此重放的失败 `error` 与 `completed_at` 保持 `null`，不把
  turn 行的事实混进既有帧；实时失败帧的 `turn.error` 由失败消息填充
  （`{code:"internal_error", message}`）。`turns.list`/`turns.retrieve` 读的是
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
- `agent.session.idle`/`agent.session.failed` 的 `metadata` 来自会话行；官方
  `AgentSession.prompt_cache_options` 是可选项，网关只在有持久值时发送它（没有值
  时字段不出现，而不是伪造 `null`）。
- `usage` 恒为 `null`；`requires_action` 永不发送（本地没有待审批事实）；
  `turn.cancelled`、budget、subagent 与官方 `function_call` item 不在本层。


### 4.3 创建与首轮

`POST /sessions` 接受 OpenAI Agents API 形状的 `model`/`instructions`（顶层与
`agent` 内同名字段必须一致）、`input`、`environment`、`tools` 与 `stream`。
`stream:true` 时网关先为 `logicalId` 打开订阅；service 受理初始输入后立即返回
（actor 在 action reply 前用 `ctx.wait_until` 注册首轮，in-process fake 由 handler
`tokio::spawn`，与 `POST /events` 的 `202` 相同），网关再构造并返回 SSE 响应
（headers 与初始 `session` 帧）。首轮事件在广播容量内实时送达；客户端读取过慢或
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
的行状态：这些检查只适用于已 live 的 runtime；崩溃遗留的 `running` 行仍由
`ensure_live` 返回 `502 session_resume_required`，turn 恢复尚未实现（§7、验收 §5-5）。
非流式创建不延迟：首轮仍在 `create_session` 内执行，错误仍以 HTTP 状态返回；
流式创建的首轮错误只出现在流上。

首次创建的写入顺序固定：`bind_environment`（hosted 时预留并持久化绑定）→ 写入
`state=creating` 的完整会话行（含 `rootSessionId`、`cwd`、model、instructions、
`mcp_tools`、environment 与时间戳）→ `session/new` → 同一行改 `idle` → 首轮
（流式创建在 `idle` 之后先建立订阅并构造/返回 SSE 响应，再调度首轮，见上）。
`created=true` 只出现在首次创建；命中已存在行（无论 `idle` 还是 `creating`）都返回
`created=false`。行状态语义：`creating` 表示身份已持久化但 Peri 会话尚未确认，
`idle`/`running` 表示 host 已就绪；`creating` 不是「创建失败」，而是可重试的中间态。

### 4.4 幂等语义

- 输入受理需要稳定 `inputId`（body 或 `Idempotency-Key`，两处必须一致；原生 body
  路径服务端不代生成）。同一 key 与同一内容重放返回同一受理，`disposition=existing`；
  同一 key 不同内容返回 `409 input_conflict`，且不入队。官方 `events` 信封的
  carve-out：信封没有 body `inputId`，有 `Idempotency-Key` 时直接作为 `inputId`
  且必须是 UUID；无 key 时由服务端生成一次性 `Uuid::now_v7()`。官方 API 本就不保证
  无 key 重试幂等，因此该生成不构成重试承诺；原生 body 路径缺少 `inputId` 仍返回
  `400`。
- 受理是逐条的：序列按受理顺序追加，`seq` 前缀可核对；**不**承诺多事件整批全有
  或全无。官方信封本轮只接受单条事件，仍按单条受理。`kind=user` 的受理同时追加
  一条 user message item；同一 key 与内容的重放返回 `existing`，不重复追加。
- `POST /sessions` 的 `Idempotency-Key` 只用于稳定 `logicalId` 并复用已存在会话，
  不是跨请求的事务承诺。
- `POST /sessions/{logicalId}/events` 的 `202` 是**受理**回执：input receipt、user
  item 与 turn 预留都持久化后才返回，turn 在响应之后执行（actor 路径经
  `wait_until` 调度，HTTP 路径用独立任务；与流式 create 一样存在 action reply 与
  响应构造之间的短窗口，见 §4.3）。响应中的 `lastItemSeq` 是受理时刻的
  `MAX(seq)`，通常只含本条 user item；assistant item 与终态经 SSE 投递。因此
  「先建流、后 POST」仍是观察本轮的可靠方式，但 202 不再等待模型。
- 同步失败仍按原语义返回：空输入/畸形请求 `400`，同 key 不同内容
  `409 input_conflict`，需要 `session/resume` 的失败 `502 session_resume_required`
  （`details.operation` 指向具体阶段）。turn 执行失败在流上以 `turn.failed` +
  `agent.session.failed` 呈现，不追认改已返回的 `202`。
- 创建中断的恢复是**按已持久化的 root id 核对**，不是重新生成：`creating` 行重试
  先 `session/load`、必要时才用同一 root id `session/new`，返回 `created=false`；
  初始输入至多执行一次（行内已有 item 时不重放；`last_seq == 0` 但已有在途 turn
  时按 §4.3 返回 `409 turn_in_progress`，不静默跳过）。两次尝试都失败返回可诊断的
  `502`（`details.operation=create_recovery_failed`），行与 root id 保留，可继续
  重试。ACP 返回的 session id 与行内 root id 不一致时以 `session_id_mismatch`
  失败，不接管该身份。
- `sessions.update` 与 `sessions.delete` 复用同一准入判定（`ensure_live` 同一判定位置）：
  - 有 live runtime 且 turn 正在运行或已有未关闭 turn：`409 turn_in_progress`；
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

`GET /sessions/{logicalId}`（以及创建响应）的 `environment` 是
`{type,id}`；hosted 会话另有 `sandbox` 投影，字段来自 `swarm_bindings`：
`logicalEnvId`、`sandboxId`、`runId`、`workspacePath`。`runId` 在绑定记录后如实
填充；`state` 固定为 `null`：网关只持久化绑定，不跟踪 Bun 的实时 SRT 启停，表里
也没有该列，所以不伪造 `reserved`/`running`。实时状态以 Bun
`GET /v1/sandboxes/{logicalEnvId}` 为准。会话 `cwd` 是网关工作目录，不是工具执行
目录；工具在 `sandbox.workspacePath` 下执行。

`status` 由会话行与进程内在途 turn 投影：有在途 turn（含已受理未开始）为
`in_progress`，行状态 `failed` 为 `failed`，否则 `idle`。本地没有审批事实，不会
出现 `requires_action`。

会话投影同时携带官方 `metadata`（`string -> string`，空对象表示无）与
`prompt_cache_options`（`{ttl:"30m"|"12h"}` 或 `null`）。`prompt_cache_options` 是
官方兼容字段，使用官方 snake_case 拼写（`AgentSession.prompt_cache_options`）；
其余 Swarm 扩展字段仍为 camelCase。create 接受两者：metadata 最多 16 对、
key 1..=64 字符、value ≤512 字符；`prompt_cache_options.ttl` 只接受 `30m`/`12h`，
`{}` 视为未配置。重复 create 命中已存在行时，省略的 metadata/prompt cache 保持
行内值，不参与冲突判定；显式提供的值（含 `{}`/`null`）必须与行内一致，否则按身份
冲突 `409 identity_conflict` 拒绝，不静默覆盖。`agent.model` 是官方 string；显式
`null` 返回 `400 invalid_request`（`details.field="agent.model"`），不作为省略处理。
`agent.tools: null` 仍是合法的清空声明。


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
  `PERI_SWARM_STORE_TOKEN_ENV`（变量名，不是值）、`PERI_SWARM_MACHINE_ID`、
  `PERI_SWARM_WORKSPACE_ROOT`；Bun 桥 `PERI_SWARM_BUN_URL`（或
  `PERI_SWARM_BUN_BASE_URL`）与 `PERI_SWARM_BUN_TOKEN` 可选，未配置时
  `environment.type=openai_hosted` 在创建时显式失败，`none` 不受影响。

Loom：

- 本地开发用 Loom 的 loopback HTTP 模式（`PERI_HRANA_EXPERIMENTAL=1`）；Rust 存储
  SDK 自建 `reqwest::Client`，无法注入 fixture CA 或关闭校验，因此不支持 Loom TLS。
- `dev.py` 只种 Peri 会话库（`local_default/demo/main`）；Swarm 状态库
  （`local_default/swarm/main`）由 `peri-swarm/scripts/provision_state_db.py` 通过平台
  管理 API 创建，并对同 `request_id` 幂等。不同 `request_id` 指向已存在身份会显式
  失败，不静默复用。

Rivet：

- 本地 engine 为 Rivet 3.1.1（`~/.rivetkit/engine/v3`）；默认 endpoint
  `http://127.0.0.1:6420`、namespace `default`、pool `peri-swarm`，默认
  `PERI_SWARM_RIVET_ENGINE_SPAWN=always` 在 `RIVETKIT_ENGINE_AUTO_DOWNLOAD=1` 时按需
  拉起本地 engine。副本组的进程只有共享同一 engine、namespace、pool、两个 Loom 库
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
  endpoint，矛盾在解析阶段失败。`never` 时进程在打开两个 Loom 库之前做一次 5 秒
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

允许的故障域是「只杀 Rust 宿主」：Loom、Bun、SRT、模型与 Rivet engine 保留。

- 新进程启动时，actor 的 `on_start` 只恢复只读视图（会话行与 `MAX(seq)`）；`GET`
  读取不装载 host、不触达 Bun。
- 第一次交互（`POST /events`，或对已存在行重复 create）触发 `ensure_live`：
  重建 host，`initialize` 后用持久 `rootSessionId`、`cwd` 与同一组 `mcpServers`
  执行 `session/load`；不写 `_meta`，不新铸 root id。
- workspace 绑定先读 Loom；缺失即失败，不新建环境。Bun 侧只做懒 `reserve`；
  `sandboxId`/`workspacePath` 变化拒绝恢复。
- 明确禁止：调用 `session/resume`、向恢复会话重发 prompt、重放 MQ 输入、把
  `session/load` 的回放帧写入 `swarm_items`、或为丢失绑定静默换 workspace。
- 持久行 `state=running`，或 `swarm_turns` 仍有开放行（`queued`/`in_progress`/
  `waiting`）时，`ensure_live` 返回 `502 session_resume_required`：崩溃中未完成
  turn 的续跑仍未实现，也没有 `state=recovering` 状态。此时遗留的 `swarm_turns`
  开放行（`queued`/`in_progress`/`waiting`）被关闭为 `failed`
  （`completed_at` 与 `error_json={code:"internal_error",
  message:"the turn was interrupted before it reached a terminal"}`）；`turns.list`/
  `turns.retrieve` 的读路径在持久行仍记录未完成 turn 且没有 live turn 时执行同一
  关闭，因此客户端不会看到永久 `in_progress` 的 phantom turn。
- `sessions.update`/`sessions.delete` 在无 live runtime 时使用同一判定：持久行
  `running` 或存在开放 turn 行都返回 `502 session_resume_required`，不会先修改或
  删除再报错；只有 live runtime 内的在途 turn 返回 `409 turn_in_progress`。
- 创建中断（行状态 `creating`）走独立的核对路径：先 `session/load`，只有当 Peri
  会话确实不存在时才用同一 root id `session/new`，冲突后再试一次 attach；两条路
  都失败返回 `502 create_recovery_failed` 并保留行。`ensure_live` 不把 `creating`
  当作 `running` 拒绝，它只拒绝已记录未完成 turn 的行。

历史只读不等于命令重放：恢复后的 `session/load` 会执行装配期探测（Bun 日志可见
`resources/list`），但不会执行任何历史工具命令；旧 workspace 文件的哈希与 mtime
保持不变是直接证据。

副本组：

- actor 无状态且空闲 300s 后由 engine 回收，下一次交互可能在**任一**副本唤醒；
  跨副本重建沿用上面的只读恢复与 `session/load` 语义，不新增在途 turn 接管。
- 因此所有副本必须使用同一 `PERI_SWARM_MACHINE_ID`、同一绝对路径的
  `PERI_SWARM_WORKSPACE_ROOT`（共享卷或同路径挂载）、可写的两个 Loom 库，并保证
  Bun（hosted）从每个副本可达；actor 重建副本上的 `cwd` 必须本机存在。
- 持久行仍为 `running` 或仍有开放 turn 行时照旧返回 `502 session_resume_required`；
  滚动停副本与在途 turn 的处置不在本层，见 §8。

## 8. 未完成项与风险

1. **后台任务引用**：Bun 转发层尚未识别 `resultType:"task"` 与 `tasks/get` 终态，
   空闲回收也不看任务台账。turn 在最后一个工具调用后长时间输出时，沙箱可能已
   回收，随后代理调用得到 `404 sandbox_not_found`，需要重新 ensure。
2. **未完成 turn 与并发 reattach**：`session/resume` 未实现；崩溃遗留的 turn 行会
  被关闭为 `failed`，但真正的 turn 续跑、并发 reattach、Bun 与 Loom 同时不可用时
  的失败文案、以及「崩溃时正在执行命令」的中断语义未覆盖。
3. **真实部署模型任务**：模型侧全部为确定性 stub；真实供应商的 tool call、长任务
   与多轮副作用未验收。
4. **1000 Agent 规模与单 engine 容量**：没有吞吐、并发会话、单 engine 节点的容量或
   多副本压力证据；actor 的并发 SSE 观看者与 engine 池容量都未经真实压力验证。
5. **生产 TLS 与入口鉴权**：网关默认只绑定 loopback；非 loopback 绑定（含
   `0.0.0.0`）需要 `PERI_SWARM_BIND_ALLOW_NON_LOOPBACK` 显式开关，启动时输出 WARN；
   Bearer 是唯一鉴权、TLS 必须由受信任入口终止，组合后的链路未验收。Rivet endpoint
   只接受私网/环回字面地址或由部署保证指向私网/环回的域名，且只支持明文 HTTP；Loom
   侧只能用 loopback HTTP。生产 TLS 与证书轮换未实现。
6. **Linux SRT**：`peri-sandbox` 有 `systemd-run` 路径，但本链路只在 macOS 的
   `sandbox-exec` 上验收；Linux 沙箱与回收行为未验证。
7. **真实模型工具调用**：工具 wire、冷/热分界与结果回填已在 stub 驱动下验证；
   真实模型是否会选择工具、如何重试与失败恢复未验证。
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
14. **官方会话资源面的活体验证**：`swarm_turns` 建表、既有库的 `ALTER` 迁移与
    `sessions.list` 的 `json_each` metadata 过滤只在单测中验证 SQL/参数形状，未在
    真实 Loom 上执行；官方 `openai@7.60.0` 客户端进程尚未消费
    `sessions.list/update/delete/turns`。
