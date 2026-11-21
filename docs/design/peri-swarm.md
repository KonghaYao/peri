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

- 不做跨机调度、租约或 fencing；本地部署只绑定 loopback，使用单个部署 Bearer。
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
| Swarm 状态库 | `peri-swarm`（`store::SwarmStateStore`） | `swarm_meta.version=1` | `swarm_sessions`、`swarm_items`、`swarm_inputs`、`swarm_bindings` 与索引 |

- 两个 locator 都是必填且必须不同；同一数据库、缺失 locator 或任一库打不开都会让
  进程启动失败，不复用另一库、不降级。
- Peri 会话库在打开时执行 Peri schema 检查/升级；未知或更高版本 fail closed。
- Swarm 状态库自带 DDL 与版本判定：已存在 `swarm_*` 表但没有 `swarm_meta` 时拒绝
  接管，未知版本直接中止。
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

### 4.1 五个方法与门禁

路由只在 `peri-swarm/src/http` 定义，`/healthz` 是唯一不鉴权的常量探针。

| 方法 | 路径 | 语义 |
| --- | --- | --- |
| `POST` | `/v1/agents/sessions` | 创建或复用会话；`stream:true` 先打开订阅再执行首轮 |
| `GET` | `/v1/agents/sessions/{logicalId}` | 读取会话投影 |
| `POST` | `/v1/agents/sessions/{logicalId}/events` | 提交一条输入（`mode=turn` 或控制输入） |
| `GET` | `/v1/agents/sessions/{logicalId}/events` | SSE：先补 backlog 再转实时 |
| `GET` | `/v1/agents/sessions/{logicalId}/items` | 按 seq 增量读取 |

约定：

- `Authorization: Bearer <部署 key>` 常量时间比较；失败 `401` 并带
  `WWW-Authenticate: Bearer`。
- `OpenAI-Beta: agents=v1` 可选，其他值 `400 unsupported_beta`；请求可用
  `x-peri-swarm-expect: 1` 探测版本，所有响应回写 `x-peri-swarm-contract: 1`。
- 请求体上限 8 MiB；`text` 1 MiB、`instructions` 64 KiB、`model` 256 B 由显式检查
  返回 `400`。错误体使用统一信封与稳定 `code`（`invalid_input`、`input_conflict`、
  `session_resume_required`、`upstream_unavailable` 等）。
- `cwd` 缺省为 `<workspaceRoot>/<logicalId>`；显式 `cwd` 必须存在且规范化，落在
  虚拟 workspace 根内。

### 4.2 SSE 与 items

- 每个 item 持久化为一行，`seq` 由 Swarm 状态库单调分配；先落库后广播，订阅者
  不存在也不影响执行。
- 用户输入在 `swarm_inputs` 受理（`disposition=inserted`）后写成一条单帧
  `message` item：`payload={role:"user",text,inputId}`，`id`/`itemId` 为
  `itim_input_<inputId>`，没有 `delta`/`complete` 字段。host 的
  `user_message_chunk` 回显不投影，因此同一 `inputId` 重试不会重复；`create` 的
  `input` 同样写一条（每次 create 调用各一条；对已 `idle` 会话的重试 create 仍按
  既有语义重跑该轮，`creating` 行的恢复不重放输入，见 §4.4）。
  assistant 帧仍按 `payload.messageId` 聚合。
- item 投影同时给出 `id` 与 `itemId`（同值，`itim_...`）；`seq` 仍是唯一分页与续读
  游标，不能用 `id` 去重 delta 帧。
- SSE 帧只有 `item` 带 `id`（等于 `seq`）；`turn` 与 `gap` 不持久化，不推进
  `Last-Event-ID`。实时段的 `turn` 先发 `started`，终止帧是 `completed` 或
  `failed`。
- 重连游标接受 `Last-Event-ID` 头或 `?afterSeq=`；服务端先补 `seq > after` 的
  backlog，再转实时广播。心跳间隔由 `PERI_SWARM_SSE_HEARTBEAT_MS` 控制。心跳
  payload 的 `lastItemSeq` 是该会话当前 `MAX(seq)`（与 item 写入方共享），只表示
  进度，不能代替已投递 item 的 `seq` 作为恢复游标。
- 回放（backlog）只发 `item` 帧，没有 `turn` 控制帧；客户端的回放终止条件是目标
  message 的 `payload.complete=true`。`after` 已等于或超过会话末尾时没有可回放
  item，流保持打开并只发心跳；客户端必须自行超时或取消（或改用 items 分页）。
- items 支持 `order=asc|desc`、`after`、`limit`，响应给出 `items`、`hasMore`、
  `nextCursor`、`lastItemSeq`；同一 assistant 消息的 delta 与 complete 共享稳定
  `itemId`。item 是事件级帧（user message、assistant delta、tool_call、
  tool_update、tool_result），不是官方高层 item；assistant 按
  `payload.messageId` 聚合，最终文本取 `payload.complete` 帧，`tool_call` 按
  `payload.toolCallId` 聚合（可能分带/不带 `input` 的两帧）。

### 4.3 创建与首轮

`POST /sessions` 接受 OpenAI Agents API 形状的 `model`/`instructions`（顶层与
`agent` 内同名字段必须一致）、`input`、`environment`、`tools` 与 `stream`。
`stream:true` 时网关先为 `logicalId` 打开订阅，再执行首轮，因此首轮事件不会丢；
非流式创建返回 `created` 与会话投影。

首次创建的写入顺序固定：`bind_environment`（hosted 时预留并持久化绑定）→ 写入
`state=creating` 的完整会话行（含 `rootSessionId`、`cwd`、model、instructions、
`mcp_tools`、environment 与时间戳）→ `session/new` → 同一行改 `idle` → 首轮。
`created=true` 只出现在首次创建；命中已存在行（无论 `idle` 还是 `creating`）都返回
`created=false`。行状态语义：`creating` 表示身份已持久化但 Peri 会话尚未确认，
`idle`/`running` 表示 host 已就绪；`creating` 不是「创建失败」，而是可重试的中间态。

### 4.4 幂等语义

- 输入受理需要稳定 `inputId`（body 或 `Idempotency-Key`，两处必须一致；服务端不
  代生成）。同一 key 与同一内容重放返回同一受理，`disposition=existing`；同一 key
  不同内容返回 `409 input_conflict`，且不入队。
- 受理是逐条的：序列按受理顺序追加，`seq` 前缀可核对；**不**承诺多事件整批全有
  或全无。`kind=user` 的受理同时追加一条 user message item；同一 key 与内容的重放
  返回 `existing`，不重复追加。
- `POST /sessions` 的 `Idempotency-Key` 只用于稳定 `logicalId` 并复用已存在会话，
  不是跨请求的事务承诺。
- 创建中断的恢复是**按已持久化的 root id 核对**，不是重新生成：`creating` 行重试
  先 `session/load`、必要时才用同一 root id `session/new`，返回 `created=false`；
  初始输入至多执行一次（行内已有 item 时不重放）。两次尝试都失败返回可诊断的
  `502`（`details.operation=create_recovery_failed`），行与 root id 保留，可继续
  重试。ACP 返回的 session id 与行内 root id 不一致时以 `session_id_mismatch`
  失败，不接管该身份。

### 4.5 会话投影与环境

`GET /sessions/{logicalId}`（以及创建响应）的 `environment` 是
`{type,id}`；hosted 会话另有 `sandbox` 投影，字段来自 `swarm_bindings`：
`logicalEnvId`、`sandboxId`、`runId`、`workspacePath`。`state` 固定为 `null`：
网关只持久化绑定，不跟踪 Bun 的实时 SRT 启停；实时状态以 Bun
`GET /v1/sandboxes/{logicalEnvId}` 为准。会话 `cwd` 是网关工作目录，不是工具执行
目录；工具在 `sandbox.workspacePath` 下执行。

`POST /sessions/{logicalId}/events` 在 HTTP 层返回 `202`，但 `mode=turn` 的受理在
本轮进入终态后才返回；响应中的 `lastItemSeq` 已包含本轮 item。对调用方而言，**返回
时本轮可能已经完成**；它不是「排队后异步执行」，也不能在返回后再开流补看本轮——
要观察本轮，先建立 SSE 再 POST。未完成 turn、`session/resume` 与后台任务边界仍是
§8 的未完成项。

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

- 本地 engine 为 Rivet 3.1.1（`~/.rivetkit/engine/v3`），网关在
  `RIVETKIT_ENGINE_AUTO_DOWNLOAD=1` 时按需拉起，endpoint 固定
  `http://127.0.0.1:6420`；pool/部署身份由进程配置提供，两个进程只有共享同一 engine
  与部署身份时才看到同一批 actor。

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
- 行状态为 `running` 时 `ensure_live` 返回 `502 session_resume_required`：崩溃中
  未完成 turn 的续跑仍未实现，也没有 `state=recovering` 状态。
- 创建中断（行状态 `creating`）走独立的核对路径：先 `session/load`，只有当 Peri
  会话确实不存在时才用同一 root id `session/new`，冲突后再试一次 attach；两条路
  都失败返回 `502 create_recovery_failed` 并保留行。`ensure_live` 不把 `creating`
  当作 `running` 拒绝，它只拒绝已记录未完成 turn 的行。

历史只读不等于命令重放：恢复后的 `session/load` 会执行装配期探测（Bun 日志可见
`resources/list`），但不会执行任何历史工具命令；旧 workspace 文件的哈希与 mtime
保持不变是直接证据。

## 8. 未完成项与风险

1. **后台任务引用**：Bun 转发层尚未识别 `resultType:"task"` 与 `tasks/get` 终态，
   空闲回收也不看任务台账。turn 在最后一个工具调用后长时间输出时，沙箱可能已
   回收，随后代理调用得到 `404 sandbox_not_found`，需要重新 ensure。
2. **未完成 turn 与并发 reattach**：`session/resume` 未实现；并发 reattach、Bun 与
   Loom 同时不可用时的失败文案、以及「崩溃时正在执行命令」的中断语义未覆盖。
3. **真实部署模型任务**：模型侧全部为确定性 stub；真实供应商的 tool call、长任务
   与多轮副作用未验收。
4. **1000 Agent 规模**：没有吞吐、并发会话或单 engine 容量的运行证据；actor 的并发
   SSE 观看者与 engine 池容量都未经真实压力验证。
5. **生产 TLS**：网关只绑定 loopback，Bearer 是唯一鉴权；Loom 侧只能用 loopback
   HTTP，生产 TLS 与证书轮换未实现。
6. **Linux SRT**：`peri-sandbox` 有 `systemd-run` 路径，但本链路只在 macOS 的
   `sandbox-exec` 上验收；Linux 沙箱与回收行为未验证。
7. **真实模型工具调用**：工具 wire、冷/热分界与结果回填已在 stub 驱动下验证；
   真实模型是否会选择工具、如何重试与失败恢复未验证。
8. **调用方第三方 MCP**：注入形状与 `session/load` 复用有集成测试，但没有真实第三方
   MCP server 的端到端运行。
9. **冷态可见性降级**：hosted 会话冻结的 system prompt 不含远端 workspace 指令与
   覆盖（冷期不可得），启动后资源发现也不刷新句柄快照。
10. **整批原子性**：逐条受理的语义固定，不提供批量事务或整批回执。
