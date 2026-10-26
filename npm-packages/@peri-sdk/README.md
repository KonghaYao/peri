# @peri-code/sdk

> 边界：SDK 不再包含进程控制。stdio transport、Workspace 进程管理、本机进程停止证明、JSONL dispatcher 与本地 SQLite 存储均已删除，不留 shim、别名或双写。ACP 连接只有一条装配路径：宿主注入 `transportFactory`（WASM 用 `startPeriWasmHost`，其他部署注入自有实现）。

Bun SDK for Peri's ACP Host over a host-injected transport. `ChatManager` 同步登记 Chat 句柄；`ChatService.execute()` 在首次绑定动作时通过默认实例工厂装配一次 `PeriInstance`，claim Session、初始化 ACP，并创建或加载会话。Peri 负责模型执行与会话持久化；SDK 只做控制面、事实与展示契约。

主入口公开 `ChatManager`、`ChatService`、`ChatUserAction`、`ChatView`、`ChatReplica`、`ChatViewRegistry`、`ChatYjsChannel`、公开类型与错误；需要替换管理器实现时面向类型导出 `ChatManagerContract`。旧 `Agent`、`ManagedAgents`、`SessionDocs`、`SessionViewStore` 与 `Sandbox` 已删除，不在任何发布入口提供，也不保留 shim、别名或双写。

```ts
import {
  AtomicSessionClaims, ChatManager, ChatUserAction, ChatView, MemoryKV, TursoStorage,
  type ChatDatabase,
} from "@peri-code/sdk";

const storage = new TursoStorage({ url: "http://127.0.0.1:8081", authToken: "local-dev" });
const database: ChatDatabase = {
  listSessions: (path) => storage.getSessions(path ?? "/workspace"),
  getSession: (id) => storage.getSession(id),
};
const manager = new ChatManager({
  agentConfig: {
    cwd: "/workspace",
    settings: myPeriConfig,
    storage: { url: "http://127.0.0.1:8081", authToken: "local-dev" },
    machineId: "00000000-0000-4000-8000-000000000001",
  },
  database,
  // 每个 Manager 一个 claims 实例；多个实例共享同一个原子 KV 驱动。
  sessionClaims: new AtomicSessionClaims(new MemoryKV()),
  host: { kind: "wasm", moduleUrl: new URL("./dist/wasm/peri-wasm.js", import.meta.url) },
  defaults: {
    path: "/workspace",
    instructions: "Help with this workspace.",
  },
});

const chat = manager.createChat({ path: "/workspace" });
const ready = await chat.execute(ChatUserAction.createNewSession({}));
const view = new ChatView(chat);

const receipt = chat.execute(ChatUserAction.send({ text: "Inspect the repository" }));
await receipt; // 等待本次输入被投递；accepted 只表示已入队。
console.log(receipt.inputId, receipt.isSent);

await chat.execute(ChatUserAction.cancel({}));
await view.dispose();
await chat.close();
await manager.shutdown();
```

`execute` 只接受 `ChatUserAction` 的八类动作：`create_new_session`、`change_session`、`send`、`force_send`、`take_back`、`load_earlier_history`、`cancel`、`answer`。传入普通 wire 对象时先经 `parseChatUserAction` 校验；Manager 不接收 `execute`。执行调度属于 Peri：SDK 只发布输入，不实现 dispatcher、工作查询或准入票据。

`ChatManagerOptions.host` 有两种装配：

- `{ kind: "wasm" }`：Manager 为每次激活启动独立 WASM 宿主，并把 `agentConfig` 序列化为启动配置；可注入 `moduleUrl`、`moduleFactory`、`env`、`ports` 与生命周期回调。
- `{ kind: "transport", factory }`：由宿主按 `path` 创建 transport；SDK 不启动 Peri 或 Workspace 进程。`factory` 的 `close()` 成功即视为停止证明，失败保留原因供重试。

宿主如需替换管理器实现或共享视图，面向契约与注册表，而不是具体类：

```ts
import { ChatViewRegistry, type ChatManagerContract } from "@peri-code/sdk";

function createHost(chats: ChatManagerContract) {
  const views = new ChatViewRegistry(); // 每个 Chat 一个 ChatView，同 Chat 的所有同步连接共享
  return {
    chats,
    views,
    async shutdown() {
      await views.disposeAll(); // 先断开同步连接与文档
      await chats.shutdown();   // 再关闭句柄、在途激活与宿主
    },
  };
}
```

`ChatManagerContract` 是 `src/chat/types.ts` 的接口，SDK 的 `ChatManager` 类实现它；`new ChatManager(options)` 只应出现在宿主装配层，测试或其它宿主可以注入自己的实现。`ChatViewRegistry` 只是可选的宿主策略，不是 SDK 不变量：宿主也可以为每条连接各建 `ChatView`，连接数仍由 `ChatView` 管理。两者都从主入口与 `/portable` 导出；`/view` 保持纯浏览器视图面，不导出注册表。

`ChatService` 的绑定动作只在空闲时允许换绑；新会话使用句柄/默认 `path`，加载历史用 Store 中的持久 `cwd`，不回退句柄 path。`findChatBySession` 覆盖 binding/cleanup 中的候选；关闭未确认时保留 claim 与索引供重试。

`AtomicSessionClaims` 包装共享原子 KV：Session claim key 只含 Session ID，不含 path 或 Chat ID。`MemoryKV` 只适用于单进程；跨进程必须注入实现 `claimIfAbsent(key, owner)` / `releaseIfOwner(key, owner)` 的原子适配器。普通 unjs `Storage` 没有 compare-and-set，会被拒绝。同一 Session 的并发绑定失败抛 `SessionBindingConflictError`，且不会启动 transport。适配器负责自己的崩溃恢复或租约策略；过期本身不是旧执行已停止的证明。

关闭是有预算的所有权事务：`ChatLifecycleOptions` 提供 `closeDrainTimeoutMs`、`startupSettleTimeoutMs`、`activationCloseTimeoutMs`、`shutdownChatTimeoutMs`。`session/close` 在预算内重放；超时返回 `ChatCloseIncompleteError`，保留 claim、索引与资源，不产生 `closed` 事实。`manager.shutdown()` 先封闭新准入，再聚合清理；可重试。

## 展示与同步

`ChatView` 消费 `ChatFact` 并维护一对 Y.Doc；晚挂 View 由事实日志重放重建。View 只读事实：不执行动作、不决定权限、不取消任务、不关闭宿主。

```ts
const view = new ChatView(chat);

// 服务端：把 Yjs 同步帧写到 WebSocket（或其它二进制通道）。
const connection = view.sync(channel); // channel: ChatYjsChannel

// 浏览器：ChatReplica 收 HELLO/SNAPSHOT/UPDATE/ACK，提供只读 SessionView。
import { ChatReplica } from "@peri-code/sdk/view";
const replica = new ChatReplica();
replica.connect(channel);
const unsubscribe = replica.subscribe(() => render(replica.read()));
await replica.dispose();
```

`ChatYjsChannel` 只约定帧收发与关闭，不创建物理连接；鉴权与传输由应用负责。`ChatReplica.read()` 返回 `SessionView`：`readiness`（`not-ready`/`ready`/`unsupported`）、完整 entries/blocks/tools/turns/tasks/plans、session info、input queue、interaction 记录与 `pendingInteractions`。未知块与孤立工具引用显式标记为 `unknown`/`unavailable`，不静默丢弃。`readSessionView` 与 `readPayload` 从 `/view` 导出；`SessionViewStore` 和按身份选择器（`selectEntry`、`selectToolCall`、`selectTurn`、`selectPlanForTurn`、`selectPendingInteractions`）已删除，消费方直接读取 `SessionView` 的数组，或按 id 自建索引；流式通知用 `ChatReplica.subscribe()`。

`readPayload(ref, reader, { signal, maxBytes })` 校验返回 id、UTF-8 字节数与 JSON；`ChatService.readPayload({id, bytes})` 直接读取当前激活的载荷仓库，错误码为 `unauthorized`、`unavailable`、`too_large`、`integrity_error`、`aborted`、`transport_error`。引用字节数与存储不一致或非法 JSON 属于 `integrity_error`，不是 `unavailable`。默认单次展示上限 1 MiB；`limitPayloadReads` 默认并发 2。

载荷预算仍是投影级默认值（待设计评审确认）：inline JSON ≤4096 字节留在文档；更大的值或超出会话 inline 预算（1 MiB）后变成 `{id,bytes,preview}`（preview 512 字符），全文留在运行侧；单次外置 >8 MiB 或超过会话外置预算 32 MiB 时拒绝，工具卡保留上一个可读版本并显示公开 `payload_too_large`。

## MCP over ACP

`defaults.mcpServers` 支持 `http`、`stdio` 与 `acp` 声明。`acp` 声明只序列化 `{type:"acp", name, serverId}`，Peri 的反向 `mcp/message` 由 SDK 桥接到官方 server 实例；`serverId` 缺省使用 Record key。

直接实例是调用方借用的单次使用实例：bridge 关闭后保留绑定，不能安全重连。跨 reload 或多个 Chat 使用时必须提供工厂：

```ts
defaults: {
  mcpServers: {
    "my-tools": {
      type: "acp",
      // 每次激活解析新实例；由激活拥有并关闭。
      server: async () => createMyMcpServer(),
    },
  },
}
```

工厂返回的实例必须实现 `close()`；解析失败时已创建实例会被关闭。SDK 不依赖官方 MCP 包运行时。

## 入口与示例

- 主入口：Chat 控制面、展示契约、config/storage 读取工具。
- `/view`：浏览器安全展示出口；与主入口共享包内 Yjs 依赖，直接服务浏览器时需要打包该依赖。`ChatViewRegistry` 是宿主共享策略，只在主入口与 `/portable` 导出，不在此入口。
- `/wasm`：平台无关的 Emscripten ACP Host、loader 与 transport；`/wasm-host` 额外提供 Node-compatible 网络/DNS 端口。
- `/portable`：不装配 Bun/stdio 进程或本地 SQLite；Chat 控制面 + 执行准入核心。准入摘要仍使用 `node:crypto`，不能宣称纯浏览器支持。

`MISE_ENV=wasm mise exec -- bun run build` 构建 `peri-wasm` release 产物，并把 `peri-wasm.js` 与 `peri_wasm.wasm` 放入 `dist/wasm/`。构建依赖 Emscripten 6.0.10；`mise exec` 会注入 `EMSDK`，直接 `bun run build` 在仅有 mise shim 的环境会把 shim 路径当作工具链并失败。工具链不满足时构建失败，不能用旧 `dist` 冒充。默认 [demo.ts](examples/demo/demo.ts) 使用 `host.kind="wasm"`；[demo-wasm.ts](examples/demo/demo-wasm.ts) 使用 `host.kind="transport"` 注入 `WasmAcpTransport`。两者都通过 `ChatView` + 二进制同步帧驱动浏览器 `ChatReplica`；示例用 SSE 适配器承载逻辑帧，生产消费端使用 WebSocket。

## Durable Execution Admission

`/portable` 额外公开可自行驱动的准入核心。SDK Chat 不使用它：输入发布是 ACP 队列命令，调度由 Peri mailbox 决定，因此没有 SDK 侧票据、dispatcher generation 或反向准入端点。

`KvExecutionRegistry` 包装 `AtomicExecutionKv` 提供 durable read/CAS apply/resolve；适配器必须实现版本化读取与 compare-and-set，只实现 claim/release 会被拒绝。`MemoryExecutionKv` 是单进程 `bestEffort` 实现，需要协调器显式用 `allowBestEffort` 选择。注册表写入与回执原子；解析不存在的原始 mutation 会把 NotApplied 封印，防止迟到执行。durable backend、账本与停止证明由嵌入方注入；SDK 不提供本地数据库文件，也不提供基于进程的停止证明。

票据绑定精确的 session、instance/generation、lifecycle/control generation、稳定 work/revision 与 `{turnId, attemptId}`。持久 attempt 经过 queued/entering/running；pause/control 变化使未进入的 work 失效，execution entry 重新校验资格。重放原始准入不会再次消耗稳定 work 预算；未知执行入口只解析原始票据，绝不盲目签发第二次执行。预算跨实例替换保留；缺失停止证明返回 Blocked，不做 lease/TTL 接管。

## Store 与配置

`TursoStorage({ url, authToken })` 通过官方 `@tursodatabase/serverless` 驱动读取会话元数据，凭证由调用方显式提供；没有本地 SQLite 文件后端、CLI 部署路径或环境变量 token 回退。`ChatDatabase.listSessions/getSession` 是 Manager 唯一读取目录的入口，不调用 ACP `session/list`。

`PeriConfig` 描述 WASM ACP 启动配置；settings、storage 与 machineId 一起经 `agentConfig` 交给 Peri。`BareHarnessConfig` 是冻结的全关闭预设，按需开启能力。`instructions` 使用 `session/new._meta["peri.instructions"]`；HTTP Workspace 与额外 MCP 使用 `session/new.mcpServers`。

Machine/workspace 绑定与 SDK 执行所有权是两件独立职责，不能互相推导。
