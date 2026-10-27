# @peri-code/sdk

在宿主提供的 ACP transport 上运行 Peri，并给应用一套可读的 Chat 控制面、事实流和只读视图。

SDK 主入口不启动或停止 Peri CLI、Workspace 进程，也不提供本地 SQLite 后端。可选的 `@peri-code/sdk/sandbox` Node 子入口按宿主显式调用管理 `peri-sandbox` 生命周期。宿主负责提供 transport、持久化 Store，并在路由层保证同一 Session 只落在一个实例上。

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

完整例子按 `peri-cf/server/agent/compose.ts` 的装配顺序展开。`storage` 与 `machineId` 是接入时最容易混淆的两个字段，注释里写明了区别。

```ts
import { createStorage } from "unstorage";
import {
  BareHarnessConfig, ChatManager, ChatUserAction, ChatViewRegistry, TursoStorage,
  type PeriConfig, type KvStorage,
} from "@peri-code/sdk";

// 1. Store 连接只声明一次：database 同时提供 SDK 只读目录与 Peri 写侧配置。
//    本地 sqld 也要求非空 authToken；生产从环境变量或密钥管理读取。
const store = {
  url: process.env.PERI_STORAGE_URL ?? "http://127.0.0.1:8081",
  authToken: process.env.PERI_STORAGE_TOKEN ?? "local-dev",
};

// 2. SDK 的会话目录：TursoStorage 直接实现 ChatDatabase（只读）。
//    agentConfig.storage 使用它的 storageConfig 纯 JSON 快照；写入仍由 Peri 完成。
const database = new TursoStorage(store);

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
  agentConfig: {
    cwd: "/workspace",   // Peri 虚拟工作区的绝对路径身份
    machineId,
    storage: database.storageConfig, // 纯 JSON 连接配置；Peri 写侧使用同一 Store
    settings,
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
    sessionInit: {
      path: "/workspace/", // 会话 cwd；peri-cf 用带尾斜杠的 SESSION_CWD
      instructions: "Help with this workspace.",
      mcpServers: {},
    },
  },
});

// 6. 每个 Chat 一个共享 View；同 Chat 的所有同步连接复用它。
const views = new ChatViewRegistry();
const chat = manager.createChat({ path: "/workspace/" });

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
| `agentConfig` | Peri 启动配置：`cwd`、`settings`、`storage`、`machineId` | Peri 运行时 |
| `database` | 会话目录读取：`listSessions`、`getSession` | `ChatManager` |
| `kv` | Session 绑定记录，3 个 KV 方法 | `ChatManager` |
| `chatConfig` | 运行后端、Session 建立输入、生命周期预算 | `ChatManager` |

### `agentConfig`

纯 JSON，构造 `ChatManager` 时序列化快照；不要放函数或循环引用。Peri 启动对象要求四项齐全（SDK 不校验其内部结构）：

| 字段 | 说明 |
| --- | --- |
| `cwd` | Peri 虚拟工作区的绝对路径身份 |
| `settings` | 整份 `PeriConfig`；替换 Peri 的全局配置 |
| `storage` | `{ url, authToken }`，Peri 运行时使用的 Store；可由 `database.storageConfig` 提供 |
| `machineId` | 宿主持久化的 UUID 执行身份 |

`storage` 不是 `database`：

1. `agentConfig.storage` 由 Peri 消费，用来写 Session、消息与运行状态。
2. `database` 由 SDK 消费，只用来读会话目录；SDK 不调用 ACP `session/list`。
3. 两者可以指向同一个 libSQL 端点，但契约不同，不能互相替代。
4. `TursoStorage` 同时提供两者：`database` 传实例，`agentConfig.storage` 传 `database.storageConfig`；默认 database 只构造一次。

`machineId` 不是 Session、Workspace 或 Chat 的标识：

1. 它标识一份可跨实例持续使用的逻辑执行环境，必须由宿主持久化并复用。
2. 同一次部署重启后应保持不变；换值等于换了一个执行环境。
3. 一个 WASM 模块实例只承载一个 `machineId`，运行期不能切换。
4. 本地 demo 的固定 UUID 只用于演示，生产必须从配置或密钥管理读取。

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

`storageConfig` 返回只含 `url` 与 `authToken` 的普通 JSON 对象；`authToken` 未设置时省略该键，也不包含 `legacyCwd`/`currentCwd`。每次读取返回新对象，可直接作为 `agentConfig.storage`。

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
| `sessionInit` | `path`、`instructions`、`mcpServers` |
| `lifecycle` | `closeDrainTimeoutMs`、`startupSettleTimeoutMs`、`activationCloseTimeoutMs`、`shutdownChatTimeoutMs` |

`runtime.kind="wasm"` 时，Manager 为每次激活启动独立 WASM 运行后端，并把 `agentConfig` 序列化为启动配置；可注入 `moduleUrl`、`moduleFactory`、`env` 与 `onDiagnostic`。scheduler 与清理/启动超时由 SDK 内部管理，用户不可配置。`lifecycle` 未显式给出时按 runtime kind 取默认值：wasm 为 activation 25s、shutdown 30s；transport 为 5s、15s。

`runtime.kind="transport"` 时，SDK 不启动 Peri 或 Workspace 进程；`factory` 的 `close()` 成功即视为停止证明，失败保留原因供重试。

## 核心概念

1. **Chat 句柄**：`manager.createChat()` 同步登记，不启动 ACP、不取得 ownership、不读历史。
2. **Session**：Peri 持久化的会话；只有首次绑定动作才会创建或加载。
3. **绑定**：`create_new_session` 使用句柄 path 或 `sessionInit.path`；`change_session` 使用 Store 中的持久 cwd，不回退句柄 path。
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

`chatConfig.sessionInit.mcpServers` 支持 `http`、`stdio` 与 `acp` 声明。`acp` 声明只序列化 `{type:"acp", name, serverId}`，Peri 的反向 `mcp/message` 由 SDK 桥接到官方 server 实例；`serverId` 缺省使用 Record key。

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

chatConfig: {
  sessionInit: {
    mcpServers: {
      "my-tools": {
        type: "acp",
        server: createMyMcpServer,
      },
    },
  },
}
```

1. 官方 `McpServer` 负责 tools/resources/prompts；桥接只依赖实例的 `connect(transport)`，不需要适配层，直接把实例或工厂放进 `type:"acp"` 的 `server`。
2. 不要手动 `connect()`：SDK 在 `session/new|load` 前连接。
   - 工厂实例由激活拥有，结束时由 SDK `close()`。
   - 直接传入的实例由调用方持有，SDK 不会关闭它；不再使用时由调用方自行 `close()`。
3. 直接传实例是单次借用，bridge 关闭后不能安全重连；跨 reload 或多个 Chat 必须用工厂，工厂结果必须有 `close()`（`McpServer` 已提供）。
4. 边界：ACP 只承载 `{serverId, requestId, method, params}`，不重新定义 MCP；sampling/roots 等 server→client 请求返回 `-32601`。

`@peri-code/sdk` 不自带官方 MCP 包，使用方自行安装：v2 用 `bun add @modelcontextprotocol/server`；v1 单包路径为 `@modelcontextprotocol/sdk/server/mcp.js`，两版都可直接传入。

## 沙箱生命周期（可选，Node only）

`@peri-code/sdk/sandbox` 是独立的 Node 子入口，用于在宿主进程内管理一个 `peri-sandbox` 实例。主入口、`/portable`、`/wasm`、`/view` 不加载它，也不会间接加载 `better-sqlite3` 或 `@anthropic-ai/sandbox-runtime`；使用该入口需要 Node >= 22.12。

`peri-sandbox` 是可选 peer 依赖，不使用 `/sandbox` 时无需安装：

```bash
bun add @peri-code/sdk
bun add peri-sandbox   # 仅在使用 /sandbox 时安装
```

> `peri-sandbox` 当前仍在仓库内且标记为 private；发布前请用本地路径或 workspace 链接安装，发布后再使用上面的包名。

**一个 `SandboxManager` 对应一个 chat session。** SDK 不维护全局 session→manager 表；多个会话请创建多个 manager。

```ts
import { SandboxManager } from "@peri-code/sdk/sandbox";

const sandbox = new SandboxManager({
  sessionKey: chatSessionId, // 或 sandboxId；宿主应持久化返回值以便重启后复用 workspace
  // settingsPath / sandboxesRoot / profileName / profile / port 可选
});

const endpoint = await sandbox.start();
const manager = new ChatManager({
  agentConfig: { cwd: endpoint.workspacePath, /* ... */ },
  database,
  kv,
  chatConfig: {
    runtime: { /* ... */ },
    sessionInit: await sandbox.chatSessionInit({
      instructions: "Use the sandbox workspace.",
    }),
  },
});

// 先关闭 Chat 控制面，再关闭沙箱
await manager.shutdown();
await sandbox.close();                    // 停止实例，保留 workspace
await sandbox.close({ deleteData: true }); // 停止并删除 sandbox 目录
```

1. `chatSessionInit()` 返回 `{ path, instructions?, mcpServers }`，可直接作为 `ChatManager.chatConfig.sessionInit`；`path` 是沙箱 workspace 的宿主绝对路径，`mcpServers.workspace` 是带 Bearer token 的 HTTP descriptor。token 只在这里出现，`status()`、`endpoint()` 与错误都不返回它。
2. `start()` 幂等且并发共享同一个 Promise；失败可重试；`close()` 之后拒绝新调用。
3. `stop()` 只关闭当前实例，保留 sandboxId 与 workspace；之后 `start()` 用同一 id 新起实例（新 runId、新 token、可能新 URL）。
4. `close()` 是终止操作且幂等；`close({ deleteData: true })` 删除 `<sandboxesRoot>/<sandboxId>`。`stop()` 后也可调用 `close({deleteData:true})`。
5. 不传 `sandboxId`/`sessionKey` 时每次启动会得到新的随机 workspace；临时会话之外请至少传 `sessionKey`。
6. descriptor 只应留在服务端进程；不要把它交给浏览器或不可信租户。默认 `serverName:"workspace"` 会被 Peri 当作系统 Workspace。
7. 沙箱重启可能改变 `mcpUrl`；已有 `ChatManager` 仍持有旧 descriptor，需要重建 Manager 或重新建立会话后才能切换到新地址。
8. 生命周期不持久：宿主/沙箱实例退出后在途请求、Task 映射与 Bash 预算都会丢失；workspace 文件保留。

## 发布入口

| 入口 | 源码 | 内容 |
| --- | --- | --- |
| `@peri-code/sdk` | `src/entries/index.ts` | Chat 控制面、展示契约、config/storage 读取工具 |
| `@peri-code/sdk/view` | `src/entries/view.ts` | 浏览器安全展示出口；需要打包包内 Yjs 依赖 |
| `@peri-code/sdk/wasm` | `src/entries/wasm.ts` | 平台无关的 Emscripten ACP 运行后端、loader 与 transport |
| `@peri-code/sdk/wasm-node` | `src/entries/wasm-node.ts` | `/wasm` 加 Node 兼容的网络/DNS 端口 |
| `@peri-code/sdk/portable` | `src/entries/portable.ts` | 不装配 Bun/stdio 进程或本地 SQLite；Chat 控制面 + 执行准入核心 |
| `@peri-code/sdk/sandbox` | `src/entries/sandbox.ts` | Node-only；宿主进程内单实例 `peri-sandbox`，生成 HTTP MCP sessionInit |

`src/entries/chat.ts` 是主入口与 `/portable` 共用的 Chat 门面，不对应独立包 subpath。`ChatViewRegistry` 只在主入口与 `/portable` 导出，`/view` 保持纯浏览器视图面。

## 源码结构

`src/` 按依赖方向分层，发布入口集中在 `src/entries/`：

| 目录 | 职责 |
| --- | --- |
| `src/contracts/` | ACP、Chat 事实与交互回答等跨层契约；不依赖控制面或视图 |
| `src/payload/` | 工具载荷读取契约与校验 |
| `src/agent/` | 一次激活的 ACP Session、交互、MCP 桥接，以及运行时端口 `PeriInstancePort` |
| `src/chat/` | Chat 控制面：绑定状态机、动作、ownership、事实日志、生命周期与错误映射 |
| `src/view/` | 事实到 Yjs 文档的投影与浏览器只读副本 |
| `src/state/`、`src/sync/`、`src/transport/` | 对象状态、同步协议、通用 JSON-RPC 与帧端口 |
| `src/wasm/` | Emscripten ACP 运行后端、loader、端口与 `WasmAcpTransport` |
| `src/execution/` | durable execution admission（仅 `/portable` 使用） |
| `src/storage/`、`src/config/`、`src/util/` | 目录读取、启动配置与通用工具 |
| `src/entries/` | 唯一发布入口；`chat.ts` 是主入口与 `/portable` 共用的门面 |

依赖方向：基础层（`contracts`/`payload`/`state`/`sync`/`transport`/`util`/`storage`/`config`/`execution`）不依赖控制面；`wasm` 不依赖 `agent`/`chat`/`view`；`agent` 不依赖 `chat`/`view`；`chat` 不依赖 `view`；`view` 不依赖 `chat`/`agent`。只有 `entries` 可以依赖所有层，任何模块都不依赖 `entries`。

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
4. `agentConfig` 是构造时快照，运行期修改不会生效。
5. `/view` 依赖 Yjs，浏览器使用需要打包；`/portable` 的准入摘要仍用 `node:crypto`。

## 参考

- [Chat 架构设计](../../docs/design/peri-sdk-chat.md)
- [Ownership 模型](OWNERSHIP-MODEL.md)
- [代码索引](../../docs/code-index/peri-ts-sdk.md)
- [peri-cf 服务端装配](../peri-cf/server/agent/compose.ts)
