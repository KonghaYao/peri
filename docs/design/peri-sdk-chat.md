# Peri SDK Chat 架构

> 状态：**现行设计**（2026-10-26）。实现与验收证据见
> [Peri SDK Chat 重构 issue](../../spec/issues/2026-10-24-peri-sdk-chat-refactor.md)；
> 代码入口以 [TS SDK 索引](../code-index/peri-ts-sdk.md) 为准。
> 本文只描述稳定结构与边界，不记录实施批次、测试计数或平台验收结果。

## 1. 目标与边界

SDK 只做 Chat 控制面、ACP 运行装配与只读展示投影；模型执行、调度、会话持久化与工具运行仍归 Peri。一个 Chat 句柄对应一次可替换的 Session 绑定，不把 Chat 句柄 ID 当作持久 Session 身份。

不新增 SDK 调度器、持久执行恢复、平行会话表或第二套 ACP 协议。ACP 连接只有宿主注入的一条装配路径；SDK 不启动 Workspace 进程，也不提供本地数据库后端。唯一例外是显式可选的 server-side 子入口 `/sandbox`：`SandboxManager` 按 `sandboxId` 管理宿主进程内的多个 adapter 实例，具体 sandbox 实现（如 `peri-sandbox`）由宿主通过 `SandboxAdapter` 注入；`SandboxHandle` 只暴露 ready 实例的 `WorkspaceRef`（`{ id, hostPath?, runtimePath }`）与 MCP descriptor（`SandboxMcpServer`）；session 由调用方组装并在 `createChat({ workspace, session })` 中按 chat 传入，不改变 ACP/Chat 契约。

`/sandbox` 的 manager 是宿主级资源注册表，不与 session 一一对应，也不保存 session 映射。SDK 核心只定义 `SandboxAdapter`/`SandboxAdapterInstance` 契约，不 import、不动态 import、也不 peer 依赖任何具体实现。配置分两层：`adapter` 必填，`adapterOptions` 是 manager 默认值；单次 `start` 可用 `adapterOptions` 整体替换默认值，并覆盖 `serverName`/`startupTimeoutMs`/`stopTimeoutMs`。再次 `start({ sandboxId })` 命中已有 handle 时，显式传入的上述字段必须与创建值一致（`adapterOptions` 按 key 排序的稳定 JSON 比较；不可序列化且非同一引用按冲突处理），否则以 `conflict` 拒绝；省略字段继承原值。`signal` 只作用于本次 `start`，不参与冲突，`closeAll` 不继承 manager 的默认 signal。

## 2. 五个核心对象

| 对象 | 职责 | 生命周期 |
| --- | --- | --- |
| `ChatManager` | 部署/Store 边界：`store.storage`/`machineId` 快照、句柄目录、SessionOwnership 协调、整体释放；per-chat 通过 `createChat({ workspace, agent })` 提供边界与运行配置（`cwd` 由 workspace 派生），可覆盖 `lifecycle`/runtime env；不接收普通执行动作 | 进程/宿主内可重建；`shutdown` 后封闭新准入 |
| `ChatService` | 单 Chat 的绑定状态机、动作分派、事实日志、载荷读取；不持有 Manager，不依赖 Yjs | 由 `createChat` 同步登记；绑定在首次动作时建立 |
| `ChatUserAction` | 八类纯数据用户意图，携带稳定 `requestId` | 单次请求；结果按 `disposition` 判断可重试性 |
| `PeriInstance` | 一次激活的 ACP Session/运行后端装配，交互、队列、历史与载荷控制 | 由 `ChatService` 拥有；绑定换代或关闭时释放 |
| `ChatView` / `ChatReplica` | 服务端事实到 Yjs 的投影、逻辑同步连接与浏览器只读副本 | `ChatView` 由 Chat 所有者释放；`ChatReplica` 由浏览器释放 |

依赖方向固定为 `ChatManager → ChatService → PeriInstance → Session → Transport`。`ChatView` 只依赖 `ChatFactSource` 与 `ChatYjsChannel`；`ChatReplica` 只消费同步帧与只读 `SessionView`。

Manager 的 `store`（`storage`/`machineId`）是部署/机器身份边界；`workspace` 与 `agent` 只属于单个 Chat，由 `createChat` 必填提供，Manager 负责从 `workspace.runtimePath` 派生 `cwd`、注入 store 并逐个序列化。per-chat `agent` 不是 JSON 对象或声明 `storage`/`machineId` 抛 `TypeError`；显式 `cwd` 与 `runtimePath` 不一致抛 `workspace_mismatch`。

Workspace 由可移植值对象 `WorkspaceRef` 表达：`runtimePath` 是 Agent/Session 看到的根（必须是规范化绝对路径），`hostPath` 是宿主文件系统路径（远程 sandbox 可省略）；本地 sandbox 两者相同。`sessionPath` 相对 `runtimePath` 解析，绝对路径或 `..` 越界以 `workspace_mismatch` 拒绝。Workspace 只是路径引用，不拥有 sandbox 或 session；MCP descriptor 与 token 仍归 sandbox adapter / `SandboxHandle.mcpServer()`。

## 3. 动作与回执

八类动作：`create_new_session`、`change_session`、`send`、`force_send`、`take_back`、`load_earlier_history`、`cancel`、`answer`。

- `send` 同步返回 `SendReceipt`：`inputId` 与 `accepted` 表示入队被接受，`then`/投递等待表示 Peri 已交付输入；两者不能混为一谈。
- `cancel` 的 `requested` 只表示取消通知已写出，不是停止证明。停止必须由 ACP 关闭/结算证据确认。
- 动作失败携带 `disposition`：`rejected` 明确未生效；`unknown` 可能已写出/执行且不得自动重试；`not_sent` 只由调用端在提交前产生。
- CF HTTP 层只做边界校验、目标绑定核对与错误映射，不重写领域规则。

## 4. 绑定、SessionOwnership 与关闭

- `createChat({ workspace, sessionPath?, agent, session?, lifecycle?, runtime?, payloadBudgets? })` 只同步登记句柄，不启动 ACP、不取得 SessionOwnership、不读取历史。`workspace` 必填：`runtimePath` 必须是绝对路径，`hostPath` 可选；`sessionPath` 相对 `runtimePath`，越界以 `workspace_mismatch` 拒绝。`agent` 必填：per-chat 运行配置，`cwd` 由 SDK 注入（显式值只能等于 `runtimePath`）；`session` 是该 Chat 的公开 Session 建立输入（`ChatSessionOptions`，不含 path）：传入时整体覆盖 `chatConfig.session` 默认值，不做字段合并；每个句柄持有独立嵌套对象（`mcpServers` 容器、http headers、stdio args/env 复制，`acp.server` 保留引用）。
- Manager 是部署/Store 边界：`store.storage`/`machineId` 由 Manager 独占，序列化每个 Chat 时注入内部 `agentConfig` JSON；per-chat 声明这两个键一律抛 `TypeError`（即使值与 Manager 相同）。per-chat `agent` 必须是 JSON 安全对象，结果深拷贝、冻结并逐 Chat 序列化为 WASM 启动配置，因此同一 Manager 可服务不同 workspace。`lifecycle` 按字段覆盖 Manager 默认值，`shutdownChatTimeoutMs` 仍由 Manager 聚合；WASM `runtime.env` 按字段合并，`onDiagnostic` 可 per-chat 覆盖。
- 首次绑定由 `create_new_session` 或 `change_session` 触发；新会话使用 `sessionPath` 派生的 cwd（省略时为 `workspace.runtimePath`），加载历史使用 Store 中的持久 cwd。
- `change_session` 有 workspace 护栏：Manager 维护内存 `sessionId → workspaceId` 映射，已知 Session 属于其他 workspace 时优先拒绝；持久 Session 没有该元数据时回退路径校验，持久 cwd 必须等于或位于当前 Chat 的 `workspace.runtimePath` 之下，否则以稳定错误 `workspace_mismatch` 拒绝。相对 cwd 无法证明在根内，同样拒绝；workspaceId 不写入 ACP 字段（纯 JS 判断，不引入 `node:path`）。
- 同一 Session 同时至多一个执行所有者。调用方注入 3 方法存储端口（`ChatManagerOptions.kv`），`ChatManager` 内部构造 `SessionOwnership`；`LocalSessionIndex` 覆盖 binding/cleanup 候选，同 Manager 内冲突带 `ownerChatId`。
- 归属记录 key 为 `ownership:<base64url(sessionId)>`，value 为 `{ sessionId, owner }`；owner 是 Manager 内部随机 UUID。acquire 是读后写、非原子；KV 记录是绑定标记/防御路由 bug，不是互斥原语，跨 Manager/进程的单实例绑定由路由层串行化，SDK 只做顺序冲突检测。`SessionOwnership` 协调器是内部实现，公开面只有 `KvStorage` 与 `clearSessionOwnership`。
- 无 TTL、续租或心跳：崩溃遗留记录由路由层调用 `clearSessionOwnership(storage, sessionId)` 清理，且只能在旧实例已停止或已被 fence 后调用。
- 旧 key `peri:managed-agent:session:*` 与新 `ownership:*` 互不可见；升级/回滚必须全量停机切换，不做双读兼容。
- 空闲才允许换绑；运行中、存在未处理输入、待决交互或未确认清理时拒绝换绑，不 cancel、不 close 旧绑定。
- 关闭是有界所有权事务：`session/close` 在预算内重放。超时返回 `ChatCloseIncompleteError` 并保留 ownership、索引与资源；不产生 `closed` 事实，可重试。
- `shutdown` 先封闭新准入，再分别结算在途绑定/关闭，最后释放 WASM 运行后端；失败聚合上报，可重试。

`ChatManager` 接收调用方注入的存储端口，并在内部为每个实例构造 `SessionOwnership` 协调器。它解决同一个 Session 被多个 Chat 句柄或服务实例同时绑定的问题：重复执行会造成消息双写与工具重复调用。绑定前向共享存储写入以 Session ID 为唯一 key 的记录（Manager 实例 owner），加载路径读到其他 owner 时冲突并在 CF 映射为 409；不覆盖已有记录。ownership 只在清理确认后释放；关闭超时或失败时保留，避免其他实例接管半关闭会话。unstorage memory driver 仅单进程有效；跨实例需要共享 storage，并由路由层保证同一 Session 的单实例绑定与崩溃清理。加载路径已有控制者时冲突映射 409 `binding_conflict`；加载路径（`change_session`）的 KV 读/写失败映射 500 `claim_failed`；新会话路径（`create_new_session`）claim 阶段失败（本地/KV 冲突或 KV 读/写失败）映射 500 `session_creation_failed`（stage `claim`，带 `createdSessionId`）。没有通用 503 ownership 错误码。release 故障保留记录供重试。

## 5. 事实与同步

`ChatService` 发布按绑定代次编号的 `ChatFactEnvelope`（seed、历史批次、通知、队列、任务、交互、运行态、关闭）。事实日志供晚挂 View 重放；View 不执行动作、不决定权限、不取消任务、不关闭运行后端。

- 每个绑定代次拥有独立的一对 Y.Doc 与同步源；换代时成对替换，旧代次条目不能混入新代次。
- `ChatYjsChannel` 只是逻辑帧通道：不创建 WebSocket/HTTP，不携带凭据，鉴权与物理连接由应用完成。
- 同步帧为二进制 `HELLO/SNAPSHOT/UPDATE/ACK`；用户可恢复向量、全量/增量快照、交付信用与有界背压由同步层负责。
- `ChatReplica` 只读：`readiness` 明确区分 `not-ready`、`ready`、`unsupported`；版本不支持不是空历史。公开读取为 `read()` 与 `subscribe()`；`readSessionView` 提供无状态纯读取，不提供可写文档、按身份选择器或快照门面。
- 客户端断线状态由物理传输适配器报告；断线不停止执行，重连通过 HELLO resume 补齐。

## 6. 运行后端、端口与工厂

`ChatRuntime` 描述一次 Chat 的运行实例来源，不是网络主机名；两个分支都产出可供 ACP 使用的 transport。

1. `{ kind: "wasm" }`：Manager 为每次激活启动独立 WASM 运行后端；可注入 `moduleUrl`、`moduleFactory`、`env` 与 `onDiagnostic`。`createChat({ runtime })` 可按 Chat 合并 `env`、覆盖 `onDiagnostic`，并把自己的 `agentConfigJson` 传给启动器。scheduler 与清理/启动超时由运行后端自持，调用方不可配置；`lifecycle` 未显式给出时按 runtime kind 取默认值（wasm 为 activation 25s、shutdown 30s），显式传入仍优先。
2. `{ kind: "transport", factory }`：调用方按 path 创建 transport；`factory` 收到 `{ signal, chatId, agentConfig }`（旧签名可忽略后两项），`factory.close()` 成功即停止证明，失败保留原因供重试。模型/权限隔离取决于该 factory 创建的运行时，SDK 不额外隔离。

`PeriInstance` 是内部实现，不是发布门面。工厂把句柄的路径、Store、per-chat 运行后端配置（`agentConfigJson`/`runtimeEnv`/`payloadBudgets`）与生命周期上下文转换为一次运行装配；`WasmRuntimePool` 负责运行后端租约与释放证据，`acquire` 透传 per-chat 启动配置。共享 MCP server 地址不共享 MCP client、声明路由或审批状态。

调用方可用 SDK `ChatViewRegistry` 按 Chat 复用同一个 `ChatView`：这是可选的部署策略，不是 SDK 不变量；注册表从主入口与 `/portable` 导出，不从 `/view` 导出。

## 7. 发布入口

| 入口 | 源码入口 | 内容 |
| --- | --- | --- |
| 主入口 | `src/entries/index.ts` | `ChatManager`、`ChatService`、`ChatUserAction`、`ChatView`、`ChatReplica`、`ChatViewRegistry`、公开类型/错误（含 `ChatManagerContract`）、Store 读取工具 |
| `/portable` | `src/entries/portable.ts` | 同一 Chat 公共面 + 嵌入方驱动的执行准入核心；仍依赖 `node:crypto`，不是纯浏览器入口 |
| `/view` | `src/entries/view.ts` | 浏览器安全展示出口：副本、`readSessionView`、载荷读取、同步协议；不引入 Node 运行时依赖，也不导出 `ChatViewRegistry` |
| `/wasm` | `src/entries/wasm.ts` | 平台无关的 Emscripten 运行后端、loader、transport |
| `/wasm-node` | `src/entries/wasm-node.ts` | `/wasm` + Node 网络/DNS 端口 |
| `/sandbox` | `src/entries/sandbox.ts` | server-side 的可选资源入口：`SandboxAdapter` 契约 + `SandboxManager`（多实例、per-start 预算与参数冲突校验、`closeAll`）+ `SandboxHandle`（生命周期、`WorkspaceRef` 与 MCP descriptor）；adapter 由宿主注入，不接收 session |

主入口与 `/portable` 共用的 Chat 门面在 `src/entries/chat.ts`。

## 8. 非目标

- 不恢复 stdio/Workspace 进程管理、本地数据库后端、JSONL dispatcher 或执行恢复账本；`/sandbox` 是唯一的进程内 Workspace 例外，且不进入主入口。
- 不让浏览器直接持有模型/Store 凭据，也不把运行后端执行搬进浏览器。
- 不声明跨实例 ownership、共享内存 Manager 或多租户隔离已实现；这些由部署注入或另行设计。
- 不用兼容 shim 维持两套业务入口；删除的公开门面按 breaking change 处理。
