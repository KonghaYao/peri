# @peri-code/sdk

在宿主提供的 ACP transport 上运行 Peri，并给应用一套可读的 Chat 控制面、事实流和只读视图。

SDK 主入口不启动或停止 Peri CLI、Workspace 进程，也不提供本地 SQLite 后端。可选的 `@peri-code/sdk/sandbox` server-side 子入口按宿主显式注入的 adapter 管理 sandbox 生命周期。宿主负责提供 transport、持久化 Store，并在路由层保证同一 Session 只落在一个实例上。

下面示例是宿主中可直接照抄的装配顺序。

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

按接入顺序展开：Store → Manager → Chat。`store.storage` 与 `machineId` 最容易混淆。

```ts
import { createStorage } from "unstorage";
import {
  BareHarnessConfig, ChatManager, ChatUserAction, ChatViewRegistry, TursoStorage,
  type PeriConfig, type KvStorage,
} from "@peri-code/sdk";

// 1. Store 连接只声明一次；本地 sqld 也要求非空 authToken。
const storeConnection = {
  url: process.env.PERI_STORAGE_URL ?? "http://127.0.0.1:8081",
  authToken: process.env.PERI_STORAGE_TOKEN ?? "local-dev",
};

// 2. SDK 会话目录（只读）；store.storage 复用同一连接配置，写入仍由 Peri 完成。
const database = new TursoStorage(storeConnection);

// 3. Session ownership：只需 getItem/setItem/removeItem；多实例必须换共享 driver。
const kv: KvStorage = createStorage();

// 4. machineId：宿主持久化的逻辑执行环境 UUID，不是 session/workspace id。
const machineId = process.env.PERI_MACHINE_ID ?? "00000000-0000-4000-8000-000000000001";

// 5. 模型与 Store 凭据只存在于服务端进程。
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
    // 按部署开启需要的 harness，这里只保留最小对话能力。
    meta_harness: {
      ...BareHarnessConfig,
      TodoMiddleware: true,
      McpMiddleware: true,
      ToolSearch: true,
    },
  },
};

const manager = new ChatManager({
  // Manager 共享 Store 与机器身份。
  store: {
    storage: database.storageConfig, // 纯 JSON；Peri 写侧使用同一 Store
    machineId,
  },
  database,
  kv,
  chatConfig: {
    runtime: {
      kind: "wasm",
      // peri-wasm.js 位置（string | URL）；推荐 new URL("./peri-wasm.js", import.meta.url)，无可用 import.meta.url（如 workerd）时传绝对 URL 或 moduleFactory。
      moduleUrl: process.env.PERI_WASM_MODULE_URL!,
      // 只把遥测变量交给 WASM；未配置 Langfuse 时删除整个 env。
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
  // workspace = 该 Chat 的边界；hostPath 是宿主路径，runtimePath 是 Agent/Session 的根。
  workspace: {
    id: "ws-local",
    hostPath: "/srv/peri/workspaces/ws-local",
    runtimePath: "/workspace",
  },
  // 可选：相对 runtimePath 的会话 cwd；越界以 workspace_mismatch 拒绝。
  sessionPath: "",
  // per-chat 运行配置；cwd 由 SDK 注入为 workspace.runtimePath。
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

### 必填与可选

| 构造 | 必填 | 可选 |
| --- | --- | --- |
| `ChatManager` | `store`、`database`、`kv`、`chatConfig.runtime` | `chatConfig.session`、`chatConfig.lifecycle` |
| `createChat` | `workspace`、`agent` | `sessionPath`、`session`、`lifecycle`、`runtime`、`payloadBudgets` |

- `store.storage` 是 Peri 写侧 Store，`database` 是 SDK 只读会话目录；`TursoStorage` 同时提供（实例给 `database`，`storageConfig` 给 `store.storage`）。
- `kv` 只需 `getItem`/`setItem`/`removeItem`；`unstorage` 的 `Storage` 直接满足。进程内 memory 只适合单进程，多实例必须换共享 driver 并由路由层串行化。
- `machineId` 是宿主持久化的执行环境身份，不是 session/workspace id；每个 WASM 实例只使用一个 `machineId`。
- per-chat `agent.cwd` 由 `workspace.runtimePath` 派生；`agent` 顶层不能声明 `storage`/`machineId`（由 Manager 注入）；每个 Chat 独立深拷贝。
- `session` 整体覆盖 Manager 默认值，不做字段合并；cwd 越出 `workspace.runtimePath` 以 `workspace_mismatch` 拒绝。
- 生命周期默认 `shutdownChatTimeoutMs`：wasm 30s，transport 15s。

## 核心概念

1. **Chat 句柄**：`createChat` 同步登记，不启动 ACP、不取得 ownership、不读历史；首次 `execute` 才绑定 Session。
2. **Session 与绑定**：新建会话用 `sessionPath` 派生的 cwd，加载历史用 Store 中的持久 cwd；两者都必须位于 `workspace.runtimePath` 下，否则以 `workspace_mismatch` 拒绝。
3. **ownership**：同一 Session 同时至多一个执行所有者；只在清理确认后释放，关闭超时或失败时保留。
4. **事实与 View**：`ChatService` 发布 `ChatFact`；`ChatView` 重放事实并维护只读 Y.Doc。View 不执行动作、不决定权限、不取消任务。

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

`ChatUserAction.send()` 返回 `SendReceipt`（`accepted`、`inputId`）；`create_new_session` 与 `change_session` 返回 `SessionReadyResult`，包含 `chatId`、`sessionId`、`bindingRevision`。

传入普通 wire 对象时先经 `parseChatUserAction` 校验。Manager 不接收 `execute`；执行调度属于 Peri。路由层可用 `chat.binding` 与 `bindingRevision` 拒绝过期连接的动作。

## 展示与同步

```ts
import { ChatReplica } from "@peri-code/sdk/view";

// 浏览器：ChatReplica 收 HELLO/SNAPSHOT/UPDATE/ACK，提供只读 SessionView。
const replica = new ChatReplica();
replica.connect(channel); // channel: ChatYjsChannel
const unsubscribe = replica.subscribe(() => render(replica.read()));
await replica.dispose();
```

`ChatYjsChannel` 只约定帧收发与关闭，不创建物理连接；鉴权与传输由应用负责。`ChatReplica.read()` 返回 `SessionView`，包含 readiness、entries/blocks/tools/turns/tasks/plans、session info、输入队列、交互记录与 `pendingInteractions`。未知块与孤立工具引用显式标记为 `unknown`/`unavailable`，不静默丢弃。

`readSessionView` 与 `readPayload` 从 `/view` 导出。`readPayload(ref, reader, { signal, maxBytes })` 校验 id、UTF-8 字节数与 JSON；`ChatService.readPayload({id, bytes})` 读取当前激活的载荷仓库。默认单次 1 MiB、并发 2，错误码见 `PayloadReadErrorCode`。

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

1. 官方 `McpServer` 负责 tools/resources/prompts；桥接只依赖实例的 `connect(transport)`，把实例或工厂放进 `type:"acp"` 的 `server` 即可，不需要适配层。
2. 不要手动 `connect()`：SDK 在 `session/new|load` 前连接。工厂实例由激活拥有并在结束时 `close()`；直接传入的实例由调用方持有、SDK 不关闭，且 bridge 关闭后不能安全重连，跨 reload 或多个 Chat 必须用工厂。
3. 边界：ACP 只承载 `{serverId, requestId, method, params}`，不重新定义 MCP；sampling/roots 等 server→client 请求返回 `-32601`。

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

> 官方 `peri-sandbox` adapter 目前未公开发布；发布前可自行实现并注入 adapter。

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

### API

**`SandboxManager`**

| 方法/属性 | 说明 |
| --- | --- |
| `start({ sandboxId?, ...overrides })` | 启动 sandbox 并返回 `SandboxHandle`；同一 `sandboxId` 且显式字段与 handle 创建值一致时复用已有 handle 并再次 `start`，省略字段继承原值；任意显式 `serverName`、`adapterOptions`（稳定 JSON 比较）、`startupTimeoutMs`、`stopTimeoutMs` 不同会抛 `conflict`；`signal` 只作用于本次 start，不参与冲突。新建失败会从 manager 移除；不传 id 时生成新 id，因此每次调用是不同 sandbox。 |
| `get(sandboxId)` | 返回当前未 `close` 的 handle，或 `undefined`。 |
| `list()` | 当前未 `close` 的 handle 快照。 |
| `closeAll({ deleteData? })` | 并发关闭全部 handle；成功的移除，失败的保留并抛 `AggregateError`，可重试。 |
| `size` | 当前跟踪的 handle 数量。 |

Manager 选项：`adapter`（必填，缺失时构造抛 `invalid_options`）、`adapterOptions`（默认值，per-start 整体替换）、`serverName`（默认 `workspace`，不能使用 `web` / `artifact` / `cron` / `workflow`）、`startupTimeoutMs`、`stopTimeoutMs`、`signal`。`signal` 只作为 `start` 的默认取消信号，`closeAll` 不继承。adapter 实现细节（`settingsPath`、`sandboxesRoot`、`profile`、`port`、`bashCalls`、`minFreeBytes` 等）由 adapter 的 options/start options 定义。

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

## 公开入口

| 入口 | 内容 |
| --- | --- |
| `@peri-code/sdk` | Chat 控制面、展示契约、config/storage 读取工具 |
| `@peri-code/sdk/view` | 浏览器安全展示出口；需要打包包内 Yjs 依赖 |
| `@peri-code/sdk/wasm` | 平台无关的 WASM ACP 运行后端、loader 与 transport |
| `@peri-code/sdk/wasm-node` | `/wasm` 加 Node 兼容的网络/DNS 端口 |
| `@peri-code/sdk/portable` | 不装配 Bun/stdio 进程或本地 SQLite；Chat 控制面 + 执行准入核心 |
| `@peri-code/sdk/sandbox` | server-side（Node 22.12+ / Bun）；`SandboxAdapter` 契约 + `SandboxManager` 多实例注册表 + `SandboxHandle`；实现由宿主注入，SDK 不加载具体 adapter，session 由调用方组装并在 `createChat` 传入 |
| `@peri-code/sdk/openui` | 轻量门面：围栏切分、主题桥与共享类型 |
| `@peri-code/sdk/openui/renderer` | `OpenUIProgram`；重出口，消费端应懒加载 |
| `@peri-code/sdk/openui/error-boundary` | `OpenUIErrorBoundary`；同步兜底，不牵入 Renderer |
| `@peri-code/sdk/openui/prompt` | 服务端提示词；只读 `spec.json`，不引入 React |
| `@peri-code/sdk/openui/spec.json` | `{ spec, promptOptions }` |

`ChatViewRegistry` 只在主入口与 `/portable` 导出，`/view` 保持纯浏览器视图面。

## OpenUI

下文假设 Streamdown 已安装，并能流式渲染 Markdown；这里只讲 OpenUI 如何接上它。模型默认输出 Markdown，只有结构化界面明显更好时，才在一条回复里额外输出**一个** `openui` 围栏。OpenUI 不接管 Markdown：它只切出围栏、渲染围栏内的程序；把 `renderText` 指给宿主已有的 Markdown 渲染器后，围栏前后的文字与失败回退都走同一个渲染器。已闭合围栏解析、校验或渲染失败时整条消息回退原文；未闭合围栏在流式中显示骨架或上一版界面，用户始终能看到模型输出。

`@peri-code/sdk/openui*` 发布 TS/TSX 源码，需要能编译它的打包器（Vite、Bun），不支持 Node 原生 ESM；`react` / `react-dom` 是可选 peer（≥18）。SDK 不依赖 Streamdown，也不导入它的样式；正文 Markdown 渲染器由宿主注入。

OpenUI 自身只有一步初始化（与正文 Markdown 渲染器无关）：在入口调用一次主题桥，让 canvas 跟随宿主主题。

```tsx
// app entry：CSS 加载后调用一次；不调用则 canvas 使用内置 palette。
import { installOpenUITheme } from "@peri-code/sdk/openui";
installOpenUITheme();
```

### 1. 切分围栏并渲染 OpenUI

`splitOpenUI(text, streaming)` 只做围栏切分，不解析 DSL：`program` 为 `null` 时不挂载 Renderer（普通 Markdown 或多围栏）；流式未闭合时 `status` 为 `open`，`program` 是已到达的片段；闭合后为 `complete`。
示例假定宿主已有 `Markdown({ text, streaming })` 组件：`streaming` 用于补全未闭合的 Markdown 块；`renderMarkdown` 就是它，`before`/`after` 与失败回退都走同一渲染器。

```tsx
// OpenUIMessage.tsx
import { lazy, Suspense, useMemo, type ReactNode } from "react";
import { splitOpenUI, type OpenUIActionEvent } from "@peri-code/sdk/openui";
import { OpenUIErrorBoundary } from "@peri-code/sdk/openui/error-boundary";

// 重量级 chunk：解析器、图表与 canvas 只在出现围栏时加载。
const OpenUIProgram = lazy(() => import("@peri-code/sdk/openui/renderer").then((m) => ({ default: m.OpenUIProgram })));

function Skeleton({ label }: { label: string }) {
  return <div data-slot="peri-openui-skeleton" role="status" aria-live="polite">{label}</div>;
}

export function OpenUIMessage({
  text, streaming, renderMarkdown, onOpenUIAction, actionsEnabled = true,
}: {
  text: string;
  streaming: boolean;
  renderMarkdown: (text: string) => ReactNode;
  onOpenUIAction?: (event: OpenUIActionEvent) => void;
  actionsEnabled?: boolean;
}) {
  const doc = useMemo(() => splitOpenUI(text, streaming), [text, streaming]);

  // 无围栏、多围栏：整条消息按 Markdown 渲染。
  if (doc.program === null) return <>{renderMarkdown(text)}</>;
  // 流结束仍未闭合：半截程序不可解析，回退原文并标记原因。
  if (doc.status === "open" && !streaming) {
    return <div data-openui-fallback="incomplete-fence">{renderMarkdown(text)}</div>;
  }

  const pending = doc.status === "open";
  const before = doc.before ? renderMarkdown(doc.before) : null;
  const skeleton = <Skeleton label={doc.program ? "正在渲染界面…" : "正在生成界面…"} />;
  if (pending && !doc.program) return <>{before}{skeleton}</>;

  return (
    <OpenUIErrorBoundary
      resetKey={doc.program}
      fallback={pending ? skeleton : renderMarkdown(text)}
    >
      <Suspense fallback={<>{before}{skeleton}{doc.after ? renderMarkdown(doc.after) : null}</>}>
        <OpenUIProgram
          program={doc.program}
          isStreaming={pending}
          actionsEnabled={actionsEnabled}
          before={doc.before}
          after={pending ? "" : doc.after}
          fallbackText={text}
          renderText={renderMarkdown}
          onAction={onOpenUIAction}
        />
      </Suspense>
    </OpenUIErrorBoundary>
  );
}
```

### 2. 按钮动作

`SessionView` 的 assistant 条目给出 text block；`streaming` 由宿主按当前 turn 是否完成决定。把消息交给 `OpenUIMessage`，并把 action 路由到发送或开链接（`send` 即宿主已有的发送函数，最终落到 `ChatUserAction.send`）：

```tsx
import type { OpenUIActionEvent } from "@peri-code/sdk/openui";

// 在消息列表里这样接；OpenUIMessage 来自步骤 1，Markdown 是宿主已有组件，text/streaming 来自 SessionView，busy/send 由宿主提供。
<OpenUIMessage
  text={text}
  streaming={streaming}
  renderMarkdown={(value) => <Markdown text={value} streaming={streaming} />}
  onOpenUIAction={(event) => handleOpenUIAction(event, send)}
  actionsEnabled={!busy && !streaming}
/>

// 只放行无 userinfo、长度 ≤2048 的 http(s)。
function safeHttpUrl(value: unknown): string | undefined {
  if (typeof value !== "string" || value.length > 2048) return undefined;
  try {
    const url = new URL(value.trim());
    if (url.protocol !== "http:" && url.protocol !== "https:") return undefined;
    if (url.username || url.password) return undefined;
    const normalized = url.toString();
    return normalized.length <= 2048 ? normalized : undefined;
  } catch {
    return undefined;
  }
}

function handleOpenUIAction(event: OpenUIActionEvent, send: (text: string) => void) {
  if (event.type === "continue_conversation") {
    // 空消息回退 params.message；消息截断 8k，表单摘要超过 2k 时省略。
    const raw = event.humanFriendlyMessage.trim() || (typeof event.params.message === "string" ? event.params.message.trim() : "");
    if (!raw) return;
    // formState 字段形如 {value, componentType}；只保留顶层 primitive。
    const form = JSON.stringify(Object.fromEntries(Object.entries(event.formState ?? {})
      .filter(([key]) => key !== "componentType" && !key.startsWith("$"))
      .map(([key, field]) => [key, field && typeof field === "object" && "value" in field ? (field as { value: unknown }).value : field] as const)
      .filter(([, value]) => typeof value === "string" || typeof value === "boolean" || (typeof value === "number" && Number.isFinite(value)))));
    const formText = form === "{}" ? "" : form.length <= 2000 ? `\n\n表单数据：${form}` : "\n\n（表单数据过长，已省略）";
    send(`${raw.slice(0, 8000)}${raw.length > 8000 ? "\n\n（消息过长，已截断）" : ""}${formText}`);
    return;
  }
  if (event.type === "open_url") {
    const url = safeHttpUrl(event.params.url);
    if (url) window.open(url, "_blank", "noopener,noreferrer");
  }
}
```

### 3. 服务端提示词

```tsx
import { generateOpenUISystemPrompt } from "@peri-code/sdk/openui/prompt";

// baseSystemPrompt 是宿主已有的系统提示词。
const instructions = [baseSystemPrompt, generateOpenUISystemPrompt()]
  .filter(Boolean)
  .join("\n\n");
```

把 `instructions` 传给 `createChat({ session: { instructions } })`。提示词只读 `spec.json`，不引入 React，也不含云端配置；它在 `session/new` 时冻结，已有会话需要新建才会带上 OpenUI 段。

### 4. 模型输出长什么样

````text
本周交付如下。

```openui
root = Stack([title, metrics, next])
title = H2("本周交付")
metrics = Grid(2, [done, blocked])
done = Stat("12", "已完成", "success")
blocked = Stat("3", "阻塞", "danger")
next = Button("展开阻塞项", @ToAssistant("展开阻塞项并给出处理建议"))
```

需要我展开阻塞项吗？
````

1. 每行 `name = Expression`，必须定义 `root = Stack([...])`；参数按位置传，可选参数只能从末尾省略。
2. 一条回复最多一个顶层围栏；普通代码块、列表项或引用块里的 `openui` 不算。
3. 动作按钮用 `@ToAssistant("...")` 或 `@OpenUrl("...")`。

### 边界与限制

- `OpenUIProgram` 必须懒加载；主 chunk 不应引入解析器、图表与 canvas。
- 未闭合围栏在流式中：解析失败或暂无可渲染程序时显示骨架；已有可渲染程序时保留上一版，渲染异常回退骨架，不使用 `fallbackText`。
- 未闭合围栏在流结束后，或已闭合围栏（含裸 DSL）解析、校验、引用解析或渲染失败时：整条消息回退原文（`renderText(fallbackText)`；宿主传 Markdown 渲染器时保持 Markdown，缺省纯文本）。
- 流式解析按 120 ms 节流：首个片段立即解析，窗口内的更新合并为最后一次；流式期间不跑严格校验，非法值可能短暂空白。
- 表单值不持久化；按钮的校验、发送、消息截断（8k）与表单摘要上限（2k）由宿主负责。聊天忙或流式时传 `actionsEnabled={false}`。
- 主题：`installOpenUITheme()`（`@peri-code/sdk/openui`）从 `--chat-code-bg` / `--chat-fg` / `--chat-link` 读取 hex 并推给 canvas；`oklch()` 等非 hex 值会被忽略并沿用内置 palette；不调用同样用内置 palette。
- 组件与参数的公开契约是 `@peri-code/sdk/openui/spec.json`（`{ spec, promptOptions }`）；`generateOpenUISystemPrompt()` 已把同一份 spec 交给模型。常用组件：`Stack`、`Row`、`Grid`、`Card`、`Table`、`Stat`、`BarChart`/`LineChart`/`PieChart`、表单控件与 `Button`。

## 进阶

`/portable` 额外公开可自行驱动的 durable execution admission，SDK Chat 不使用它；`KvExecutionRegistry` 包装 `AtomicExecutionKv` 提供 durable read/CAS apply/resolve。票据绑定 session、instance/generation、lifecycle/control generation 与 `{turnId, attemptId}`，预算跨实例替换保留。Machine/workspace 绑定与 SDK 执行所有权是两件独立职责。

## 限制

1. SDK 不管理 Peri 或 Workspace 进程；Workspace 文件工具必须由外部 MCP 提供。
2. 没有 TTL、续租或心跳；崩溃遗留记录由路由层在旧实例停止后调用 `clearSessionOwnership(storage, sessionId)` 清理。
3. 没有共享 KV 与路由亲和时，不保证多实例安全。
4. `store` 在 Manager 构造时校验并深拷贝（`machineId` 非空、`storage` JSON 安全）；`createChat` 必填 `workspace` 与 `agent`，`cwd` 由 `workspace.runtimePath` 派生；含 `storage`/`machineId` 或与 workspace 冲突的 `cwd` 直接报错。
5. `/view` 依赖 Yjs，浏览器使用需要打包；`/portable` 的准入摘要仍用 `node:crypto`。
6. 错误码：加载路径 ownership 冲突为 409 `binding_conflict`；加载路径 KV 故障为 500 `claim_failed`；新会话 claim 失败为 500 `session_creation_failed`。

## 示例

- [demo.ts](https://github.com/KonghaYao/peri/blob/main/npm-packages/@peri-sdk/examples/demo/demo.ts) 使用 `runtime.kind="wasm"`。
- [demo-wasm.ts](https://github.com/KonghaYao/peri/blob/main/npm-packages/@peri-sdk/examples/demo/demo-wasm.ts) 使用 `runtime.kind="transport"` 注入 `WasmAcpTransport`。

两者都通过 `ChatView` + 二进制同步帧驱动浏览器 `ChatReplica`；示例用 SSE 适配器承载逻辑帧，生产消费端使用 WebSocket。
