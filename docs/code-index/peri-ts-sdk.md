# Peri TS SDK 代码索引

状态：现行实现。权威结构见 [Peri SDK Chat 架构](../design/peri-sdk-chat.md)，使用说明见
[SDK README](../../npm-packages/@peri-sdk/README.md)。本文只给入口与职责，不复制动态 inventory 或行号。

## 发布入口

| 入口 | 文件 | 内容 |
| --- | --- | --- |
| 主入口 | `npm-packages/@peri-sdk/src/entries/index.ts` → `src/entries/chat.ts` | `ChatManager`、`ChatService`、`ChatUserAction`、`ChatView`、`ChatReplica`、`ChatViewRegistry`、`TursoStorage`、公开类型/错误（含 `ChatManagerContract`）、ownership 公开面（`KvStorage` 类型、`clearSessionOwnership` 函数）、Store/KV/transport 类型 |
| `/portable` | `src/entries/portable.ts` | 主入口的 Chat 公共面 + 嵌入方驱动的执行准入核心；准入摘要仍依赖 `node:crypto` |
| `/view` | `src/entries/view.ts` | 浏览器安全展示：`ChatView`、`ChatReplica`、`readSessionView`、载荷读取、同步协议；不引入 Node 运行时依赖，也不导出 `ChatViewRegistry` |
| `/wasm` | `src/entries/wasm.ts` | Emscripten ACP 运行后端、loader、`WasmAcpTransport` |
| `/wasm-node` | `src/entries/wasm-node.ts` | `/wasm` + Node 网络/DNS 端口 |

旧 `Agent`、`ManagedAgents`、`SessionDocs`、`SessionViewStore`、`AtomicManagedAgentKv`、
`agent-claim-conflict-error`、`Sandbox` 已删除，不在任何发布入口导出；`tests/public-view.test.ts`
只对旧符号做负向断言。stdlib/Workspace 进程管理、本地数据库、JSONL dispatcher 与执行恢复账本
也不在 SDK 内。

## 依赖方向

1. `contracts` 可依赖 `state`/`transport` 的类型，不得依赖 `chat`/`agent`/`view`/`wasm`。
2. `payload`、`state`、`sync`、`storage`、`config`、`execution`、`util`、`transport` 不得依赖 `chat`/`agent`/`view`/`wasm`。
3. `wasm` 可依赖 `contracts`/`payload`/`transport`/`util`，不得依赖 `chat`/`agent`/`view`。
4. `agent` 可依赖 `contracts`/`payload`/`state`/`transport`/`wasm`/`util`，不得依赖 `chat`/`view`。
5. `chat` 可依赖 `contracts`/`payload`/`state`/`transport`/`agent`/`wasm`/`util`，不得依赖 `view`。
6. `view` 可依赖 `contracts`/`payload`/`state`/`sync`/`transport`/`util`，不得依赖 `chat`/`agent`。
7. `entries` 可依赖所有层；任何模块不得依赖 `entries`。

## 控制面

| 职责 | 入口 | 行为 |
| --- | --- | --- |
| Manager | `src/chat/manager.ts` | 同步 `createChat`；句柄目录、`findChatBySession`、目录读取、`closeChat`、`shutdown`；不接收普通执行动作 |
| Manager 契约 | `src/chat/types.ts`、`src/entries/chat.ts` | `ChatManager`/`ChatService` 接口；对外类型名 `ChatManagerContract`，具体类 `src/chat/manager.ts` 实现它，调用方可注入自有实现 |
| Service | `src/chat/service.ts`、`src/chat/binding.ts`、`src/chat/close-chat.ts` | 绑定状态机、八类动作分派、回执关联、事实日志、关闭预算与失败保留；载荷读取按发起时的绑定代次校验，换绑后的晚到结果/错误丢弃为 `unavailable` |
| 动作 | `src/chat/action.ts`、`src/util/json-value.ts` | 八类纯数据 Action、`requestId`、JSON 深拷贝/冻结、`parseChatUserAction`；不依赖 `instanceof` |
| 类型/错误 | `src/chat/types.ts`、`src/agent/port.ts`、`src/chat/errors.ts` | `ChatRuntime`、`ChatSessionInit`、`ChatLifecycleOptions`、`PeriInstancePort`、错误 `disposition` 与 `ChatCloseIncompleteError` |
| 事实 | `src/contracts/chat.ts`、`src/chat/fact-hub.ts` | 按绑定代次编号的事实封套；晚挂 View 重放 |
| 回执 | `src/chat/send-receipt.ts` | `inputId` 稳定；`accepted` 是入队，投递等待是另一件事 |
| Session ownership（内部） | `src/chat/ownership.ts`、`src/chat/types.ts` | `ChatManager` 接收 3 方法 `KvStorage` 并在内部构造 `KvSessionOwnership`（owner 为实例随机 UUID）；key 为 `ownership:<base64url(sessionId)>`，value 为 `{ sessionId, owner }`；acquire 是读后写、非原子，KV 记录是绑定标记/防御路由 bug，不是互斥原语；无 TTL/续租/CAS，路由层负责串行化与 `clearSessionOwnership`；`LocalSessionIndex` 负责同 Manager 内句柄互斥。`SessionOwnership`/`KvSessionOwnership`/`SessionOwnershipConflictError`/key 函数仅模块内使用；入口只导出 `KvStorage` 与 `clearSessionOwnership` |
| 工厂 | `src/chat/instance-factory.ts`、`src/chat/service-context.ts` | 把 Manager 上下文转换为一次 `PeriInstance`；生产默认装配，测试可注入 |
| 会话目录 | `src/storage/turso-storage.ts` | `TursoStorage` 直接实现 `ChatDatabase`；`storageConfig` getter 返回只含 `url`/`authToken` 的纯 JSON（token 为 undefined 时省略该键，不含 cwd），供 `agentConfig.storage`；显式 `path` 查单个 cwd，省略时串行合并 `legacyCwd`/`currentCwd`；同 id 以当前 cwd 为准，按 `updatedAt` 倒序；缺表为空目录 |
| 生命周期预算 | `src/util/deadline.ts`、`src/chat/service-lifecycle.ts` | 关闭/启动/shutdown 的有界等待；超时是 Incomplete，不是停止证明 |

## 运行侧

| 职责 | 入口 | 行为 |
| --- | --- | --- |
| PeriInstance | `src/agent/peri-instance.ts` | 一次激活的 Session/运行后端/交互/队列/历史/载荷控制；剥离 Yjs，只输出事实 |
| Session | `src/agent/session.ts`、`src/agent/session-close.ts` | 纯 ACP 客户端：`session/new|load`、input 队列命令、`session/cancel`、有界 `session/close`、历史分页 |
| 交互 | `src/agent/{interaction-broker,ask-user-question}.ts`、`src/contracts/interaction.ts` | 权限/elicitation/AskUserQuestion 的 ACP 反向请求与回答；turn 结束用安全默认收敛 |
| MCP over ACP | `src/agent/acp-mcp-bridge.ts`、`src/contracts/acp.ts` | `acp` 声明走反向 `mcp/message`；直接实例单次借用，跨激活必须用工厂；owned 实例由激活关闭 |
| 载荷 | `src/state/tool-payloads.ts`、`src/chat/payload-access.ts`、`src/payload/reader.ts` | 内容寻址引用、inline/单项/会话预算、`readPayload` 字节与 JSON 校验；错误码 `unauthorized`/`unavailable`/`too_large`/`integrity_error`/`aborted`/`transport_error` |
| 运行后端 | `src/wasm/{runtime,runtime-pool,loader,ports,scheduler,transport}.ts`、`src/wasm/budget.ts` | WASM 启动/租约/关闭证据；scheduler 与清理/启动超时由运行后端自持；`moduleFactory` 时无需 `moduleUrl`；HTTP/DNS 端口仅在深层运行后端入口可注入 |

## 展示与同步

| 职责 | 入口 | 行为 |
| --- | --- | --- |
| View | `src/view/chat-view.ts` | 订阅 `ChatFactSource`；每个绑定代次一对 Y.Doc 与 `SessionDocSync`；换代成对替换；`dispose` 释放连接与文档 |
| View 共享 | `src/view/view-registry.ts` | `ChatViewRegistry`：按 Chat id 复用同一个 `ChatView`；提供 attach/get/dispose/disposeAll；从主入口与 `/portable` 导出，不从 `/view` 导出 |
| 同步连接 | `src/view/chat-sync.ts`、`src/view/channel.ts` | `ChatYjsChannel` 只做逻辑帧；HELLO/SNAPSHOT/UPDATE/ACK、交付信用、有界背压与终止 |
| 副本 | `src/view/chat-replica.ts` | 浏览器只读：HELLO resume、gap repair、generation/schema 校验、`read()`/`subscribe()` |
| 文档模型 | `src/view/chat-document.ts`、`src/state/{doc-model,protocol,session-projection,task-projection,interaction-projection,turn-machine}.ts` | ACP 通知 → Entry/Block/Tool/Turn/Task/Plan/Interaction；历史幂等合并、终态收敛、closed 展示事实 |
| 只读视图 | `src/view/session-view.ts`、`src/view/read-cache.ts` | `readSessionView` 纯读取完整 `SessionView`；不再导出按身份选择器，未知块/孤立工具引用显式诊断；无 `getSnapshot`/`subscribe` 门面 |
| 同步协议 | `src/sync/{wire,peer,delivery,session-doc-sync,session-doc-replica}.ts` | 二进制帧、协议版本、状态向量、离线补齐与慢客户端上界 |

## 验证入口

- `bun run typecheck`：`tsc --noEmit` + demo tsconfig；测试目录不在 include 内。
- `bun test tests test`：单元/行为测试；`test/` 覆盖执行准入、Session 关闭/启动与 SessionOwnership。
- `bun run test`：在 `MISE_ENV=wasm mise exec --` 下先 build（含 peri-wasm）再全量测试。
- `bun run test:wasm`：先 build，再跑真实 WASM ACP 集成（真实打包 WASM + 真实 Turso listener + fixture 模型）。
- `bun run smoke:workers`：本地 workerd 探针；Hosted Workers 未部署。
- 关键回归：`tests/chat-{action,manager,binding,actions,close-bounded,view,replica,document,instance-factory}.test.ts`、`tests/view-registry.test.ts`、`tests/peri-instance.test.ts`、`tests/history-pagination.test.ts`、`tests/acp-mcp-lifecycle.test.ts`、`tests/turso-storage.test.ts`、`tests/wasm-acp-integration.test.ts`。
- 公共出口负向断言：`tests/public-view.test.ts`，同时确认 `ChatViewRegistry` 只在主入口、不在 `/view`。

## 消费者

- `npm-packages/peri-cf` 通过主入口与 `/view` 消费 Manager/Action/View/Replica；平台装配、HTTP/WS 与鉴权留在应用。索引见 [peri-cf](peri-cf.md)。
- Workers smoke 入口在 `examples/workers/`；只证明当前 Host 方法表与本地 `workerd` 路径，不代表 hosted Workers 验收。
