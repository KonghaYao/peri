# @peri-code/sdk

在宿主提供的 ACP transport 上运行 Peri，并给应用一套可读的 Chat 控制面、事实流和只读视图。

SDK 主入口不启动或停止 Peri CLI、Workspace 进程，也不提供本地 SQLite 后端。可选的 `@peri-code/sdk/sandbox` server-side 子入口按宿主显式注入的 adapter 管理 sandbox 生命周期。宿主负责提供 transport、持久化 Store，并在路由层保证同一 Session 只落在一个实例上。

下面的接入代码来自 [`peri-cf`](../peri-cf/server/agent/compose.ts) 的真实装配，而不是抽象伪代码。

## 适合什么场景

| 适合 | 不适合 |
| --- | --- |
| 在 Bun/Node/浏览器宿主里嵌入 Peri | 用 SDK 启动/停止 Peri CLI |
| 多个 Chat 句柄、历史加载、取消、交互回答 | 多实例调度、租约或 fencing |
| 服务端事实流 + 浏览器只读视图 | 由 SDK 管理 Workspace 文件系统 |

## 安装

```bash
bun add @peri-code/sdk
# 宿主自己选择具体实现；SDK 不依赖 unstorage
bun add unstorage
```

## 快速开始

完整例子按 `peri-cf/server/agent/compose.ts` 的装配顺序展开。`store.storage` 与 `machineId` 是接入时最容易混淆的两个字段，注释里写明了区别。

```ts
import { createStorage } from "unstorage";
import {
  BareHarnessConfig, ChatManager, ChatUserAction, ChatViewRegistry, TursoStorage,
  type PeriConfig, type KvStorage,
} from "@peri-code/sdk";

// 1. Store 连接只声明一次：database 同时提供 SDK 只读目录与 Peri 写侧配置。
//    本地 sqld 也要求非空 authToken；生产从环境变量或密钥管理读取。
const storeConnection = {
  url: process.env.PERI_STORAGE_URL ?? "http://127.0.0.1:8081",
  authToken: process.env.PERI_STORAGE_TOKEN ?? "local-dev",
};

// 2. SDK 的会话目录：TursoStorage 直接实现 ChatDatabase（只读）。
//    store.storage 使用它的 storageConfig 纯 JSON 快照；写入仍由 Peri 完成。
const database = new TursoStorage(storeConnection);

// 3. Session ownership：只需 getItem/setItem/removeItem。
//    peri-cf 默认用 unstorage 的进程内 memory storage；
//    多实例必须换共享 driver，并由路由层保证单实例绑定。
const kv: KvStorage = createStorage();

// 4. machineId：宿主持久化的 UUID，标识一个逻辑执行环境。
//    peri-cf 从 PERI_MACHINE_ID 读取并校验 UUID。
//    不要用 sessionId/workspaceId 代替，也不要每次启动随机生成；
//    换 machineId 后，旧 Workspace/Session 可能无法按原身份找到。
const machineId = process.env.PERI_MACHINE_ID ?? "00000000-0000-4000-8000-000000000001";

// 5. PeriConfig：与 peri-cf 一样，模型与 Store 凭据只存在于服务端进程。
const settings: PeriConfig = {
  config: {
    active_alias: "sonnet",
    providers: [{
      id: "model",
      type: "openai",
      apiKey: process.env.PERI_MODEL_API_KEY!,
      baseUrl: process.env.PERI_MODEL_BASE_URL!,
    }],
    profiles: {
      sonnet: {
        provider: "model",
        model: process.env.PERI_MODEL_ID!,
      },
    },
    // peri-cf 按部署开启需要的 harness；这里只保留最小对话能力。
    meta_harness: {
      ...BareHarnessConfig,
      TodoMiddleware: true,
      McpMiddleware: true,
      ToolSearch: true,
    },
  },
};

const manager = new ChatManager({
  // Manager = 部署/Store 边界：Store 与机器身份由整个 Manager 共享。
  store: {
    storage: database.storageConfig, // 纯 JSON 连接配置；Peri 写侧使用同一 Store
    machineId,
  },
  database,
  kv,
  chatConfig: {
    runtime: {
      kind: "wasm",
      moduleUrl: process.env.PERI_WASM_MODULE_URL
        ?? new URL("./dist/wasm/peri-wasm.js", import.meta.url),
      // 可选：只把遥测变量交给 WASM 实例，避免服务端凭据扩散到模块环境。
      // 未配置 Langfuse 时删除整个 env。
      env: {
        LANGFUSE_PUBLIC_KEY: process.env.LANGFUSE_PUBLIC_KEY!,
        LANGFUSE_SECRET_KEY: process.env.LANGFUSE_SECRET_KEY!,
        LANGFUSE_BASE_URL: process.env.LANGFUSE_BASE_URL ?? "https://cloud.langfuse.com",
      },
      onDiagnostic: (kind, error) => console.error("[peri]", kind, error),
    },
  },
});

// 6. 每个 Chat 一个共享 View；同 Chat 的所有同步连接复用它。
const views = new ChatViewRegistry();
const chat = manager.createChat({
  // workspace = 该 Chat 的边界：runtimePath 是 Agent/Session 看到的根。
  // hostPath 是宿主文件系统路径；远程 sandbox 没有本地路径时可省略。
  workspace: {
    id: "ws-local",
    hostPath: "/srv/peri/workspaces/ws-local",
    runtimePath: "/workspace",
  },
  // 可选：相对 workspace.runtimePath 的会话 cwd；越界以 workspace_mismatch 拒绝。
  sessionPath: "",
  // agent = per-chat 运行配置；cwd 由 SDK 注入为 workspace.runtimePath。
  agent: { settings },
  session: {
    instructions: "Help with this workspace.",
    mcpServers: {},
  },
});

// 首次 execute 才绑定 Session。
const ready = await chat.execute(ChatUserAction.createNewSession({}));
console.log(ready.sessionId);

// accepted 表示输入已入队，不表示 Peri 已执行完成。
const receipt = chat.execute(ChatUserAction.send({ text: "Inspect the repository" }));
await receipt.accepted;
console.log(receipt.inputId);

// 加载历史会话。
await chat.execute(ChatUserAction.changeSessionTo(ready.sessionId));

// 服务端把 Yjs 同步帧写到 WebSocket 或其它二进制通道。
const view = views.attach(chat);
view.sync(channel); // channel: ChatYjsChannel

// 关闭：先关 Chat，再释放 View。
await manager.closeChat(chat.id);
await views.dispose(chat.id);

// 进程退出：先释放所有 View，再让 Manager 聚合关闭。
await views.disposeAll();
await manager.shutdown();
```

## 四个必填输入

| 选项 | 作用 | 消费者 |
| --- | --- | --- |
| `store` | Manager 部署边界：`storage`（Peri 写侧 Store 配置）与 `machineId`（持久化执行环境 UUID）；整个 Manager 共享 | `ChatManager` |
| `database` | 会话目录读取：`listSessions`、`getSession` | `ChatManager` |
| `kv` | Session 绑定记录，3 个 KV 方法 | `ChatManager` |
| `chatConfig` | 运行后端、可选默认 Session 建立输入、生命周期预算；per-chat 可覆盖 | `ChatManager` |

### `store` 与 per-chat `agent`

Manager 只声明部署边界：`store.storage` 与 `store.machineId` 由整个 Manager 共享，构造时校验并深拷贝快照。序列化每个 Chat 的内部 `agentConfig` JSON 时，SDK 会注入这两个值；per-chat 配置出现 `storage`/`machineId` 一律抛 `TypeError`（即使值与 Manager 相同）。

`createChat` 必填提供 per-chat `agent`：

| 字段 | 说明 |
| --- | --- |
| `cwd` | 可选；SDK 最终注入 `workspace.runtimePath`。显式提供时必须等于该值，否则抛 `workspace_mismatch` |
| `settings` | 可选；整份 `PeriConfig`，替换 Peri 的全局配置 |
| `env` | 可选；字符串映射 |
| 其他字段 | 其余 JSON 字段原样传递 |

per-chat `agent` 必须是 JSON 安全对象；`cwd` 非字符串/空串、配置不是对象、含 `storage`/`machineId` 都会抛 `TypeError`。每个 Chat 得到独立的深拷贝、冻结并序列化的 JSON，不复用 Manager 或其他 Chat 的引用。

`store.storage` 不是 `database`：

1. `store.storage` 由 Peri 消费，用来写 Session、消息与运行状态。
2. `database` 由 SDK 消费，只用来读会话目录；SDK 不调用 ACP `session/list`。
3. 两者可以指向同一个 libSQL 端点，但契约不同，不能互相替代。
4. `TursoStorage` 同时提供两者：`database` 传实例，`store.storage` 传 `database.storageConfig`；默认 database 只构造一次。

`machineId` 不是 Session、Workspace 或 Chat 的标识：

1. 它标识一份可跨实例持续使用的逻辑执行环境，必须由宿主持久化并复用。
2. 同一次部署重启后应保持不变；换值等于换了一个执行环境。
3. 一个 WASM 模块实例只承载一个 `machineId`，运行期不能切换。
4. 本地 demo 的固定 UUID 只用于演示，生产必须从配置或密钥管理读取。

`createChat({ workspace, sessionPath })` 决定会话 cwd：

1. `workspace` 必填，`runtimePath` 必须是绝对路径；SDK 归一化 `.`/`..` 与分隔符后作为 Agent 与 Session 的共同根。
2. `sessionPath` 可选且相对 `runtimePath`；省略表示 workspace 根，`../` 越界或传入绝对路径都会以 `workspace_mismatch` 拒绝。
3. 不再接受 `session.path`；公开的 `ChatSessionOptions` 只含 `instructions`/`mcpServers`，cwd 一律从 workspace 派生。
4. `hostPath` 是宿主文件系统路径，`runtimePath` 是 Agent/session 看到的路径；本地 sandbox 两者相同，远程 sandbox 可省略 `hostPath`。
5. Workspace 只是路径引用，不拥有 sandbox 或 session；MCP 描述符与 token 仍由 sandbox adapter / `SandboxHandle.mcpServer()` 提供。

### `database`

```ts
class TursoStorage implements ChatDatabase {
  constructor(options: {
    url: string;
    authToken?: string;
    legacyCwd?: string;   // 默认 "/workspace"
    currentCwd?: string;  // 默认 "/workspace/"
  });
  get storageConfig(): { url: string; authToken?: string };
  listSessions(path?: string): Promise<ChatSessionDirectoryEntry[]>;
  getSession(id: string): Promise<ChatSessionDirectoryEntry | null>;
}
```

`TursoStorage` 直接实现 `ChatDatabase`，是 Manager 读取会话目录的唯一入口。显式 `path` 只查该 cwd；省略时串行读取 `legacyCwd` 与 `currentCwd`，同 id 以当前 cwd 为准并按 `updatedAt` 倒序。缺表错误收敛为空目录，其它错误照常抛出。凭证由调用方显式提供，没有本地 SQLite 文件后端、CLI 部署路径或环境变量 token 回退。

`storageConfig` 返回只含 `url` 与 `authToken` 的普通 JSON 对象；`authToken` 未设置时省略该键，也不包含 `legacyCwd`/`currentCwd`。每次读取返回新对象，可直接作为 `store.storage`。

### `kv`

```ts
interface KvStorage {
  getItem(key: string): Promise<unknown>;
  setItem(key: string, value: unknown): Promise<void>;
  removeItem(key: string): Promise<void>;
}
```

`ChatManager` 用它记录“这个 Session 当前由哪个 Manager 持有”。记录 key 为 `ownership:<base64url(sessionId)>`，value 为 `{ sessionId, owner }`。

1. 只做读后写，非原子；记录是绑定标记，不是互斥原语。
2. 同一 Session 的单实例绑定由路由层串行化保证；SDK 只做顺序冲突检测。
3. 没有 TTL、续租或心跳；崩溃遗留记录由路由层在旧实例停止后调用 `clearSessionOwnership(storage, sessionId)` 清理。
4. 公开面只有 `KvStorage` 与 `clearSessionOwnership`；`SessionOwnership` 等协调器是内部实现。
5. 错误映射：加载路径冲突 → 409 `binding_conflict`；加载路径 KV 故障 → 500 `claim_failed`；新会话 claim 失败 → 500 `session_creation_failed`（stage `claim`）。没有 ownership 专用的 503。

`unstorage` 的 `Storage` 结构化满足这个端口；SDK 不 import unstorage，具体实现由宿主安装。`createStorage()` 默认是进程内 memory storage，只适合单进程；多实例必须换共享 driver。

### `chatConfig`

| 字段 | 说明 |
| --- | --- |
| `runtime` | 必填。`{ kind: "wasm" }` 在进程内启动 WASM 模块；`{ kind: "transport", factory }` 由宿主按 path 创建 transport |
| `session` | 可选默认值：`instructions`、`mcpServers`（不含 cwd）；`createChat({ session })` 按 chat 整体覆盖，不做字段合并 |
| `lifecycle` | `closeDrainTimeoutMs`、`startupSettleTimeoutMs`、`activationCloseTimeoutMs`、`shutdownChatTimeoutMs`；`createChat({ lifecycle })` 按字段覆盖 |

`runtime.kind="wasm"` 时，Manager 为每次激活启动独立 WASM 运行后端；可注入 `moduleUrl`、`moduleFactory`、`env` 与 `onDiagnostic`。`createChat({ runtime: { env } })` 的 env 会按字段合并到 Manager env 上，`onDiagnostic` 覆盖该 Chat 的诊断回调。scheduler 与清理/启动超时由 SDK 内部管理，用户不可配置。`lifecycle` 未显式给出时按 runtime kind 取默认值：wasm 为 activation 25s、shutdown 30s；transport 为 5s、15s。per-chat `lifecycle.shutdownChatTimeoutMs` 不改变 Manager 聚合等待预算。

`runtime.kind="transport"` 时，SDK 不启动 Peri 或 Workspace 进程；`factory(path, { signal, chatId, agentConfig })` 接收当前 Chat 的 id 与包含 Manager store 的内部 agentConfig JSON，旧工厂可忽略新增字段。`factory` 的 `close()` 成功即视为停止证明，失败保留原因供重试。模型与权限隔离取决于 factory 创建的运行时；WASM 分支每次激活都启动独立实例，不共享执行环境。

### `createChat`

| 字段 | 说明 |
| --- | --- |
| `workspace` | 必填；`{ id, hostPath?, runtimePath }`，`runtimePath` 必须是绝对路径，决定 `agent.cwd` 与会话 cwd |
| `sessionPath` | 可选；相对 `workspace.runtimePath` 的新会话 cwd，越界以 `workspace_mismatch` 拒绝 |
| `session` | 该 Chat 的 `instructions`、`mcpServers`；传值时整体覆盖 `chatConfig.session`，不做字段合并 |
| `agent` | 必填；per-chat 运行配置，`cwd` 可选且只能等于 `runtimePath`，不能含 `storage`/`machineId`；Manager 注入 store 后独立冻结、序列化 |
| `lifecycle` | 按字段覆盖 Manager 生命周期预算；`shutdownChatTimeoutMs` 仍由 Manager 聚合 |
| `runtime` | WASM 运行参数：`env` 合并到 Manager env，`onDiagnostic` 覆盖该 Chat 的回调 |
| `payloadBudgets` | 传给该 Chat 的 `PeriInstance`；没有 Manager 默认值，不传时使用 PeriInstance 默认预算 |

未传 `session` 时使用 Manager 默认值；两者都没有时使用 `{}`。`session.mcpServers` 容器、http `headers`、stdio `args`/`env` 都按 Chat 复制（`acp` 的 `server` 实例保留引用）；runtime env 也按 Chat 复制/合并。`createChat` 同步登记句柄，不启动 ACP；每个 Chat 的配置互不影响。缺少 `workspace`、`runtimePath` 非绝对路径、`agent` 不是 JSON 对象、或出现 `storage`/`machineId` 都会抛 `TypeError`。

## 核心概念

1. **Chat 句柄**：`manager.createChat({ workspace, sessionPath, agent, session, lifecycle, runtime, payloadBudgets })` 同步登记，不启动 ACP、不取得 ownership、不读历史；`workspace` 与 `agent` 必填，store 由 Manager 注入。
2. **Session**：Peri 持久化的会话；只有首次绑定动作才会创建或加载。
3. **绑定**：`create_new_session` 使用 `sessionPath` 派生的 cwd；`change_session` 使用 Store 中的持久 cwd。加载历史的 cwd 必须等于或位于 `workspace.runtimePath` 之下，否则以 `workspace_mismatch` 拒绝。Manager 同时维护内存 `sessionId → workspaceId` 映射：已知 Session 属于其他 workspace 时优先按 id 拒绝；持久 Session 没有该元数据时才回退路径校验。workspaceId 不写入 ACP 字段。
4. **ownership**：同一 Session 同时至多一个执行所有者；只在清理确认后释放，关闭超时或失败时保留。
5. **事实与 View**：`ChatService` 发布 `ChatFact`；`ChatView` 重放事实并维护只读 Y.Doc。View 不执行动作、不决定权限、不取消任务。

## 常用操作

`execute` 只接受 `ChatUserAction` 的八类动作；推荐用工厂方法构造：

| 动作 | 工厂方法 | 用途 |
| --- | --- | --- |
| `create_new_session` | `ChatUserAction.createNewSession({ title? })` | 创建并绑定新 Session |
| `change_session` | `ChatUserAction.changeSessionTo(sessionId)` | 加载已有 Session |
| `send` | `ChatUserAction.send({ text })` | 发送输入；`accepted` 只表示已入队 |
| `force_send` | `ChatUserAction.forceSend(inputId)` | 立即派发队列中的输入 |
| `take_back` | `ChatUserAction.takeBack(inputId)` | 撤回队列中的输入 |
| `load_earlier_history` | `ChatUserAction.loadEarlierHistory({ limit? })` | 向前加载历史 |
| `cancel` | `ChatUserAction.cancel()` | 取消当前执行 |
| `answer` | `ChatUserAction.answer({ interactionId, answer })` | 回答待决交互 |

`ChatUserAction.send()` 返回 `SendReceipt`：`accepted` 表示已入队，不是执行完成。`create_new_session` 与 `change_session` 返回 `SessionReadyResult`，包含 `chatId`、`sessionId`、`bindingRevision`。

传入普通 wire 对象时先经 `parseChatUserAction` 校验。Manager 不接收 `execute`；执行调度属于 Peri。`peri-cf` 的 HTTP 层还会用 `chat.binding` 与 `bindingRevision` 拒绝过期连接的动作。

## 展示与同步

```ts
import { ChatReplica } from "@peri-code/sdk/view";

const views = new ChatViewRegistry();
const view = views.attach(chat);          // 同 Chat 复用同一个 View
view.sync(channel);                       // channel: ChatYjsChannel

// 浏览器：ChatReplica 收 HELLO/SNAPSHOT/UPDATE/ACK，提供只读 SessionView。
const replica = new ChatReplica();
replica.connect(channel);
const unsubscribe = replica.subscribe(() => render(replica.read()));
await replica.dispose();
```

`ChatYjsChannel` 只约定帧收发与关闭，不创建物理连接；鉴权与传输由应用负责。`ChatReplica.read()` 返回 `SessionView`，包含 readiness、entries/blocks/tools/turns/tasks/plans、session info、输入队列、交互记录与 `pendingInteractions`。未知块与孤立工具引用显式标记为 `unknown`/`unavailable`，不静默丢弃。

`readSessionView` 与 `readPayload` 从 `/view` 导出。`SessionViewStore` 和按身份选择器已删除；消费方直接读取 `SessionView` 的数组，或按 id 自建索引。

`readPayload(ref, reader, { signal, maxBytes })` 校验返回 id、UTF-8 字节数与 JSON；`ChatService.readPayload({id, bytes})` 直接读取当前激活的载荷仓库，错误码为 `unauthorized`、`unavailable`、`too_large`、`integrity_error`、`aborted`、`transport_error`。默认单次展示上限 1 MiB；`limitPayloadReads` 默认并发 2。

## MCP over ACP

`session.mcpServers`（来自 `chatConfig.session` 默认值或 `createChat({ session })`）支持 `http`、`stdio` 与 `acp` 声明。每个 Chat 复制 `mcpServers` 容器、http `headers` 与 stdio `args`/`env`；`acp` 只复制描述对象，`server` 实例/工厂保持原引用。`acp` 声明只序列化 `{type:"acp", name, serverId}`，Peri 的反向 `mcp/message` 由 SDK 桥接到官方 server 实例；`serverId` 缺省使用 Record key。

### 配合官方 McpServer

`acp` 声明把应用进程内已有的官方 MCP Server 通过 ACP 反向 `mcp/message` 暴露给 Peri：

```ts
import { McpServer } from "@modelcontextprotocol/server";

// 每次激活创建新实例；SDK 负责 connect/close，不要自己连接 stdio/HTTP。
const createMyMcpServer = () => {
  const server = new McpServer({ name: "my-tools", version: "1.0.0" });
  server.registerTool("echo", { description: "Echo text" }, async () => ({
    content: [{ type: "text", text: "ok" }],
  }));
  return server;
};

const workspace = {
  id: "workspace-default",
  hostPath: "/workspace",
  runtimePath: "/workspace",
};
const chat = manager.createChat({
  workspace,
  agent: {}, // agent.cwd 由 workspace.runtimePath 派生
  session: {
    mcpServers: {
      "my-tools": {
        type: "acp",
        server: createMyMcpServer,
      },
    },
  },
});
```

1. 官方 `McpServer` 负责 tools/resources/prompts；桥接只依赖实例的 `connect(transport)`，不需要适配层，直接把实例或工厂放进 `type:"acp"` 的 `server`。
2. 不要手动 `connect()`：SDK 在 `session/new|load` 前连接。
   - 工厂实例由激活拥有，结束时由 SDK `close()`。
   - 直接传入的实例由调用方持有，SDK 不会关闭它；不再使用时由调用方自行 `close()`。
3. 直接传实例是单次借用，bridge 关闭后不能安全重连；跨 reload 或多个 Chat 必须用工厂，工厂结果必须有 `close()`（`McpServer` 已提供）。
4. 边界：ACP 只承载 `{serverId, requestId, method, params}`，不重新定义 MCP；sampling/roots 等 server→client 请求返回 `-32601`。

`@peri-code/sdk` 不自带官方 MCP 包，使用方自行安装：v2 用 `bun add @modelcontextprotocol/server`；v1 单包路径为 `@modelcontextprotocol/sdk/server/mcp.js`，两版都可直接传入。

## 沙箱生命周期（可选，server-side / host runtime）

`@peri-code/sdk/sandbox` 是独立的 server-side（宿主运行时）子入口，提供 `SandboxAdapter` 契约、`SandboxManager` + `SandboxHandle` 三层：SDK 只定义契约与按 `sandboxId` 的资源注册表，具体 sandbox 实现由宿主通过 `adapter` 注入。它不是 daemon、control socket 或 SQLite registry，也不感知 session；sandbox 状态只存在于 manager 跟踪的 handle 和 adapter 自己的持久化数据中。主入口、`/portable`、`/wasm`、`/view` 不加载它；SDK 核心不 import、不动态 import、也不声明任何 adapter 的 peer 依赖。

该入口要求宿主运行时为 Node 22.12+ 或 Bun；浏览器、WASM、Edge 不可用。当前只支持 macOS / Linux；Linux 需要可用的 systemd user session，Windows 不支持。

`adapterOptions` 是 adapter 私有的启动配置：manager 默认值可被单次 `start` 整体替换，不会与默认值合并。`adapterOptions` 用 key 排序后的稳定 JSON 比较；包含函数、类实例、循环引用等不可序列化值时不会静默视为相同。

### 安装与前置条件

SDK 不声明任何 adapter 依赖。使用 `/sandbox` 时自行安装一个 adapter 实现并注入；官方实现是 `peri-sandbox`：

```bash
bun add @peri-code/sdk
bun add peri-sandbox   # 仅在使用 /sandbox 时安装
```

> `peri-sandbox` 当前仍在仓库内且标记为 private；发布前请用本地路径或 workspace 链接安装，发布后再使用上面的包名。

`init` 是可选的环境固定步骤，不是启动前置条件。不传 `settingsPath` 时，adapter 使用默认 settings 路径（`~/.peri-sandbox/settings.json`，可用 `PERI_SANDBOX_SETTINGS` 覆盖）；该文件不存在则回退到内存默认配置，`start()` 可直接用。前提是 Peri 可被发现：`PERI_PATH` 或默认 `~/.peri/peri`，否则报 `unsupported_environment`。

```bash
bunx peri-sandbox doctor   # 可选：检查平台、Peri、SRT/cgroup，只报告不清理
```

需要固定 settings 路径、哈希或自定义 profile 时，再执行 `init`：

```bash
bunx peri-sandbox init     # 写入 ~/.peri-sandbox/settings.json（0600）与 sandboxes/（0700）
```

`init` 会把 `periPath` 与 SHA-256 固化进 settings，不生成 token、端口或后台服务。`settingsPath` / `sandboxesRoot` 通过 `createPeriSandboxAdapter({ settingsPath, sandboxesRoot })` 传入，两者都可省略；传递显式 `settingsPath` 时文件必须已存在（先 `init` 或手工创建），否则启动报 `invalid_options`。需要显式指定时用 `node:os` 的 `homedir()` 加 `node:path` 的 `join()` 拼接，不要在 JS 参数里写 `"~/..."`，字符串里的 `~` 不会被展开。per-sandbox 的 `profileName` / `profile` / `port` / `bashCalls` / `minFreeBytes` 等通过 `adapterOptions` 传入。

### 用法

Manager 独立于会话：它不接收 session，也不记录 session 映射。`handle.workspace` 返回 `{ id, hostPath, runtimePath }`，`handle.mcpServer()` 返回 ready sandbox 的 MCP 描述符；调用方把 `workspace` 与 MCP 描述符组装进 `createChat`，绑定到哪个会话也由调用方决定。Workspace 不拥有 sandbox 或 session；token 仍只在 `handle.mcpServer()` 的 descriptor 中。

```ts
import { ChatManager } from "@peri-code/sdk";
import { SandboxManager } from "@peri-code/sdk/sandbox";
import { createPeriSandboxAdapter } from "peri-sandbox";

const sandboxes = new SandboxManager({
  // adapter 必填；SDK 不提供默认 adapter，也不动态 import 任何实现。
  // 不传 settingsPath：默认 settings 不存在时回退内存默认配置，无需 init。
  adapter: createPeriSandboxAdapter(),
  // manager 默认值；可被单次 start 整体替换。
  adapterOptions: { profileName: "workspace-default" },
  serverName: "workspace",
});

const sandbox = await sandboxes.start({
  sandboxId: "sbx_0123456789abcdef0123456789abcdef", // 可选；不传时生成新的 sbx_<32 hex>
  // per-start adapterOptions 整体替换 manager 默认值。
  adapterOptions: {
    profileName: "workspace-default",
    minFreeBytes: 512 * 1024 * 1024,
  },
  // handle 自身的启动/停止预算；省略时继承 manager 默认值。
  startupTimeoutMs: 60_000,
  stopTimeoutMs: 10_000,
});
const mcp = await sandbox.mcpServer();
const workspace = sandbox.workspace!; // { id, hostPath, runtimePath }

const manager = new ChatManager({
  // Manager = 部署/Store 边界：Store 与机器身份由整个 Manager 共享。
  store: {
    storage: database.storageConfig,
    machineId,
  },
  database,
  kv,
  chatConfig: {
    runtime: { /* ... */ },
    // 可选默认值；这里不传，由每个 chat 自己决定
  },
});

// sandbox 绑定到哪个 chat 由调用方决定。
const chat = manager.createChat({
  // workspace 决定 agent.cwd 与会话 cwd；不需要再传 agent.cwd/path。
  workspace,
  agent: { settings },
  session: {
    instructions: "Use the sandbox workspace.",
    mcpServers: { [mcp.name]: mcp.config },
  },
});

// 先关闭 Chat 控制面，再关闭沙箱
await manager.shutdown();
await sandbox.close(); // 终止实例，保留 workspace
// 或：await sandbox.close({ deleteData: true }); // 同时删除 <sandboxesRoot>/<sandboxId>
// 或：await sandboxes.closeAll(); // 关闭 manager 跟踪的全部 handle
```

需要固定 settings 路径时，在 adapter 选项中显式传入（文件必须已存在或先 `init`；JS 不展开 `~`）：

```ts
import { homedir } from "node:os";
import { join } from "node:path";
import { createPeriSandboxAdapter } from "peri-sandbox";

const adapter = createPeriSandboxAdapter({
  settingsPath: join(homedir(), ".peri-sandbox", "settings.json"),
  sandboxesRoot: join(homedir(), ".peri-sandbox", "sandboxes"), // 可选：覆盖 settings 中的部署根
});
```

### 两个 Chat 绑定两个 sandbox workspace

Manager 只持有部署级 `store.storage`/`store.machineId`；每个 Chat 用自己的 `workspace` 决定 WASM 运行时工作区与会话 cwd，用 `session.mcpServers` 绑定该 workspace 的 MCP：

```ts
const manager = new ChatManager({
  // storage/machineId 由整个 Manager 共享；每个 Chat 的 workspace 决定 runtimePath，agent.cwd 由 SDK 派生。
  store: { storage: database.storageConfig, machineId },
  database,
  kv,
  chatConfig: {
    runtime: {
      kind: "wasm",
      moduleUrl: new URL("./dist/wasm/peri-wasm.js", import.meta.url),
      env: { LANGFUSE_PUBLIC_KEY: process.env.LANGFUSE_PUBLIC_KEY! },
    },
  },
});

const [alpha, beta] = await Promise.all([
  sandboxes.start({ sandboxId: "sbx_alpha" }),
  sandboxes.start({ sandboxId: "sbx_beta" }),
]);
const alphaMcp = await alpha.mcpServer();
const betaMcp = await beta.mcpServer();

// 两个 Chat 各自绑定一个 workspace；运行配置与会话 cwd 都在自己的 sandbox 内。
const alphaChat = manager.createChat({
  workspace: alpha.workspace!,
  agent: { settings },
  session: {
    instructions: "Work only inside the alpha workspace.",
    mcpServers: { [alphaMcp.name]: alphaMcp.config },
  },
  runtime: { env: { PERI_TENANT: "alpha" } },
});
const betaChat = manager.createChat({
  workspace: beta.workspace!,
  agent: { settings },
  session: {
    instructions: "Work only inside the beta workspace.",
    mcpServers: { [betaMcp.name]: betaMcp.config },
  },
  runtime: { env: { PERI_TENANT: "beta" } },
});
```

`change_session` 会拒绝加载 cwd 不在该 Chat `workspace.runtimePath` 下的持久会话；同一 sandbox 的子目录仍允许。Manager 已知该 Session 属于另一个 workspace 时优先按内存映射拒绝；持久 Session 没有 workspace 元数据时才回退路径校验。

### API

**`SandboxManager`**

| 方法/属性 | 说明 |
| --- | --- |
| `start({ sandboxId?, ...overrides })` | 启动 sandbox 并返回 `SandboxHandle`；同一 `sandboxId` 且显式字段与 handle 创建值一致时复用已有 handle 并再次 `start`，省略字段继承原值；任意显式 `serverName`、`adapterOptions`（稳定 JSON 比较）、`startupTimeoutMs`、`stopTimeoutMs` 不同会抛 `conflict`；`signal` 只作用于本次 start，不参与冲突。新建失败会从 manager 移除；不传 id 时生成新 id，因此每次调用是不同 sandbox。 |
| `get(sandboxId)` | 返回当前未 `close` 的 handle，或 `undefined`。 |
| `list()` | 当前未 `close` 的 handle 快照。 |
| `closeAll({ deleteData? })` | 并发关闭全部 handle；成功的移除，失败的保留并抛 `AggregateError`，可重试。 |
| `size` | 当前跟踪的 handle 数量。 |

Manager 选项：`adapter`（必填，缺失时构造抛 `invalid_options`）、`adapterOptions`（默认值，per-start 整体替换）、`serverName`（默认 `workspace`，不能使用 `web` / `artifact` / `cron` / `workflow`）、`startupTimeoutMs`、`stopTimeoutMs`、`signal`。`signal` 只作为 `start` 的默认取消信号，`closeAll` 不继承。管理器不再接受 `backend`、`moduleSpecifier`、`settingsPath`、`sandboxesRoot`、`periPath`、`periSha256`、`profile`、`profileName`、`port`、`bashCalls`、`minFreeBytes` 等实现细节；这些由 adapter 的 options/start options 定义。

**`SandboxAdapter`**

```ts
interface SandboxAdapter<TOptions = unknown> {
  start(options: { sandboxId: string; signal?: AbortSignal; options?: TOptions }): Promise<SandboxAdapterInstance>;
  deleteData?(sandboxId: string): Promise<void>;
}

interface SandboxAdapterInstance {
  readonly sandboxId: string;
  readonly workspace: {
    id: string;
    hostPath?: string;   // 远程 sandbox 可省略
    runtimePath: string; // Agent/session 看到的根，必须是绝对路径
  };
  readonly mcpServer: { type: "http"; url: string; headers: Record<string, string> }; // token 只在这里
  close(): Promise<void>;
}
```

`peri-sandbox` 的 `createPeriSandboxAdapter()` 返回 `{ id: sandboxId, hostPath: workspacePath, runtimePath: workspacePath }`；本地 sandbox 两个路径相同，未来若 WASM 运行时路径不同，由 adapter 自己决定。Adapter 返回的 `sandboxId` 必须与请求一致，`workspace.runtimePath` 必须是绝对路径，`mcpServer` 必须是 loopback HTTP descriptor；否则 `start()` 失败并关闭已返回的实例。

**`SandboxHandle`**

| 方法/属性 | 说明 |
| --- | --- |
| `sandboxId` / `serverName` | 该 sandbox 的稳定 id 与 MCP server key。 |
| `state` | `idle` / `starting` / `ready` / `stopping` / `stopped` / `failed` / `closed`。 |
| `workspace` | adapter 报告的 `{ id, hostPath?, runtimePath }`（冻结、已归一化）；首次 `start()` 前为 `null`，停止或关闭后仍保留。 |
| `start()` | 启动或复用该实例，返回 `{ sandboxId, serverName, mcpUrl, workspace }`；并发调用共享同一个 Promise，失败可重试，`close()` 后拒绝。 |
| `status()` | 同步快照 `{ sandboxId, state, mcpUrl, workspace, lastError }`；不返回 token。 |
| `mcpServer()` | 仅在 `ready` 时返回 `{ name, config }`；`config` 是 adapter 提供的 loopback HTTP descriptor。非 ready 抛 `not_ready`，`close()` 后抛 `closed`。token 只在这里出现。 |
| `endpoint()` | 当前 endpoint 或 `null`；不返回 token。 |
| `stop()` | 关闭 adapter 实例，保留 handle 与 workspace，可再次 `start()`；不从 manager 移除。 |
| `close({ deleteData? })` | 先调用 `instance.close()`；`deleteData:true` 且 adapter 提供 `deleteData()` 时再删除持久化数据。终止操作幂等；成功后从 manager 移除，之后不能再 `start()`。 |

每次 `start` 可覆盖：`sandboxId`、`adapterOptions`、`serverName`、`startupTimeoutMs`、`stopTimeoutMs`、`signal`；其余值来自 manager。`adapterOptions` 整体替换 manager 默认值。`signal` 只作用于本次调用，不会被 handle 保留；对已有 `sandboxId` 传入不同 `signal` 不构成冲突。

### 生命周期与边界

1. `handle.workspace` 是可移植的 `{ id, hostPath?, runtimePath }`；`hostPath` 是宿主路径，`runtimePath` 是 Agent/session 看到的根，两者不能互相替代。`mcpServer()` 返回的 `config` 是带 Bearer token 的 loopback HTTP descriptor；descriptor 只应留在服务端进程，不要交给浏览器或不可信租户。
2. manager 与 session 解耦：同一个 handle 由调用方决定绑定到哪个会话、何时解除绑定；manager 不保存这种关系，也不提供 session 目录。
3. 需要跨宿主重启复用 workspace 时，调用方应持久化 `sandboxId` 并再次传给 `start`。同一 `sandboxId` 的 workspace 在 `stop()` 和 `close()` 后保留；重新 `start()` 会得到 adapter 的新实例（token、`mcpUrl` 可能变化），已有 `ChatManager` 仍持有旧 descriptor，需要重新建立会话。再次 `start` 时显式传入的创建参数必须与 handle 原值一致，否则以 `conflict` 拒绝。
4. `stop()` 只关闭实例，handle 仍可再次 `start()`；`close()` 成功后才从 `get`/`list` 消失。`closeAll()` 部分失败时，失败 handle 保留以便重试。
5. 生命周期不持久：宿主/实例退出后在途请求、Task 映射与 Bash 预算都会丢失；workspace 文件保留。不要依赖旧实例的 URL 或 token。
6. `SandboxManager` 构造时缺少 `adapter` 抛 `invalid_options`；环境/配置错误、id 冲突、启动或停止失败分别使用稳定的 `SandboxManagerError.code`。Adapter 抛出的稳定 `code`（如 `conflict`、`start_cancelled`、`limit_exceeded`）会被映射。
7. 沙箱隔离由 adapter 实现决定；使用 `peri-sandbox` 时由 `adapterOptions.profile` / `profileName` 控制（出站域名、只读路径、内存/CPU/pids 上限）。SDK 不扩大这些权限，也不把 token 写入日志或状态。
8. 同一 `sandboxId` 的创建参数不可静默改写：显式冲突返回 `conflict`，需先 `close()` 再用新参数创建。`adapterOptions` 按 key 排序的稳定 JSON 比较；不可序列化且不是同一引用时同样按冲突处理。

## 发布入口

| 入口 | 源码 | 内容 |
| --- | --- | --- |
| `@peri-code/sdk` | `src/entries/index.ts` | Chat 控制面、展示契约、config/storage 读取工具 |
| `@peri-code/sdk/view` | `src/entries/view.ts` | 浏览器安全展示出口；需要打包包内 Yjs 依赖 |
| `@peri-code/sdk/wasm` | `src/entries/wasm.ts` | 平台无关的 Emscripten ACP 运行后端、loader 与 transport |
| `@peri-code/sdk/wasm-node` | `src/entries/wasm-node.ts` | `/wasm` 加 Node 兼容的网络/DNS 端口 |
| `@peri-code/sdk/portable` | `src/entries/portable.ts` | 不装配 Bun/stdio 进程或本地 SQLite；Chat 控制面 + 执行准入核心 |
| `@peri-code/sdk/sandbox` | `src/entries/sandbox.ts` | server-side（Node 22.12+ / Bun）；`SandboxAdapter` 契约 + `SandboxManager` 多实例注册表 + `SandboxHandle`；实现由宿主注入，SDK 不加载具体 adapter，session 由调用方组装并在 `createChat` 传入 |

`src/entries/chat.ts` 是主入口与 `/portable` 共用的 Chat 门面，不对应独立包 subpath。`ChatViewRegistry` 只在主入口与 `/portable` 导出，`/view` 保持纯浏览器视图面。

## 源码结构

`src/` 按依赖方向分层，发布入口集中在 `src/entries/`：

| 目录 | 职责 |
| --- | --- |
| `src/contracts/` | ACP、Chat 事实与交互回答等跨层契约；不依赖控制面或视图 |
| `src/payload/` | 工具载荷读取契约与校验 |
| `src/agent/` | 一次激活的 ACP Session、交互、MCP 桥接，以及运行时端口 `PeriInstancePort` |
| `src/workspace/` | 可移植的 `WorkspaceRef` 值对象与纯 JS 路径归一化/越界判断；不依赖 Node/Bun |
| `src/chat/` | Chat 控制面：绑定状态机、动作、ownership、事实日志、生命周期与错误映射 |
| `src/view/` | 事实到 Yjs 文档的投影与浏览器只读副本 |
| `src/state/`、`src/sync/`、`src/transport/` | 对象状态、同步协议、通用 JSON-RPC 与帧端口 |
| `src/wasm/` | Emscripten ACP 运行后端、loader、端口与 `WasmAcpTransport` |
| `src/execution/` | durable execution admission（仅 `/portable` 使用） |
| `src/storage/`、`src/config/`、`src/util/` | 目录读取、启动配置与通用工具 |
| `src/entries/` | 唯一发布入口；`chat.ts` 是主入口与 `/portable` 共用的门面 |

依赖方向：基础层（`contracts`/`payload`/`state`/`sync`/`transport`/`util`/`storage`/`config`/`execution`/`workspace`）不依赖控制面；`chat` 与 `sandbox` 都依赖 `workspace`，但互不依赖；`wasm` 不依赖 `agent`/`chat`/`view`；`agent` 不依赖 `chat`/`view`；`chat` 不依赖 `view`；`view` 不依赖 `chat`/`agent`。只有 `entries` 可以依赖所有层，任何模块都不依赖 `entries`。

## 构建与示例

`MISE_ENV=wasm mise exec -- bun run build` 构建 `peri-wasm` release 产物，并把 `peri-wasm.js` 与 `peri_wasm.wasm` 放入 `dist/wasm/`。构建依赖 Emscripten 6.0.10；`mise exec` 会注入 `EMSDK`，直接 `bun run build` 在仅有 mise shim 的环境会把 shim 路径当作工具链并失败。

默认 [demo.ts](examples/demo/demo.ts) 使用 `runtime.kind="wasm"`；[demo-wasm.ts](examples/demo/demo-wasm.ts) 使用 `runtime.kind="transport"` 注入 `WasmAcpTransport`。两者都通过 `ChatView` + 二进制同步帧驱动浏览器 `ChatReplica`；示例用 SSE 适配器承载逻辑帧，生产消费端使用 WebSocket。

## 发布

发布由 [`.github/workflows/release-sdk.yml`](../../.github/workflows/release-sdk.yml) 完成，不要在本地直接 `npm publish`。

1. 推送 `sdk-v*` tag（例如 `sdk-v0.1.0`）：workflow 会构建 WASM、跑测试、打包并发布到 npm。
2. 手动验证：运行 `Release SDK`，填写版本号并保持 `dry_run=true`；只构建、测试并上传 tarball，不发布。
3. 正式发布需要仓库 secret `NPM_TOKEN`（对 `@peri-code` scope 有发布权限）；含 `-` 的预发布版本发布到 `next`，其余发布到 `latest`。

## 进阶

`/portable` 额外公开可自行驱动的 durable execution admission，SDK Chat 不使用它。`KvExecutionRegistry` 包装 `AtomicExecutionKv` 提供 durable read/CAS apply/resolve；适配器必须实现 `readExecutionValue` 与 `compareAndSetExecutionValue`。`MemoryExecutionKv` 是单进程 `bestEffort` 实现，需要显式 `allowBestEffort`。票据绑定 session、instance/generation、lifecycle/control generation、稳定 work/revision 与 `{turnId, attemptId}`；预算跨实例替换保留，缺失停止证明返回 Blocked，不做 lease/TTL 接管。

Machine/workspace 绑定与 SDK 执行所有权是两件独立职责，不能互相推导。

## 限制

1. SDK 不管理 Peri 或 Workspace 进程；Workspace 文件工具必须由外部 MCP 提供。
2. 没有 TTL、续租或心跳；崩溃清理是路由层责任。
3. 没有共享 KV 与路由亲和时，不保证多实例安全。
4. `store` 在 Manager 构造时校验并深拷贝（`machineId` 非空、`storage` JSON 安全）；`createChat` 必填 `workspace` 与 `agent`，`cwd` 由 `workspace.runtimePath` 派生；含 `storage`/`machineId` 或与 workspace 冲突的 `cwd` 直接报错。
5. `/view` 依赖 Yjs，浏览器使用需要打包；`/portable` 的准入摘要仍用 `node:crypto`。

## 参考

- [Chat 架构设计](../../docs/design/peri-sdk-chat.md)
- [Ownership 模型](OWNERSHIP-MODEL.md)
- [代码索引](../../docs/code-index/peri-ts-sdk.md)
- [peri-cf 服务端装配](../peri-cf/server/agent/compose.ts)
