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
rivet/echo-runtime ──► @peri-code/sdk（Transport；无 ACP、进程或 LLM 的参考实现）
rivet/sandbox-actor ──► contracts/errors + contracts/identity + util/deferred（不依赖 SDK）
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
| `ChatService`、ChatDatabase、活动投影 | 一次 `open` | `c.vars` + 按句柄索引的 `WeakMap` | `close` action、`onSleep`、`onDestroy` |
| 共享 KV（ownership 记录） | 跨实例 | 宿主提供的 `ChatOwnership` | actor 只在 `open` 前清一条陈旧记录 |
| sandbox 运行实例 | 进程，按 `sandboxId` 分片 | 宿主提供的 `SandboxRuntimePort` | refs 归零、`closeAll`、sleep/destroy；进程级回收由宿主在自己的 runtime 上完成 |

本包不提供隐藏单例：`openDatabase` 与 `ownership` 都由宿主显式持有，actor 只在一次激活内打开并关闭它们。

### Sandbox actor

`rivet/sandbox-actor.ts` 是单个 `sandboxId` 引用计数的唯一权威：`vars.holders` 是唯一状态，
`refs` 恒等于其长度，同一 holder 重复 acquire 不重复计数。端口
`SandboxRuntimePort { start, close, stateOf }` 只做 spawn/close/只读状态，不持有计数；
`runtime` 可选，缺省时 actor 仍可注册，`acquire` 返回 `sandbox_unavailable`。

`closeAll` 只关闭本 `sandboxId`（幂等、不启动、不碰其它 id）；进程级回收由宿主在自己的
运行端口上完成，端口契约不定义该动作。失败统一返回 `PeriErrorPayload`（`sandbox_*` / `invalid_identity` /
`internal_error`），端口错误原文只进 `onDiagnostic`，不跨 actor。

引用只在 activation 内有效：`refs > 0` 时持有一个 keep-awake hold，归零释放；sleep/destroy
上报 `holders_lost_on_teardown` 后 best-effort 关闭本实例。`status.orphaned` 只读地暴露
「refs=0 但实例仍在」，监控读取不产生副作用，清理留给 `closeAll`。

## 3. 身份与边界

| 身份 | 生命周期 | 语义 |
| --- | --- | --- |
| actor id / key | Rivet actor | chat 为 `[chatId]`，sandbox 为 `[sbx, sandboxId]`；本包不解释其它业务 key |
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

## 4. Chat actor

chat actor 是会话的执行实体，key 固定为 `[chatId]`：`chatId` 由客户端生成并保持稳定，同时是路由键与所有权键，actor 不解释其他业务身份。ACP sessionId 只存在于 `ChatService` 内部，可通过 `change_session` 切换，因此不能承担 key 的稳定性要求。

### 生命周期映射

```text
action open(target) ──► resetOwnership ──► openDatabase ──► createChatSession ──► ChatActivation
                                                                                     │
                                                ChatActivationActivity.start() ◄─────┘
onSleep / onDestroy ──► chat.close() ──► database.close()（各自 try，失败记诊断）
action close ──► 同一路径；chat 失败向调用方返回 close_incomplete
```

- 只有 `open` 建立激活，除 `close` 外的 action 在未 open 时一律 `closed`（`reason: 'not_open'`）；`close` 在未 open 时是 no-op。
- `open` 幂等：chat/workspace/agent 指纹一致直接返回 `reused: true`，不重建；不一致返回 `binding_conflict`。
- sleep/destroy 必须先把 vars 清空再等待关闭：失败的 `close` 不能留下“看起来还开着”的 actor，下一次 `open` 重新装配。
- chat 关闭失败不跳过 `database.close()`；两者失败都记 `close_failed` 诊断，sleep 不因释放失败而保留激活。
- `onLifecycle` 只在 wake 成功之后与 sleep / destroy 收尾之后触发；抛错转为 `lifecycle_observer_failed`，不改变生命周期。

### 所有权与数据库

SDK 的 ownership 是 `get / set / remove`，没有原子 CAS；Rivet 对同一 key 只保留一个激活，因此 `open` 前发现的记录一定属于上一个激活：

1. `resetOwnership` 读一次记录：缺失返回 `absent`；存在则 `clearSessionOwnership` 并返回 `cleared`；
2. 记录格式非法时 fail closed（SDK 抛错），不按“无记录”继续，也不覆盖；
3. 清理失败中止 `open` 并记 `ownership_reset_failed`，此时不打开数据库、不触碰 KV 之外的任何资源。

`openDatabase(target.storage)` 打开一次调用私有的会话目录；`createChatSession` 失败时必须先 `database.close()` 再抛出，不留半开句柄。actor 自身不持有任何数据库、不建表、不迁移。

### Action 面

| Action | 输入 | 输出 |
| --- | --- | --- |
| `open` | `ChatOpenTarget`（chatId、workspace、agent、storage、可选 session/sessionPath/lifecycle） | `ChatOpenResult`（chatId、workspace、binding、reused） |
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

包内的 `createEchoChatRuntime()` 是同一 transport 契约的最小实现：不启动进程、不接 LLM，把输入回显为 assistant 文本，并实现 `session/close`。它只用于打通 actor 闭环与测试，不写 transcript，也不跨激活保留历史。

`chat.close()` 需要 transport 确认 `session/close`：缺失、拒绝或超出 `closeDrainTimeoutMs` 时 SDK 抛 `ChatCloseIncompleteError`，actor 映射为 `close_incomplete` 并保留 SDK 侧 ownership 与未确认激活；actor 自身句柄已清空，继续使用需要重新 `open`。从未建立 Session 的激活不会发送 `session/close`。

### 错误映射

| 来源 | 对外 |
| --- | --- |
| `ActorError` / `PeriError` | `UserError(message, { code, metadata })`，code 保持领域 code |
| `ChatActionError` | `UserError`，code 为 SDK 错误 code，metadata 带 `requestId` / `disposition` |
| `ChatCloseIncompleteError` | `UserError`，code 为 `close_incomplete`，metadata 带失败链 |
| `ZodError` | `UserError`，code 为 `invalid_input` |
| 其他异常 | 原样抛出，由 Rivet 收敛为 `internal_error`，不泄露内部细节 |

### 组合根

本包不提供 registry 工厂：`setup({ use: { chat, ... } })` 由部署方书写，actor options（`actionTimeout` 等）声明在 `createPeriChatActor({ options })` 上，避免出现“registry 接受但 Rivet 不解析”的伪配置。

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

sleep 只关闭激活，不删除会话数据。下一次 `open` 重新装配，不恢复旧执行，也不自动回放历史：`open` 返回的 `binding` 仍是 `null`。是否可读历史取决于 runtime 与 Store；只有显式 `change_session` / `session/load` 才会请求历史，echo 不回放。数据库 schema、连接与放置都归宿主和 SDK。

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
4. sandbox actor 的引用计数、`closeAll` 只作用于自身 `sandboxId`，以及 sleep/destroy 的 best-effort 清理；
5. `open` 幂等、actor key 不匹配与陈旧 ownership 清理；
6. MCP 作用域来自认证边界，会话与资源解耦。

### 真实接入必须另验

| 接入点 | 待验事项 |
| --- | --- |
| 真实 Peri runtime | 内部 Subagent、工具调用、交付与交互都能上报到活动账本 |
| Rivet engine | sleep/wake、崩溃接管、`keepAwake` 与节点回收的时序 |
| Store 读取 | `ResourceQueries` 指向的 Store 在多节点、超时与凭据轮换下的行为 |
| Sandbox 运行端口 | 宿主进程能独立回收实例；sleep 丢失 holders 后 `closeAll` 能清理孤儿 |
| 部署 | 多节点、版本共存、drain 策略与忙碌节点保护 |

### 已知非目标

- 不恢复旧执行状态，只保留持久化数据；
- 不保证外部副作用 exactly-once；
- 不实现 Actor 放置或跨节点协调；
- `keepAwake` 不防 SIGKILL、机器丢失或编排器强制终止；
- 不提供 resource 写路径或持久化账本；这些属于宿主与 Store。
