# @peri-code/rivet — 设计

## 1. 定位

本包是 [Rivet](https://rivet.dev) actor 与 Peri SDK 之间的对接层，不实现第三个运行时：

| 能力 | 归属 |
| --- | --- |
| actor 身份、action 路由、唤醒/睡眠、部署与扩缩容 | **Rivet** |
| Chat 句柄、Session、ACP 运行后端、动作/事实/观测 | **Peri SDK** |
| 活动投影、keep-awake、sleep/wake 接线、resource 查询与 MCP | **本包** |

两条主链路：

```text
Rivet action ──► PeriChatActor ──► ChatActivation ──► ChatService (Peri SDK)
                       │                 │
                       │                 └── facts / observe ──► ChatActivationActivity ──► ActivityLedger
                       ├── open ──► createChatSession ──► ChatDatabase (per activation)
                       └── c.keepAwake ◄── KeepAwakeCoordinator

HTTP MCP ──► ResourceQueries ──► Store
```

`@peri-code/rivet`（根入口）不依赖 `rivetkit`；Rivet 只在 `@peri-code/rivet/rivet` 入口出现。领域逻辑可以脱离 Rivet 单测，Rivet 适配层只做接线与边界转换。

## 2. 分层与依赖方向

```text
rivet/* ──► host/chat-activity, host/keep-awake, mcp/http, resource/queries, identity, errors
   │        @peri-code/sdk（createChatSession、ChatService、ownership）
   └──► rivetkit（peer dependency）

host/chat-activity ──► host/activity
host/keep-awake ─────► host/activity

rivet/chat-session ──► openDatabase + createChatSession（一次激活一个 ChatDatabase）
rivet/chat-session-activity ──► host/chat-activity + host/keep-awake
rivet/sandbox-actor ──► contracts/errors + contracts/identity + util/deferred（不依赖 SDK）
rivet/sandbox-mcp-actor ──► rivet/sandbox-actor + rivet/workspace-uri（同一引用计数核心）
sandbox/peri-sandbox-runtime ──► rivet/workspace-uri + peri-sandbox（独立 `/sandbox` 入口）
```

约束：

1. `host`、`resource`、`mcp` 不感知 Rivet；只有 `rivet/*` 导入 `rivetkit`。
2. `mcp` 不保存调度状态；`resource` 不感知 Chat；actor 不持久化执行状态。
3. `ActorContext` 不进入 SDK 公共 API。适配层只取 `c.actorId`、`c.key`、`c.abortSignal`、`c.vars`、`c.keepAwake` 与 `c.broadcast`。

### 运行时资源归属

Rivet 的 `vars` 是激活级数据，`createVars` 只有 5s 预算，因此共享客户端不能放进 actor：

| 资源 | 作用域 | 持有者 | 关闭时机 |
| --- | --- | --- | --- |
| `ownership` 端口、`openDatabase` 端口 | 进程 / isolate | 宿主组合根，经 `createPeriChatActor` 注入 | 进程退出或 isolate 丢弃；actor 不关闭 |
| 宿主资源 owner | 进程 | 组合根创建并强持有的 `HostResourceOwner`，显式注入 Chat/Sandbox 工厂 | 宿主 `beginShutdown()` + `closeAll()`；不随 activation vars 消失 |
| `ChatService`、ChatDatabase | 一次 `open` | owner 记录；`c.vars` 只是 action 面引用 | `close` action、`onSleep`、`onDestroy`、宿主 `closeAll` |
| 活动投影、事实订阅、keep-awake | 一次 activation | owner 记录的 detach 回调（只在不退役期间持有 `c` 回调） | retire/close 时先解绑；真实 SDK/DB 继续归 owner |
| 共享 KV（ownership 记录） | 跨实例 | 宿主提供的 `ChatOwnership` | actor 只在 `open` 前清一条陈旧记录 |
| sandbox 运行实例 | 进程，按 workspace URI 分片（兼容入口按 `sandboxId`） | 宿主提供的 `SandboxRuntimePort` / `SandboxMcpRuntimePort`；owner 记录 gate 与关闭 closure | refs 归零、`closeAll`、sleep/destroy、宿主 `closeAll`；进程级回收由宿主在自己的 runtime 上完成 |

owner 的 key 沿用现有 actor key（Chat 为 `[chatId]`，Sandbox 为 `['sbx', workspaceUri]`），`activationRef` 只是内存对象身份，用来区分新旧回调，不是新的业务 ID。内部是 owner 实例私有的 Map，不持久化、不保存可重放执行状态，也没有模块级孤儿表。

本包不提供隐藏单例：`openDatabase`、`ownership` 与 `HostResourceOwner` 都由宿主显式持有；actor 只在一次激活内使用它们，未确认关闭留在 owner，由同一宿主进程枚举和重试。

### Sandbox actor

`rivet/sandbox-actor.ts` 是单个 sandbox 资源引用计数的唯一权威：`vars.holders` 是唯一状态，
`refs` 恒等于其长度，同一 holder 重复 acquire 不重复计数。端口
`SandboxRuntimePort { start, close, stateOf }` 只做 spawn/close/只读状态，不持有计数；
`runtime` 可选，缺省时 actor 仍可注册，`acquire` 返回 `sandbox_unavailable`。
旧 `createSandboxActor` 以 `sandboxId` 为身份保留为兼容入口，仍用同一核心。

`rivet/sandbox-mcp-actor.ts` 把同一核心的 resource 身份换成 canonical workspace URI，
key 固定为 `['sbx', workspaceUri]`，并新增 `SandboxMcpRuntimePort.fetchMcp(uri, request)`。
`machineId`、`authorizedWorkspaces` 与 `token` 都来自部署组合根；key/URI 只是定位符，
不是授权或凭据。每次 action 先校验凭据，再用 `parseWorkspaceUri` 校验 canonical 形式、
同机与 allowlist，全部通过后才触达 runtime/start/close/state。

`/mcp` HTTP 只接受显式 MCP path，要求当前有 active holder，不隐式 acquire。转发请求用 URL + init
显式构造；网关凭据（`authorization`/`cookie`/`proxy-authorization`/`x-rivet-token`）与 actor 凭据
（`x-peri-sandbox-token`）在构造前移除，不依赖 `new Request(request, { headers })` 的空 Headers
覆盖语义；内部 MCP 使用 `peri-sandbox` 实例自己的 Bearer token。最后一个
release（以及 `closeAll`/sleep/destroy）先关闭新入口，abort 并主动 cancel 实际 response
reader/stream，只有 cancel resolve 或取消前已观察到 read done/error 才算 settled；cancel
reject 会保留 inflight/holder 并记 `drain_failed`，不会把「调用过 cancel」当作确认，也不对
已经结束的 stream 再发一次 cancel 制造伪确认。收束未确认时返回
`sandbox_close_incomplete`（`drain_failed`）并保留 holder/实例供重试，不返回假关闭。
late response 同样走该收束，cancel 失败不 `finishRequest`。gate、inflight 与关闭 closure
登记在宿主 owner 的记录里，而不是 `vars` 的 WeakMap：Rivet 清掉 vars 后，同 key 的新
activation 仍要先关闭旧记录，旧 gate 未清时 HTTP 返回 `sandbox_closing`，不会启动第二个环境。
关闭 closure 只捕获 vars、runtime 与 gate，不捕获 action context 或 keep-awake 回调。
`onBeforeConnect` 对带请求头的连接复用同一凭据校验。

真实宿主适配器在 `sandbox/peri-sandbox-runtime.ts`，经独立 `@peri-code/rivet/sandbox`
入口导出：它只把 canonical URI 逐字绑定到 host 认可的 0700 真实目录，并以显式
`workspacePath` 调用 `peri-sandbox`；不把 URI 哈希/截断/编码成 `sessionKey` 或
`sandboxId`，也不 root-join 工作区。`peri-sandbox` 的 `sbx_...` 只是进程/状态目录句柄，
仅用于资源控制，不是查找、授权或恢复的第二身份。授权目录与 `sandboxesRoot` 双向不重叠，
URI 路径必须与授权的真实目录逐字相同（symlink/realpath 别名不授权）。`peri-sandbox` 是
可选 peer，Chat 入口不导入它。

`closeAll` 只关闭本 resource（幂等、不启动、不碰其它 resource）；进程级回收由宿主在自己的
运行端口上完成（适配器额外提供 `closeAll()` 供宿主退出），端口契约不定义该动作。失败统一返回
`PeriErrorPayload`（`sandbox_*` / `invalid_identity` / `invalid_input` / `internal_error`），
端口错误只在 `onDiagnostic` 出口以凭据脱敏后的安全投影出现；已知 actor token 不进入
message/stack/cause，也不跨 actor 或进入 action/HTTP 响应。

引用只在 activation 内有效：`refs > 0` 时持有一个 keep-awake hold，归零释放；lease 在 owner
`retire`/`beginShutdown`/`closeAll` 时解绑该 hold，即使资源 close 失败也不会留下失效回调；
sleep/destroy 上报 `holders_lost_on_teardown` 后由 owner 记录执行收束与关闭，未确认则保留 gate/
实例供宿主重试。`retire(vars)` 是 activation 身份的终态：即使随后的确认关闭删除了 owner record，
同一 vars 的 `open` 仍被拒绝；同 key 的新 activation 在旧资源确认关闭后可以正常打开。`beginShutdown` 是准入线性化点：等待旧 lease close 后、进入 create 前重新校验，
此后不再有新的 create/DB reset/hold；action 在 core refs 队列内再次校验 lease retired，避免
取得 lease 后排队期间越过 owner 关闭。`status.orphaned` 只读地暴露「refs=0 但实例仍在」，监控
读取不产生副作用，清理留给 `closeAll`；owner 的 `closeAll` 与 runtime 的进程级 `closeAll()` 是
两条独立结果，不能用一个成功推断另一个；适配器兜底 `runtime.closeAll()` 只在 owner 确认排空后
调用，临时 state 目录仅在 owner 与适配器关闭都确认后删除。

## 3. 身份与边界

| 身份 | 生命周期 | 语义 |
| --- | --- | --- |
| actor id / key | Rivet actor | chat 为 `[chatId]`，Sandbox MCP 为 `[sbx, workspaceUri]`；兼容入口为 `[sbx, sandboxId]`；本包不解释其它业务 key |
| `RootSessionId` | 会话 | Peri 顶层会话身份，由现有会话模型拥有 |
| `ResourceId` | 资源读取 | 独立资源身份，不绑定客户端连接 |
| `ResourceScope` | 授权范围 | resource 查询键的一部分；只来自认证边界，防止跨租户读取 |
| `InputId` | 单次输入 | 宿主内去重与诊断关联 |

服务端身份（`src/identity.ts`、`src/contracts/server-identity.ts`）在边界处解析：非空、不含空白与控制字符；一般 id 长度不超过 200，`ResourceScope` 不超过 256。解析失败抛 `ActorError('invalid_identity')`；公开契约身份（`/contracts`）解析失败抛 `PeriError('invalid_identity')`。

**身份不互相替代**：`rootSessionId` 不当作 actor 身份，MCP session ID 不参与资源定位。

Chat 路径上有三个容易混用的 id，路由只用第一个：

| 身份 | 来源 | 用途 |
| --- | --- | --- |
| actor key / `target.chatId` | 客户端生成 | `getOrCreate` 的事件路由、ownership 键、`open` 的一致性检查 |
| `ChatBindingTarget.chatId` / `SessionReadyResult.chatId` | SDK 内部 chat 句柄 id | 出现在 `execute` 回执与 facts 信封；不能当路由键 |
| `sessionId` | ACP Session | 会话内事实分组；`change_session` 可改变，不能当路由键 |

`ChatOpenResult.chatId` 原样回显 `target.chatId`（actor key）。

### Workspace 边界

`open.workspace` 只接受 canonical `peri+sandbox://` 字符串，来源是 `WorkspaceURI.toString()`。actor 用 `parseWorkspaceUri` 严格校验（不接受大写 scheme/UUID、原始空格或路径归一化以外的写法），再把字符串转成 SDK 内部的 `WorkspaceRef`：`id` 就是 canonical URI 本身，`runtimePath` 是解析出的路径；不哈希、不映射、不拼 root。

机器身份只来自部署组合根的 `createPeriChatActor({ machineId })`。URI 里的 machineId 与部署 machineId 不一致时，`open` 在 `resetOwnership`、`openDatabase` 和 runtime 构造之前以 `invalid_identity` 拒绝，零副作用。`ChatOpenResult.workspace` 回显 canonical URI，不泄露内部 ref。

## 4. Chat actor

chat actor 是会话的执行实体，key 固定为 `[chatId]`：`chatId` 由客户端生成并保持稳定，同时是路由键与所有权键，actor 不解释其他业务身份。ACP sessionId 只存在于 `ChatService` 内部，可通过 `change_session` 切换，因此不能承担 key 的稳定性要求。

### 生命周期映射

```text
action open(target) ──► resolveWorkspaceURI ──► owner.open(key, vars)
                                                   │  旧 lease 关闭未确认 → 直接失败
                                                   ▼
                                   resetOwnership ──► openDatabase（立即登记 owner）
                                                   ──► createChatSession（登记 chat，order 0）
                                                   ──► ChatActivationActivity.start()
onSleep / onDestroy ──► owner.retire(vars) ──► lease.close()（chat → database，失败即抛）
action close ──► lease.close()；任一失败保留 owner 记录与已确认进度，返回 close_incomplete
host shutdown ──► beginShutdown() ──► closeAll（最多 2 轮，只重试可重试 incomplete）
```

- 只有 `open` 建立激活，除 `close` 外的 action 在未 open 时一律 `closed`（`reason: 'not_open'`）；`close` 在未 open 时是 no-op。
- `open` 幂等：workspace、agent、storage、session、sessionPath 与 lifecycle 指纹一致直接返回 `reused: true`，不重建；任一配置不一致返回 `binding_conflict`，不会悄悄忽略变化。
- 同一 activation 的 `open`/`close`/teardown 按 vars 串行；owner 再按 key 串行同 key 的不同 activation。并发同目标 `open` 只构造一次激活；不同目标明确 `binding_conflict`。
- 显式 `close` 在 chat 失败时保留整个记录（database 尚未关闭），在 database 失败时保留已关闭 chat 的进度；下次 `close` 只重试未确认资源，成功后才清空 vars。失败的激活不会被 `open` 报告为 `reused`，`open` 会先重试关闭。
- sleep/destroy 与显式 `close` 共用同一 owner 记录，不再「chat 失败也先关 database」：SDK 未确认关闭时 database 可能仍被使用。失败保留记录与进度；已知 SDK 失败映射为 `close_incomplete`，database 等未知错误原样抛出。
- RivetKit 3.1.1 的 onSleep/onDestroy 包装在 `finally` 中清理 native runtime state 并 dispose activation；本包不把 hook 抛错当作 vars 跨激活保留。真实资源在 owner：同 key 的新 activation 必须先确认旧 lease 关闭，未确认会阻止 `resetOwnership`/`openDatabase`；进程停机用 `closeAll` 枚举重试。只有明确 incomplete 才重试，未知错误立即暴露，清空 vars 不代表 owner 已清空。`beginShutdown` 与新 `open` 共享同一有序准入：等待旧 close 后、注册/`create` 前重新校验停机状态，停机线性化之后不再 reset ownership、打开 database 或构造 SDK。
- `onLifecycle` 只在 wake 成功之后与 sleep / destroy 收尾之后触发；抛错转为 `lifecycle_observer_failed`，不改变生命周期。

### 所有权与数据库

SDK 的 ownership 是 `get / set / remove`，没有原子 CAS；Rivet 对同一 key 只保留一个激活，因此 `open` 前发现的记录一定属于上一个激活：

1. `resetOwnership` 读一次记录：缺失返回 `absent`；存在则 `clearSessionOwnership` 并返回 `cleared`；
2. 记录格式非法时 fail closed（SDK 抛错），不按“无记录”继续，也不覆盖；
3. 清理失败中止 `open` 并记 `ownership_reset_failed`，此时不打开数据库、不触碰 KV 之外的任何资源。

`openDatabase(target.storage)` 打开一次调用私有的会话目录；一取得就登记到 owner 记录，因此 `createChatSession` 失败时数据库仍由宿主负责关闭，关闭再次失败也不会丢 owner。actor 自身不持有数据库表结构、不建表、不迁移。

### Action 面

| Action | 输入 | 输出 |
| --- | --- | --- |
| `open` | `ChatOpenTarget`（chatId、canonical workspace URI、agent、storage、可选 session/sessionPath/lifecycle） | `ChatOpenResult`（chatId、workspace URI、binding、reused） |
| `execute` | SDK `ChatUserAction` 原样 JSON + 可选 `ChatExecuteTarget` | `ChatExecuteResult`（receipt / 控制结果） |
| `readPayload` | `ToolPayloadReadRef` + 可选 `ReadPayloadOptions` | `ToolPayloadResponse` |
| `observe` | — | `ChatObservation \| null` |
| `activity` | — | `RuntimeActivity` |
| `facts` | — | `ChatFactsResult`（无游标，当前代次全量） |
| `close` | — | `{ closed: true }` |

actor 不复制 SDK 状态机：`execute` 在 SDK 边界内解析 action；`send` 只等到 `accepted`，整轮结果通过事实流通知。带 `ChatExecuteTarget` 的调用先比对当前绑定，代次或 session 不一致时以 `binding_conflict` 拒绝，不执行副作用。

### 事实与事件

`ChatFactEnvelope` 通过 `chatFact` 事件广播给实时客户端；`onBinding` 重放只用于重建活动账本，不重复广播。`facts` 不接收游标：一次订阅拿到的就是当前绑定代次的全量事实，客户端自己按 `bindingRevision` / `sequence` 去重。日志溢出时返回 `overflow: true`，客户端必须重新读会话。

不持久化任何事实位置：没有 actor state、没有游标表、没有绑定表。

### 运行时与 transport

`ChatConfig.runtime` 有两个分支：`{ kind: "wasm" }` 由 SDK 自带产物托管 ACP；`{ kind: "transport", factory }` 由宿主提供 `Transport`，每次激活（`create_new_session` 或 `change_session`）调用一次工厂。适配层不复制 transport 协议，只在 `ChatService` 边界注入 `ChatConfig`；请求/通知清单与关闭语义以 `docs/custom-runtime.md` 为准。

本包不内置 runtime：真实运行使用 SDK 的 `{ kind: "wasm" }`，产物加载、Store 连接与 ACP 托管都在 SDK 内；测试与示例只使用宿主提供的 `Transport`。两种分支共用同一个 `ChatService` 边界，包内不复制 transport 协议。

`chat.close()` 需要 transport 确认 `session/close`：缺失、拒绝或超出 `closeDrainTimeoutMs` 时 SDK 抛 `ChatCloseIncompleteError`，actor 映射为 `close_incomplete` 并保留 SDK 侧 ownership、未确认激活与 actor 句柄；只有所有资源都确认关闭后才清空 vars，重试、下次 `open` 或宿主 `closeAll` 会继续未完成的关闭。从未建立 Session 的激活不会发送 `session/close`。

### 错误映射

| 来源 | 对外 |
| --- | --- |
| `ActorError` / `PeriError` | `UserError(message, { code, metadata })`，code 保持领域 code |
| `ChatActionError` | `UserError`，code 为 SDK 错误 code，metadata 带 `requestId` / `disposition` |
| `ChatCloseIncompleteError` | `UserError`，code 为 `close_incomplete`，metadata 带失败链 |
| `ZodError` | `UserError`，code 为 `invalid_input` |
| 其他异常 | 原样抛出，由 Rivet 收敛为 `internal_error`，不泄露内部细节 |

### 组合根

本包不提供 registry 工厂：`setup({ use: { chat, ... } })` 由部署方书写，actor options（`actionTimeout` 等）声明在 `createPeriChatActor({ options })` 上，避免出现“registry 接受但 Rivet 不解析”的伪配置。`createPeriChatActor` 与 `createSandboxMcpActor` 都要求显式注入 `HostResourceOwner`；组合根负责创建、强持有，并在停机时按 `beginShutdown → closeAll` 收尾。包内没有自动创建或模块级兜底 owner，避免资源在无主状态下被静默丢弃。`registry.shutdown()` 只做引擎/客户端收尾，可能吞掉部分错误，不能作为资源关闭确认；资源结果与 registry 结果分别处理，上一项失败不得跳过下一项。

## 5. 活动账本

`ActivityLedger` 是一个 activation 内活动计数的唯一权威：

```ts
type RuntimeActivity = {
  revision: number;
  complete: boolean;
  rootRunning: boolean;
  activeInternalTasks: number;
  queuedInputs: number;
  pendingDeliveries: number;
  pendingInteractions: number;
  localOperations: number;
  closing: boolean;
};
```

### 不变量

1. 每次计数变化发布一个新的冻结快照，`revision` 严格递增。
2. `beginQueuedInput` 在 `closing` 后抛错；其余 lease 仍允许，因为已接受的工作必须能收尾。
3. root 与 queued 的交接顺序固定：先取得 root lease，再释放 queued lease，快照不会在交接点归零。
4. lease 幂等；重复 `release` 是 no-op；无匹配获取的释放抛 `activity_invariant`。
5. `complete` 是显式字段。适配层暂时无法观测全部计数时必须置 `false`；消费方按“仍需驻留”处理。
6. 观察者异常不污染计数：有 sink 时交给 sink，否则在 microtask 中重新抛出。

### 判定函数

| 函数 | 条件 | 用途 |
| --- | --- | --- |
| `isQuiescent` | root 停止且五个计数全为 0 | 活动层面是否已静默 |
| `requiresActivationContext` | 快照不完整、closing 或未静默 | keep-awake 是否必须持有 |
| `canReleaseActivation` | 快照完整、未 closing 且已静默 | 宿主是否可以释放实例 |

`closing` 故意让 activation 继续驻留：关闭准入后，已接受的工作仍要完成，实例不能在 drain 期间被回收。

`waitForQuiescence()` 返回下一次静默快照。它没有超时：超时会让适配器在工作仍持有 lease 时释放实例。需要上界的调用方自行 `Promise.race`。

## 6. Chat 活动投影

SDK 的 `ChatActivityTracker` 负责把观测与事实翻译成绝对快照；本包的 `ChatActivityLedger` 只把快照映射成租约增量，不做判定。

### 观测是相位受限的

`ChatObservation.phaseLimited` 固定为 `true`：公开相位不是内部状态机的完整投影。因此：

- 只有观测时，`rootRunning`、`queuedInputs`、`pendingInteractions` 更新，但 `complete` 保持 `false`，activation 永不因“看起来空闲”而睡眠；
- 注入完整的 SDK `ChatActivitySource` 后，`activeInternalTasks` 与 `pendingDeliveries` 才有权威来源，`complete` 才能置 `true`。

这是实验里失败过的场景：主任务结束后仍在运行的 Subagent 被睡眠回收。

### 事实

| 事实 | 影响 |
| --- | --- |
| `task-snapshot` | 统计非终态任务；`unobserved:*` 视为非终态 |
| `run-state` | 更新 `rootRunning` |
| `turn-complete` / `turn-cancel` | `rootRunning = false` |
| `closed` | 标记 closing，释放所有活动 lease |

计数来自绝对值而非增量，重复或乱序事实不会累积 lease。非法计数被钳到 `[0, +∞)` 整数，避免一个坏快照永久占用实例。

### 更新顺序

`#sync` 先在需要时把 `complete` 置 `false`，再逐项设置计数，最后才置 `complete = true`。监听者不会在更新中途看到“完整且可释放”的快照。

`beginLocalOperation` 为每个正在执行的 action / payload 读取持有一个 `localOperations` lease；`beginClose` 先冻结准入，句柄关闭后才允许账本静默。

## 7. Keep-awake 协调

`KeepAwakeCoordinator` 订阅活动快照，把 `requiresActivationContext` 映射为一次 `KeepAwakePort.acquire()`：

- 一个连续活跃 epoch 只持有一个 hold，不按计数增量重复获取；
- 订阅回调同步执行，因此 `submit` / `execute` 返回前 hold 已获取；
- epoch 结束（快照完整、未 closing、已静默）时释放；
- 获取失败通过 `onError` 上报，hold 保持为空，下一次活动变化重试；
- 释放失败保留 hold，下一次变化重试，避免丢失驻留；
- `closing` 期间继续持有，`stop()` 在 drain 完成后统一释放。

`rivetKeepAwakePort` 把 `KeepAwakePort` 桥接到 Rivet 的 `c.keepAwake(promise)`：一次 acquire 对应一个未 settle 的 promise，release 时 resolve。`waitUntil` 不在正常保活路径上，只用于有界收尾。

## 8. 资源读取

resource 不是 actor：只读快照没有需要保活的执行状态，wake/sleep 只会增加延迟。本包不提供 resource actor 定义，也不把 resource 注册进 `setup()`。

```text
HTTP ──► createResourceMcpHttpHandler ──► resolveScope(request)
              └─► createResourceMcpServer ──► resource_get ──► ResourceQueries.get(scope, resourceId)
```

`ResourceQueries` 是宿主实现的只读端口：`get(scope, resourceId)` 返回 `ResourceSnapshot | null`。`null` 只表示该 scope 下不存在，存储故障必须 reject，不能折叠成 miss。

读模型固定四种 `ResourceKind`：`agent_config`、`provider_list`、`mcp_plugin_config`、`model_settings`。`revision` 不透明，只用于判断是否变化；`value` 已由服务端掩码，密钥不回传。

每次读取都实时查询宿主：本包不缓存、不写资源、不保存数据库句柄，也没有资源的创建、关闭或作业生命周期。资源的写入与失效属于宿主和 Store。

## 9. MCP 边界

### Stateless per request

`createResourceMcpServer` 每次请求新建，所有读取走同一个 `ResourceQueries`。MCP session 结束不改变资源状态，重连也不需要新绑定；handler 不保存任何会话状态。

### Scope 只来自认证边界

HTTP handler 从 `resolveScope(request)` 得到 scope，放入 `AuthInfo.extra[SCOPE_AUTH_EXTRA_KEY]`，server 工厂再读回。工具参数里没有 scope，客户端无法伪造。缺少 scope 抛 `mcp_scope_required`；HTTP 边界未认证直接返回 401 JSON。

### 工具与注解

| 工具 | 注解 |
| --- | --- |
| `resource_get` | `readOnlyHint: true` |

工具参数只有 `resourceId`；所有工具声明 `openWorldHint: false`。

### 错误映射

`ActorError` 转为 `isError: true` 的 `CallToolResult`，文本内容为 `{ "error": error.toJSON() }`；`resource_get` 找不到目标时是 `resource_not_found`。非领域异常继续向上抛，交给传输层和 `onError`，避免把实现缺陷伪装成业务错误。

## 10. 状态与存储边界

本包不持久化执行状态：没有 actor state、没有绑定表、没有事实游标，也没有账本或迁移脚本。

| 数据 | 持有者 | 生命周期 |
| --- | --- | --- |
| `ChatService`、`ChatDatabase`、活动投影 | chat actor 的 `vars`（一次 `open`） | `close`、`onSleep`、`onDestroy` |
| 会话目录（如 `TursoStorage`） | 宿主的 `openDatabase` 端口 | 每次激活打开一次；actor 不建表、不迁移 |
| ownership 记录 | 宿主的 `ChatOwnership` | 只在 `open` 前清理陈旧记录 |
| resource 快照 | 宿主的 `ResourceQueries` | 每次读取实时查询，本包不缓存 |

sleep 只关闭激活，不删除会话数据。下一次 `open` 重新装配，不恢复旧执行，也不自动回放历史：`open` 返回的 `binding` 仍是 `null`。是否可读历史取决于 runtime 与 Store；只有显式 `change_session` / `session/load` 才会请求历史，wasm runtime 从 Store 读取。数据库 schema、连接与放置都归宿主和 SDK。

## 11. 证据记录

`EvidenceRecorder` 只收集，不落盘：

- `check(id, title, execute)`：执行成功记录 `passed` 与可选 JSON details；抛错记录 `failed` 与错误信息；自身不抛业务错误；
- `skip(id, title, reason)`：记录 `skipped`；
- 重复 id 抛 `invalid_input`，避免覆盖证据；
- `report()` 输出 `runId`、时间范围、环境、检查列表与 `passed/failed/skipped` 汇总，形状对齐实验计划的 `results.json`。

时间、环境、`runId` 都可注入，保证测试确定性。

## 12. 验证边界与非目标

### 本包已覆盖

1. Chat actor 的 action/event/wake/sleep 接线与 `UserError` 映射（结构级）；
2. 活动账本、keep-awake 在租约交接、关闭 drain、观察失败下的行为；
3. resource 只读查询的 scope 鉴权、掩码快照与 MCP 工具错误映射；
4. sandbox actor 的引用计数、`closeAll` 只作用于自身 resource、sleep/destroy 的 best-effort 清理，以及 Sandbox MCP 的 URI 授权/凭据/`/mcp` 转发/关闭前收束；
5. `open` 幂等、actor key 不匹配与陈旧 ownership 清理；
6. MCP 作用域来自认证边界，会话与资源解耦；
7. 宿主 owner 在 vars 被替换后仍持有未确认资源、同 key 重开阻塞/成功、迟到旧 close 不删新记录、`closeAll` 的 timeout/并发/有界重试与未知错误暴露；sandbox cancel reject、late response 与丢 vars 新 activation 不能绕过旧 gate。

### 真实接入必须另验

| 接入点 | 待验事项 |
| --- | --- |
| 真实 Peri runtime | 内部 Subagent、工具调用、交付与交互都能上报到活动账本 |
| Rivet engine | sleep/wake、崩溃接管、`keepAwake` 与节点回收的时序 |
| Store 读取 | `ResourceQueries` 指向的 Store 在多节点、超时与凭据轮换下的行为 |
| Sandbox 运行端口 | 宿主进程能独立回收实例；sleep 丢失 holders 后 `closeAll` 能清理孤儿；真实 `peri-sandbox` 的环境与 MCP 在 actor `/mcp` 后可用 |
| 部署 | 多节点、版本共存、drain 策略与忙碌节点保护 |

### 已知非目标

- 不恢复旧执行状态，只保留持久化数据；
- 不保证外部副作用 exactly-once；
- 不实现 Actor 放置或跨节点协调；
- `keepAwake` 不防 SIGKILL、机器丢失或编排器强制终止；
- `closeAll` 只做进程内、有界重试；SIGKILL/进程崩溃后未登记的本进程资源不会自动清理，cancel 永久拒绝的 sandbox 请求保持 incomplete；
- 不提供 resource 写路径或持久化账本；这些属于宿主与 Store。
