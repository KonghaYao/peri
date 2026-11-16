# @peri-code/rivet

Rivet actor 适配层：`chat` actor 负责一次会话，可选的 `sandbox` actor 管理工具环境引用，resource 是普通函数加 MCP handler。

从 `@peri-code/rivet/rivet` 的 `createPeriChatActor` 开始，由你的 `setup()` 注册。包内自带 `createEchoChatRuntime()`，不接 LLM、ACP 或 Store 也能跑通 actor 闭环。

```ts
// 签名 · 包面：除 package.json 外的六个公开入口
// @peri-code/rivet            根：ActorError、activity/keep-awake 工具、resource MCP 工厂、ResourceQueries 类型
// @peri-code/rivet/rivet      createPeriChatActor、createSandboxActor、createEchoChatRuntime、createNoopChatDatabase 与端口类型
// @peri-code/rivet/mcp        createResourceMcpServer / createResourceMcpHttpHandler
// @peri-code/rivet/contracts  PeriError、decodePeriError、客户端契约类型
// @peri-code/rivet/client     仅类型：PeriClient 等 M0 契约，没有工厂
// @peri-code/rivet/evidence   EvidenceRecorder
```
代码块首行标注：`可运行` 是完整文件，`片段` 依赖上文变量，`签名` 是类型或清单。根入口不依赖 `rivetkit`；只有 `/rivet` 与你的 registry 装配引用它。

## 1. 入口与定位

```ts
// 签名 · 最短路径
// 1. createPeriChatActor(options)         定义期：端口 + 机器身份 + runtime
// 2. createSandboxActor(options)          可选：sandboxId 的引用计数
// 3. setup({ use: { chat, sandbox } })    注册 actor
// 4. registry.startAndWait()              服务端启动：原地拉起 engine 并等待 envoy 注册
// 5. client.chat.getOrCreate([chatId])    实例期：一个 chat 一个 actor
// 6. chat.open(target)                    调用期：workspace / agent / storage
// 7. execute({ type: "create_new_session" }) 建立 ACP Session
// 8. execute({ type: "send" })            发送输入
```
`chat` actor 只持有一个 `ChatService`，不调用别的 actor；`sandbox` actor 可选，不注册时 chat 照常工作。resource 不注册进 `setup()`，只读 Store，没有需要保活的运行状态。

## 2. 安装（本仓库尚未发布版本）

当前 `@peri-code/rivet` 依赖同仓库的 `@peri-code/sdk` 作为 peer dependency。registry 上的 SDK 与 rivet 版本不配套，所以必须先用 `pack:local` 打出两个 tarball，再在同一条安装命令里一起安装。

### 2.1 构建并打包

SDK 有两条构建路径，先选一条（两个包先各自 `bun install`）：

```bash
# 可运行 · 在仓库根目录
cd npm-packages/@peri-sdk
bun run build:ts        # 仅 TypeScript：echo/transport 路径够用，不需要 Emscripten

# 或者：完整构建，额外产出 dist/wasm/*
bun run build
```

`build:ts` 会生成除 wasm 产物外的全部入口，适合全新 clone：没有 `dist` 时用它就能完成安装与 quickstart。它不生成 `dist/wasm/peri-wasm.js` 与 `dist/wasm/peri_wasm.wasm`，因此也不是完整测试构建：在 `@peri-code/sdk` 里单独执行后跑 `bun test`，`tests/wasm-acp-integration.test.ts` 的 2 个测试会因缺少这两个文件失败；`bun run test` 会先跑完整 `bun run build`。配置 `runtime.kind: "wasm"` 的进程第一次启动时会报：

```text
Cannot find module '.../dist/wasm/peri-wasm.js' imported from .../dist/wasm.js
```

`bun run build` 需要 Emscripten 6.0.10 和 Python ≥3.10（系统 `/usr/bin/python3` 的 3.9.6 不满足）。没有 emsdk 时：

```bash
# 片段 · 固定版本；已安装 emsdk 可跳过 clone
git clone https://github.com/emscripten-core/emsdk.git ~/.emsdk
cd ~/.emsdk
./emsdk install 6.0.10
./emsdk activate 6.0.10
source ./emsdk_env.sh
emcc --version   # 输出应包含 6.0.10
```

`scripts/cargo-wasm.sh` 还会检查 Python 版本；低于 3.10 时直接报 `Emscripten requires Python 3.10 or newer`。

```bash
cd ../peri-rivet
bun run build
bun run pack:local --out /tmp/peri-rivet-pack
```
最后一条命令会打印 tarball 路径和完整安装命令；原样复制执行即可，不要改用最新标签或手写版本范围。

### 2.2 安装

```bash
# 可运行 · 输出以 pack:local 的打印为准；文件名随版本变化
npm install /tmp/peri-rivet-pack/peri-code-sdk-0.0.0.tgz \
  /tmp/peri-rivet-pack/peri-code-rivet-0.1.0.tgz \
  rivetkit@3.1.1
```
顺序固定：SDK 在前，rivet 在后，`rivetkit` 版本显式写死。`@peri-code/sdk` 是 peer，npm 会从同一条命令里的 tarball 解析它，而不是去 registry 找。

npm 11 可能提示 `allow-scripts`（本仓库实测为 `@openuidev/lang-core` 与 `cbor-extract` 的安装脚本）；按提示批准或忽略。echo/transport 路径不依赖 postinstall 产物，engine 二进制来自 optional dependency，不经过这两个脚本。

```bash
# 可运行 · import 探针：先确认包面能加载，再写业务代码
node -e "import('@peri-code/rivet/rivet').then((m) => console.log(typeof m.createEchoChatRuntime))"
# 输出：function
```

engine 二进制还要单独确认，见 [4.1](#41-engine-平台二进制缺失)。

### 2.3 失败模式是有意的

只安装 rivet、不安装 SDK tarball 报 `ETARGET`；改装 registry 版 SDK 报 `ERESOLVE`：

```text
npm error code ETARGET
npm error notarget No matching version found for @peri-code/sdk@^0.0.0.
```

```text
npm error code ERESOLVE
npm error peer @peri-code/sdk@"^0.0.0" from @peri-code/rivet@0.1.0
```
peer 范围是 `^0.0.0`，registry 上的 SDK 只有 0.5.0 等版本；0.5.0 与本仓库不兼容（缺 `./observe` 入口、`createChatSession` 等）。这两处失败都是快速失败，不要用 `--force`、`--legacy-peer-deps` 或最新标签绕过；回到 `pack:local` 的安装命令。

## 3. 五分钟跑通（echo runtime）

下面的完整文件不接 LLM、ACP 进程或真实 Store。`createEchoChatRuntime()` 把每条输入回显成 `echo: <text>`，用来验证 actor 的 open → create_new_session → send → facts → close 闭环。

```js
// 可运行 · chat.mjs
import { setup } from 'rivetkit';
import { createClient } from 'rivetkit/client';
import {
  createEchoChatRuntime,
  createNoopChatDatabase,
  createPeriChatActor,
} from '@peri-code/rivet/rivet';

const ENGINE_PORT = Number(process.env.PERI_ENGINE_PORT ?? '8961');
const ENDPOINT = `http://127.0.0.1:${ENGINE_PORT}`;

// 进程内 ownership 演示；多实例必须换成共享实现。
const records = new Map();
const ownership = {
  getItem: async (key) => records.get(key) ?? null,
  setItem: async (key, value) => {
    records.set(key, value);
  },
  removeItem: async (key) => {
    records.delete(key);
  },
};

// echo runtime 不读 storage；这里只是满足 ChatOpenTarget 的必填字段。
const chat = createPeriChatActor({
  openDatabase: createNoopChatDatabase,
  ownership,
  machineId: '00000000-0000-4000-8000-000000000001',
  chatConfig: { runtime: createEchoChatRuntime() },
});

const registry = setup({
  use: { chat },
  startEngine: true,
  enginePort: ENGINE_PORT,
  // 不启动本地 Services；否则还要安装 @rivet-dev/services。
  startServices: false,
  shutdown: { disableSignalHandlers: true },
});

// startAndWait 等待 envoy 真正注册到 engine；start() 不返回启动结果，失败只能从日志观察。
await registry.startAndWait();

const client = createClient({
  endpoint: ENDPOINT,
  disableMetadataLookup: true,
});

const chatId = 'rt_quickstart_1';
const handle = client.chat.getOrCreate([chatId]);

const opened = await handle.open({
  chatId,
  workspace: { id: 'ws_quickstart', runtimePath: process.cwd() },
  agent: {},
  storage: { url: 'http://127.0.0.1:8091/db/local_default/echo/main' },
});
// binding 为 null 是正常初始状态：open 只建立激活，还没有 ACP Session。
console.log('open', opened.chatId, opened.binding);

// 必须先建 Session，否则 send 会以 not_ready 拒绝。
await handle.execute({
  type: 'create_new_session',
  requestId: 'req_create_1',
  payload: {},
});

const receipt = await handle.execute({
  type: 'send',
  requestId: 'req_send_1',
  payload: { text: 'hello' },
});
console.log('send', receipt.inputId);

await new Promise((resolve) => setTimeout(resolve, 300));

const facts = await handle.facts();
for (const { fact } of facts.envelopes) {
  if (fact.kind !== 'notice') continue;
  const update = fact.notification.params?.update;
  if (update?.content?.type !== 'text') continue;
  console.log(update.sessionUpdate, update.content.text);
}

await handle.close();
await client.dispose();
await registry.shutdown();
```

```bash
# 可运行 · 启动；engine 数据在 .peri-engine/.rivetkit
RIVETKIT_STORAGE_PATH="$PWD/.peri-engine" node chat.mjs
```

预期输出（id 会变化）：

```text
open rt_quickstart_1 null
send 194a7ed5-668f-4ea0-a46d-0dd5e5edda2f
user_message_chunk hello
agent_message_chunk echo: hello
```
看到 `user_message_chunk` 与 `agent_message_chunk` 就说明事实流已经包含用户与助手文本。`close()` 正常返回 `{ closed: true }`；echo runtime 实现了 `session/close`，不会出现 `close_incomplete`。

清理：脚本末尾已 `client.dispose()` 与 `registry.shutdown()`。如果进程被强行终止，确认 `enginePort` 端口已释放，再删除 `.peri-engine` 目录。

### 3.1 echo 不持久化会话或历史

```text
// 片段 · echo runtime 的边界
每个激活一个 transport，队列只在内存
close() 后再 open：binding 为 null，facts 为空
session/load：接受任意 sessionId，但不回放任何通知
刷新后历史仍在：只能由宿主持久化消息投影，或换真实 runtime
```

echo 不跨激活持久化会话或历史。`close()` 之后重新 `open` 会得到空 binding，不会从历史重建；同一次激活内 `send` 的回显也只来自内存队列。要让刷新后的 UI 仍有历史，宿主必须自己保存投影（例如把 `facts()` 投影写入应用数据库），或接入带 Store 的真实 runtime。不要用 echo 验证「重新打开后继续上次对话」。

## 4. 故障排查

### 4.1 engine 平台二进制缺失

`@rivetkit/engine-cli` 只是解析器：真正的 `rivet-engine` 在平台包 `@rivetkit/engine-cli-<platform>` 里，由 optional dependency 安装。npm 镜像缺包、使用 `--omit=optional` 或平台不受支持时，安装阶段不会报错，启动时才失败。先用包内预检确认：

```bash
# 可运行 · 在 npm-packages/peri-rivet 内
bun run check:engine
```

成功时打印 `rivet-engine is available`、二进制 `path`、`version` 与 `source`。失败时打印可复制的平台包安装命令、`RIVET_ENGINE_BINARY_PATH` 覆盖方式，并说明 `RIVET_ENGINE_BINARY` 是同一覆盖的别名。

消费者安装形态下，从应用根目录执行等价探针：

```bash
# 可运行 · npm 默认扁平安装；从 package.json 所在目录执行
node node_modules/@peri-code/rivet/scripts/check-engine.mjs
```

平台包按主机安装，以 macOS Apple Silicon / rivetkit 3.1.1 为例：

```bash
# 可运行 · 与 check:engine 失败时打印的命令一致
npm install --registry=https://registry.npmjs.org --include=optional @rivetkit/engine-cli-darwin-arm64@3.1.1
```
其它平台包名：`darwin-x64`、`linux-x64-musl`、`linux-arm64-musl`。Windows 没有 npm 平台包。

也可以直接指定二进制：

```bash
# 可运行 · 路径必须是绝对路径且文件存在
RIVET_ENGINE_BINARY_PATH=/opt/rivet/bin/rivet-engine node chat.mjs
```
`RIVET_ENGINE_BINARY` 是同一覆盖的别名。错误信息里出现 `engine.binary_unavailable` 时，先查这一项。

### 4.2 端口、数据目录与第二实例

默认 engine 端口是 6420，默认数据目录是 `~/.rivetkit`。同一台机器上跑第二个应用时会争用同一个端口和同一份 engine 配置，后启动的实例会注册失败或连到错误的 engine。本包不提供自动隔离。

```ts
// 片段 · 每个应用自己的 engine 端口与数据目录
const registry = setup({
  use: { chat },
  startEngine: true,
  enginePort: 8961,          // 与 client endpoint 的端口一致
  startServices: false,      // 不需要本地 Services 时显式关闭
  shutdown: { disableSignalHandlers: true },
});
```

```bash
# 片段 · 另一种写法：环境变量
RIVET_RUN_ENGINE_PORT=8961 \
RIVET_RUN_SERVICES=0 \
RIVETKIT_STORAGE_PATH=/srv/app-a \
node app.mjs
```
`RIVETKIT_STORAGE_PATH` 是 engine 的根目录，engine 在其下创建 `.rivetkit/var/...`（配置、日志与数据都在这里）；不设置时根目录是 `~`，因此默认落到 `~/.rivetkit`。容器里也可以通过独立的 `HOME` 隔离。client 的 `endpoint` 必须写成 `http://127.0.0.1:<enginePort>`，否则会连到默认端口。`startServices` 缺省时会在拉起本地 engine 时开启，需要 `@rivet-dev/services`；不装就在 `setup()` 里写 `startServices: false`（或 `RIVET_RUN_SERVICES=0`）。

engine 启动失败时，先看 `<RIVETKIT_STORAGE_PATH>/.rivetkit/var/logs/rivet-engine/` 下的 stderr 日志；日志明确报 `configured log shard capacity ... exceeds ... free` 时，清理旧数据目录或换一个可用空间**严格大于 4294967296 字节**（4 GiB）的存储卷。3.9 GiB 仍然会失败；默认 `max_disk_bytes` 就是 4294967296，engine 不会把它降到可用空间以下。实测原文：

```text
configured log shard capacity of 4294967296 bytes exceeds the 4044738560 bytes free on its filesystem; lower storage.log_shard.max_disk_bytes or storage.cold_store.disk_cache_bytes, unset them to size from free space, or free disk
```

第二实例更简单的做法是复用已经在运行的 engine：第一个进程拥有 registry 与 engine，第二个进程只创建 client，不再调用 `setup()`，也不启动 engine。

```ts
// 片段 · 进程 B：只做客户端，endpoint 指向进程 A 的 engine
import { createClient } from "rivetkit/client";
import type { ChatRegistry } from "./registry";

const client = createClient<ChatRegistry>({
  endpoint: "http://127.0.0.1:8961",
  disableMetadataLookup: true,
});
```
`ChatRegistry` 是 `setup()` 的返回类型；用 `import type` 引入，不会在进程 B 执行 registry 或拉起 engine。两个进程仍共用同一个 engine 与它的数据目录，因此生命周期归进程 A。

### 4.3 `not_ready`

`open` 只构建 SDK 句柄；`binding === null` 表示还没有 ACP Session。此时直接 `send` 会得到 `not_ready`（`Chat has no ready binding`）。按顺序调用 `create_new_session`，或在 `send` 前先读 `facts()` 检查 `binding`。

`create_new_session` 在已经绑定 Session 的句柄上也会返回 `not_ready`；需要换会话时用 `change_session`。

### 4.4 `listen()` 不是启动方式

rivetkit 3.1.1 的服务端启动是 `startAndWait()`（或不等就绪的 `start()`）。不带 `application` 调用 `listen()` 会直接报 `serverless is disabled in this version`；`listen({ application })` 只用于需要自定义 HTTP 入口的部署。本包的 quickstart 不依赖 HTTP 监听。

## 5. 关系图

```text
// 片段 · actor 与 resource 的边界
调用方（BFF / ACP 入口 / 脚本）
        │  rivetkit createClient
        ▼
  engine ──路由──▶ registry（你的 setup）
                     └─ chat [chatId] ──▶ ChatService
                            │ open(target)        │
                            ├─ openDatabase ─────▶ 会话目录（ChatDatabase）
                            └─ ownership ────────▶ Session 归属记录

resource 路径（不经过 engine）：
HTTP ─▶ createResourceMcpHttpHandler ─▶ ResourceQueries.get(scope, id)

sandbox 路径（可选，独立 actor）：
调用方 ─▶ sandbox [sbx, sandboxId] ─▶ SandboxRuntimePort.start / close
```
chat actor 与外层只有两个接触点：创建时的端口、调用时的 action 入参；`setup()` 只注册定义，不实例化 `ChatService`。sandbox actor 与 resource 没有依赖关系，只为工具环境提供引用计数。

## 6. 外层组装

四步：端口类型 → actor 工厂 → registry → 组合根。

```ts
// 可运行 · ports.ts
import type { ChatConfig } from "@peri-code/sdk";
import type { ChatOwnership, OpenChatDatabase } from "@peri-code/rivet/rivet";

export interface ActorPorts {
  openDatabase: OpenChatDatabase;
  ownership: ChatOwnership;
  machineId: string;
  chatConfig: ChatConfig;
}
```

```ts
// 可运行 · chat.ts
import { createPeriChatActor } from "@peri-code/rivet/rivet";
import type { ActorPorts } from "./ports";

export function createChat(ports: ActorPorts) {
  return createPeriChatActor({
    openDatabase: ports.openDatabase,
    ownership: ports.ownership,
    machineId: ports.machineId,
    chatConfig: ports.chatConfig,
  });
}
```
`ChatService` 在 actor 内构造；组合根只传端口，不持有 chat 句柄。

```ts
// 可运行 · registry.ts
import { setup } from "rivetkit";
import { createChat } from "./chat";
import type { ActorPorts } from "./ports";

export function createRegistry(ports: ActorPorts) {
  return setup({
    use: { chat: createChat(ports) },
    startEngine: true,
    enginePort: 8961,
    startServices: false,
    shutdown: { disableSignalHandlers: true },
  });
}

export type ChatRegistry = ReturnType<typeof createRegistry>;
```

```ts
// 可运行 · main.ts：组合根
import { createClient } from "rivetkit/client";
import type { ActorPorts } from "./ports";
import { createRegistry, type ChatRegistry } from "./registry";

export async function startHost(ports: ActorPorts) {
  const registry = createRegistry(ports);
  // 等待 envoy 注册完成；失败会 reject，不会留下“看似启动”的进程。
  await registry.startAndWait();

  const client = createClient<ChatRegistry>({
    endpoint: process.env.RIVET_ENDPOINT ?? "http://127.0.0.1:8961",
    disableMetadataLookup: true,
  });

  return async () => {
    await client.dispose();
    await registry.shutdown();
  };
}
```
关闭顺序：先 `client`（停止发起调用），再 `registry`（停服务）。不要用 HTTP 监听入口启动服务端 actor。

## 7. 配置从哪进

```ts
// 签名 · ChatActorOptions：chat actor 的全部注入点
import type { ActorOptionsInput } from "rivetkit";
import type { ChatConfig } from "@peri-code/sdk";
import type {
  ChatActorDiagnostic,
  ChatOwnership,
  OpenChatDatabase,
  PeriActorLifecycleEvent,
} from "@peri-code/rivet/rivet";

export interface ChatActorOptions {
  readonly openDatabase: OpenChatDatabase;
  readonly ownership: ChatOwnership;
  readonly machineId: string;
  readonly chatConfig: ChatConfig;
  readonly options?: ActorOptionsInput;
  readonly onLifecycle?: (event: PeriActorLifecycleEvent) => void;
  readonly onDiagnostic?: (diagnostic: ChatActorDiagnostic) => void;
}
```
三层固定：定义期是上面七项，实例期是 key，调用期是 action 入参；workspace/agent/storage 只能从 `open(target)` 进。`onLifecycle` 在 wake 成功后与 sleep/destroy 收尾后触发；`onDiagnostic` 接收 `ownership_reset_failed`、`close_failed`、`lifecycle_observer_failed`、`fact_observer_failed`、`activity_source_failed`、`keep_awake_failed`，观察者抛错不影响生命周期。

```ts
// 片段 · target 是每个 chat 自己的配置
const target = {
  chatId,
  workspace: { id: "ws_local", runtimePath: "/srv/peri/workspace" },
  agent: { settings }, // 模型与 Store 凭据只存在于服务端
  storage: { url: "http://127.0.0.1:8091/db/local_default/demo/main" },
};
```
`chatConfig` 给默认值，target 的 `session`/`lifecycle` 覆盖本次 open；`onDiagnostic`/`onLifecycle` 是观测口。`target.chatId` 必须等于 actor key，否则 `invalid_identity`。重复 `open` 的指纹只比较 chat/workspace/agent（含 `sessionPath`），storage 与 lifecycle 不参与；指纹相同返回 `reused: true`，不同返回 `binding_conflict`。

actor 没有「读取当前 target」的 action，`facts()` 也只返回 binding 与事实。`open` 每次都要传完整的 `workspace`/`agent`/`storage`，宿主必须自己保存这三项（例如按 chatId 落库）；不要从 `ChatOpenResult` 或事实流反推。

## 8. 一次请求

```text
// 片段 · open → create_new_session → execute → facts → close
chat.open(target)
  ├─ key 与 target.chatId 不一致               → invalid_identity
  ├─ 同 fingerprint 已打开                     → reused: true
  ├─ 不同 fingerprint 已打开                    → binding_conflict
  └─ 否则 resetOwnership → openDatabase → createChatSession
chat.execute({ type: "create_new_session" })   建立 ACP Session，binding 从 null 变为非 null
chat.execute({ type: "send" })                 入队；未绑定 Session → not_ready
chat.facts()    当前代次全量回放
chat.close()    chat.close() → database.close()
```

```ts
// 片段 · 最短调用链
const opened = await chat.open(target);
if (opened.binding === null) console.log("SDK 尚未绑定 Session");

await chat.execute({ type: "create_new_session", requestId: "req_new_1", payload: {} });

const receipt = await chat.execute({ type: "send", requestId: "req_1", payload: { text: "hi" } });
if (receipt.type === "send") console.log(receipt.inputId);

const facts = await chat.facts();
console.log(facts.envelopes.length, facts.overflow);

await chat.close();
```

`facts()` 不提供游标：每次返回当前代次的全量事实，溢出时 `overflow: true`，客户端必须重新绑定。actor sleep 会关闭 `ChatService` 与目录连接；下次 `open` 重新装配，不恢复执行状态，也不自动回放历史（echo 没有历史）。

`close()` 只释放本次激活：它不删除会话数据，也不注销 actor key。未 open 时 `close()` 是 no-op，返回 `{ closed: true }`。

```text
// 片段 · close 不做的事
close()          关闭 ChatService 与 ChatDatabase，清空本次激活的 binding；不删除 Store 里的 Session
actor key        getOrCreate([chatId]) 的 key 仍可路由；再次 open 会重新装配同一个 actor
删除会话         本包没有删除 action；宿主移除自己的 chatId → sessionId 索引，并按 Store 的能力删除数据
推荐             新会话用新 chatId；不要用 close 冒充 delete
```
actor key 仍存在的影响：如果宿主只删除了自己的索引却复用同一个 `chatId`，旧 Session 仍然留在 Store 中，真实 runtime 可以通过 `change_session` 重新载入；echo 则因为不持久化而始终为空。需要「彻底消失」时换新 `chatId`，并由宿主处理 Store 数据。

### 身份：三个 id 与一个代次

```text
// 签名 · 不要混用
actor key        client.chat.getOrCreate([chatId]) 的路由键；客户端生成，整个 actor 生命周期稳定
ChatOpenResult.chatId      目标里的 chatId 原样返回，等于 actor key
ChatBindingTarget.chatId    SDK 内部 chat 句柄 id（随机 UUID）；出现在 execute 回执与 facts 信封里
sessionId       ACP Session 身份；create_new_session / change_session 可改变
bindingRevision 绑定代次；与 sequence 一起用于事实去重
```
路由、`open.target.chatId`、ownership 键都用 actor key；ACP `sessionId` 只用于会话内的事实分组与切换，不能当 actor key。`SessionReadyResult.chatId` / `facts().binding.chatId` 是 SDK 内部 id，不是路由键。

## 9. Action 签名与参数类型

```ts
// 签名 · 客户端看到的形式（rivetkit 自动去掉 context 参数）
import type { ChatObservation } from "@peri-code/sdk/observe";
import type { ReadPayloadOptions, ToolPayloadReadRef, ToolPayloadResponse } from "@peri-code/sdk";
import type { RuntimeActivity } from "@peri-code/rivet";
import type {
  ChatExecuteResult,
  ChatExecuteTarget,
  ChatFactsResult,
  ChatOpenResult,
  ChatOpenTarget,
} from "@peri-code/rivet/rivet";

declare const chat: {
  open(target: ChatOpenTarget): Promise<ChatOpenResult>;
  execute(raw: unknown, target?: ChatExecuteTarget | null): Promise<ChatExecuteResult>;
  readPayload(ref: ToolPayloadReadRef, options?: ReadPayloadOptions): Promise<ToolPayloadResponse>;
  observe(): ChatObservation | null;
  activity(): RuntimeActivity;
  facts(): ChatFactsResult;
  close(): Promise<{ readonly closed: true }>;
};
```
`ChatActorActions` 是同一份签名（首参是 rivetkit 的 `ActionContext`）；`execute` 的 `send` 直接返回回执，其它类型包 `{ type, result }`。

```ts
// 片段 · execute 回执的 narrowing
const receipt = await chat.execute({
  type: "send",
  requestId: crypto.randomUUID(),
  payload: { text: "hi" },
});

if (receipt.type === "send") {
  console.log(receipt.inputId);              // send：直接是回执
} else if (receipt.type === "create_new_session" || receipt.type === "change_session") {
  console.log(receipt.result.sessionId);     // 会话类：{ type, result }
} else {
  console.log(receipt.type, receipt.result); // cancel / control：{ type, result }
}
```

`execute` 的入参类型来自 SDK 的 `ChatUserAction`。客户端直接标注它，服务端会按类型与字段重新校验：

```ts
// 片段 · ChatUserAction 标注
import type { ChatUserAction } from "@peri-code/sdk";

const action: ChatUserAction = {
  type: "send",
  requestId: crypto.randomUUID(),
  payload: { text: "hello" },
};
await chat.execute(action);
```
可用类型：`create_new_session`、`change_session`、`send`、`force_send`、`take_back`、`load_earlier_history`、`cancel`、`answer`、`set_config_option`。非法字段返回 `invalid_input`。

```ts
// 签名 · open 与 facts 的返回
import type { ChatBindingTarget, ChatFactEnvelope, WorkspaceRef } from "@peri-code/sdk";

export interface ChatOpenResult {
  readonly chatId: string;
  readonly workspace: WorkspaceRef;
  readonly binding: ChatBindingTarget | null;
  readonly reused: boolean;
}

export interface ChatFactsResult {
  readonly binding: ChatBindingTarget | null;
  readonly envelopes: ChatFactEnvelope[];
  readonly overflow: boolean;
}
```

`binding` 为 `null` 表示 SDK 还未绑定 Session，这是 `open` 后的正常初始状态；`observe` 缺失时为 `null`，`activity` 缺失时返回 `complete: false` 的快照。

## 10. 从 facts 构建 UI

### 10.1 消息投影

`facts()` 返回 `ChatFactEnvelope[]`；每条信封带 `chatId`、`sessionId`、`bindingRevision`、`sequence` 与 `fact`。消息文本在 `fact.kind === "notice"` 的 `session/update` 通知里，`update.sessionUpdate` 为 `user_message_chunk` / `agent_message_chunk`；外部输入也可能只以 `fact.kind === "input-delivered"` 到达。

```js
// 片段 · project.mjs：facts → 消息列表；按 inputId/messageId 去重
export function projectMessages(envelopes) {
  const byId = new Map();
  for (const envelope of envelopes) {
    const { fact } = envelope;
    if (fact.kind === "input-delivered") {
      const id = `${envelope.sessionId}:${fact.inputId}`;
      if (!byId.has(id)) byId.set(id, { id, role: "user", text: fact.text });
      continue;
    }
    if (fact.kind !== "notice") continue;
    const notification = fact.notification;
    if (notification.method !== "session/update") continue;
    const update = notification.params?.update;
    const text = update?.content?.type === "text" ? update.content.text : "";
    if (update?.sessionUpdate === "user_message_chunk") {
      // 与 input-delivered 使用同一个 inputId，先到者留下完整文本。
      const id = `${envelope.sessionId}:${update.messageId}`;
      if (!byId.has(id)) byId.set(id, { id, role: "user", text });
      continue;
    }
    if (update?.sessionUpdate === "agent_message_chunk") {
      // assistant 文本可能是增量，按 messageId 追加。
      const id = `${envelope.sessionId}:${update.messageId}`;
      const message = byId.get(id) ?? { id, role: "assistant", text: "" };
      message.text += text;
      byId.set(id, message);
    }
  }
  return [...byId.values()];
}
```

```js
// 片段 · 轮询 fallback：没有事件通道时也能维持 UI
const timer = setInterval(async () => {
  const { binding, envelopes, overflow } = await handle.facts();
  if (overflow) {
    // 回放不完整：丢弃本地投影，重新订阅或重新读取会话。
  }
  render(binding, projectMessages(envelopes));
}, 1000);
```

轮询适合调试与兜底，但 `facts()` 没有游标，每次都是当前代次的全量回放；轮询成本随事实量增长，生产 UI 应先用一次 `facts()` 取初始快照，再接 `chatFact` 增量。实时通道走 actor 的 `chatFact` 事件；它和 `facts()` 给出的是同一批事实，不要另建一套状态机：

```js
// 片段 · 订阅 chatFact；事件只含有增量，初始快照仍取 facts()
const connection = handle.connect();
const detach = connection.on("chatFact", (envelope) => {
  // 用 envelope.bindingRevision 判断是否已换代；sequence 在同一代次内递增。
  applyFact(envelope);
});

// 停止订阅
detach();
await connection.dispose();
```
客户端自己保存 `bindingRevision` / `sequence` 位置；换代时丢弃旧代次的回放。`overflow: true` 时不要拼接，直接重新读取。

### 10.2 会话列表由宿主维护

chat actor 只承载一个会话，不提供“列出全部 chat”的 action。会话发现是宿主端口：

```ts
// 片段 · 宿主侧读取目录；这不是 actor action
const sessions = await openDatabase(storage).listSessions();
// ChatSessionDirectoryEntry: { sessionId, title, cwd, messageCount, createdAt, updatedAt }
```
`listSessions()` 列出 Store 里的 ACP Session，不是 chat actor key。宿主若需要“会话列表 → 某个 chat”的映射，要自己保存 `chatId` 与最近一次 `create_new_session` / `change_session` 回执里的 `sessionId`；不要指望 actor 替你持久化这张表。

### 10.3 最小 BFF：浏览器不直连 actor

actor 的 action 走 Rivet client 协议，浏览器不能直接调用 engine；页面必须经由宿主自己的后端。`@peri-code/rivet/client` 只有类型、没有客户端工厂，`createClient` 来自 `rivetkit/client`。下面的 BFF 连接 §6 的 host（进程 A 保持运行），只新增一个 HTTP 进程。

```ts
// 可运行 · bff.ts：先启动 §6 的 host（保持运行），再 bun bff.ts
import { createServer } from "node:http";
import { createClient } from "rivetkit/client";
import type { ChatFactsResult } from "@peri-code/rivet/rivet";
import type { ChatRegistry } from "./registry";

const client = createClient<ChatRegistry>({
  endpoint: process.env.RIVET_ENDPOINT ?? "http://127.0.0.1:8961",
  disableMetadataLookup: true,
});
const chatId = "bff_demo";
const chat = client.chat.getOrCreate([chatId]);
const target = { chatId, workspace: { id: "ws_bff", runtimePath: process.cwd() }, agent: {}, storage: { url: "http://127.0.0.1:8091/db/local_default/echo/main" } };

type Update = { sessionUpdate: string; content: { type: string; text: string } };
function project(facts: ChatFactsResult) {
  return facts.envelopes.flatMap(({ fact }) => {
    if (fact.kind !== "notice" || fact.notification.method !== "session/update") return [];
    const update = (fact.notification.params as { update?: Update }).update;
    if (update?.content?.type !== "text") return [];
    const role = update.sessionUpdate === "user_message_chunk" ? "user" : update.sessionUpdate === "agent_message_chunk" ? "assistant" : null;
    return role === null ? [] : [{ role, text: update.content.text }];
  });
}

async function send(text: string) {
  await chat.open(target); // 每次 open 都要完整 target；同指纹会 reused
  if ((await chat.facts()).binding === null) {
    await chat.execute({ type: "create_new_session", requestId: crypto.randomUUID(), payload: {} });
  }
  const action = { type: "send", requestId: crypto.randomUUID(), payload: { text } };
  await chat.execute(action);
  for (let i = 0; i < 100; i += 1) {
    const messages = project(await chat.facts());
    if (messages.some((m) => m.text === `echo: ${text}`)) return messages;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error("timeout");
}

const page = `<!doctype html><meta charset="utf-8"><pre id="out"></pre>
<form id="form"><input id="text"><button>send</button></form>
<script>form.onsubmit = async (e) => { e.preventDefault();
  const r = await fetch("/api/messages", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ text: text.value }) });
  out.textContent = JSON.stringify(await r.json(), null, 2); };</script>`;

const status = { invalid_identity: 400, invalid_input: 400, invalid_action: 400, binding_conflict: 409, not_ready: 409, closed: 409, session_creation_failed: 502, close_incomplete: 500 } as Record<string, number>;

const server = createServer(async (req, res) => {
  try {
    if (req.method === "GET" && req.url === "/") return res.writeHead(200, { "content-type": "text/html; charset=utf-8" }).end(page);
    if (req.method === "POST" && req.url === "/api/messages") {
      let raw = "";
      for await (const chunk of req) raw += chunk;
      return res.setHeader("content-type", "application/json").end(JSON.stringify(await send(JSON.parse(raw).text)));
    }
    res.writeHead(404).end();
  } catch (error) {
    const code = (error as { code?: string }).code ?? "internal_error";
    res.writeHead(status[code] ?? 500, { "content-type": "application/json" }).end(JSON.stringify({ code, message: error instanceof Error ? error.message : String(error) }));
  }
});
const bffPort = Number(process.env.BFF_PORT ?? "8962");
server.listen(bffPort, "127.0.0.1", () => console.log(`bff http://127.0.0.1:${bffPort}`));
for (const signal of ["SIGINT", "SIGTERM"] as const) process.once(signal, () => { server.close(); client.dispose().finally(() => process.exit(0)); });
```

```bash
# 片段 · 进程 B；进程 A 用 §6 的 startHost(ports) 启动并保持
RIVET_ENDPOINT=http://127.0.0.1:8961 BFF_PORT=8962 bun bff.ts
curl -sS -X POST http://127.0.0.1:8962/api/messages \
  -H 'content-type: application/json' -d '{"text":"hello"}'
# [{"role":"user","text":"hello"},{"role":"assistant","text":"echo: hello"}]
```
错误从 client 抛出时读 `error.code` 与 `error.message`；HTTP 状态建议映射已写在 `status` 中。需要解析 `metadata` 里的领域错误时，用 `@peri-code/rivet/contracts` 的 `decodePeriError`。不要把 engine 的 `endpoint` 直接交给浏览器，也不要在前端保存 actor 调用凭据；示例未加鉴权、body 上限与限流，生产必须补齐。

## 11. 自定义 runtime 与 transport

`chatConfig.runtime` 有两个分支：`{ kind: "wasm" }` 使用 SDK 自带产物，`{ kind: "transport", factory }` 由宿主提供 ACP transport。完整协议（请求/通知清单、`session/close`、错误码）在 [docs/custom-runtime.md](docs/custom-runtime.md)，这里只给接入口。

```ts
// 片段 · 用 echo runtime 替换真实后端
import type { ChatConfig } from "@peri-code/sdk";
import { createEchoChatRuntime } from "@peri-code/rivet/rivet";

const chatConfig: ChatConfig = { runtime: createEchoChatRuntime() };
```

```ts
// 片段 · 接真实 ACP 后端
const chatConfig: ChatConfig = {
  runtime: {
    kind: "transport",
    factory: (path, options) => createMyAcpTransport(path, options),
  },
};
```
`factory` 每次激活调用一次；`create_new_session` 与 `change_session` 都会新建 transport。`path` 是本次会话 cwd，`options.chatId` 是 SDK 内部 chat 句柄 id。

### close 与 `session/close`

只要本次激活建立过 Session，transport 就必须实现 `session/close`：

- 缺失、拒绝或超过 `lifecycle.closeDrainTimeoutMs`（默认 3000ms）都会让 `chat.close()` 抛 `ChatCloseIncompleteError`，actor 映射为 `close_incomplete`。
- `close_incomplete` 表示关闭未确认；SDK 保留 ownership 与未确认激活，供重新 `open` 时的重载/关闭结算。actor 自身已清空持有的句柄，不能继续调用旧句柄。
- 从未建立过 Session 时，SDK 走启动清理，不会发送 `session/close`。
- `activationCloseTimeoutMs`（默认 5000ms）包住整个激活关闭；两个预算都可以在 `chatConfig.lifecycle` 或 target 的 `lifecycle` 中按字段覆盖。

echo runtime 已实现 `session/close`，所以 quickstart 的 `close()` 会正常返回。换成自定义 transport 时，先按文档实现 `session/close`，再依赖 `close()` 的成功。

## 12. sandbox actor（可选）

`./rivet` 导出 `createSandboxActor`：一个 `sandboxId` 一个 actor，引用计数只存在于 `vars.holders`。它不依赖 chat actor，也不是 resource 的替代；只在宿主提供运行端口时注册。

```ts
// 可运行 · sandbox.ts：端口只做 spawn / close / 只读状态
import { createSandboxActor, type SandboxRuntimePort } from "@peri-code/rivet/rivet";

const running = new Map<string, string>();

const runtime: SandboxRuntimePort = {
  start: async (sandboxId) => {
    running.set(sandboxId, "ready");
    return { sandboxId };
  },
  close: async (sandboxId) => {
    running.delete(sandboxId);
  },
  // 未登记返回 undefined；返回值只用于展示与漂移校正，不参与计数。
  stateOf: (sandboxId) => running.get(sandboxId),
};

export const sandbox = createSandboxActor({
  runtime,
  onDiagnostic: (diagnostic) => console.warn(diagnostic),
});
```

```ts
// 片段 · 与 chat 一起注册；key 必须是 ["sbx", sandboxId]，sandboxId 默认取 key[1]
const registry = setup({ use: { chat, sandbox } });

const sbx = client.sandbox.getOrCreate(["sbx", sandboxId]);
const acquired = await sbx.acquire("chat:rt_1");
if (!acquired.ok) throw new Error(`${acquired.error.code}: ${acquired.error.message}`);

const status = await sbx.status();
if (status.ok) console.log(status.value.refs, status.value.orphaned);

await sbx.release("chat:rt_1"); // 最后一个引用释放时关闭实例
await sbx.closeAll();           // 只关闭本 sandboxId 的实例
```

key 是 sandbox 身份的唯一声明：必须恰好是 `["sbx", sandboxId]`（两段，首段固定为 `"sbx"`）。`sandboxIdFromKey` 只能改写 sandboxId 的取值，不能放宽 key 形状；形状不符时上述 action 返回 `invalid_identity`，不会启动或操作 sandbox。

`acquire` / `release` / `status` / `closeAll` 返回 `SandboxActorResult`：失败是 `PeriErrorPayload`，code 为 `sandbox_unavailable`、`sandbox_start_failed`、`sandbox_close_incomplete`、`invalid_identity` 或 `internal_error`；端口错误原文只进 `onDiagnostic`。

同一 holder 重复 `acquire` 不重复计数；`refs > 0` 时 actor 持有一个 keep-awake，归零释放。`onSleep` / `onDestroy` 在关闭实例前记录 `holders_lost_on_teardown`，然后 best-effort close。`closeAll` 只作用于自身 `sandboxId`，即使 `refs` 为 0 也会调用 close，用来清理 sleep 丢失 holders 后的孤儿；进程级回收由宿主在自己的运行端口上完成。`status.orphaned` 只读地暴露“refs=0 但实例仍在”，读取不产生副作用。

## 13. 错误与 activity

### 13.1 稳定错误码

```text
// 签名 · 客户端按 code 分支，不要解析 message
invalid_identity         actor key 不是 [chatId]、target.chatId 与 key 不一致、id 含空白/控制字符。
                         处理：修正 key 或 target；不要重试同一请求。
invalid_input            actor action 入参不符合边界 schema（含 ZodError）。
                         处理：修正请求形状后重试。
invalid_action           execute 的 ChatUserAction 校验失败（类型/字段/requestId）。
                         处理：按 ChatUserAction 修正后重试，不要原样重放。
binding_conflict         open 的指纹与当前激活不同；execute 携带的 ChatExecuteTarget 代次/session 已过期；
                         或同一 Session 已被其它 chat 持有（SDK ownership 冲突）。
                         处理：重新 open 或重新读 binding 后再发。
closed                   未 open 就调用其它 action（reason: not_open），或句柄已关闭。
                         处理：重新 open；不要复用旧句柄。
close_incomplete         session/close 缺失、拒绝或超时。
                         处理：在 runtime 实现 session/close；SDK 保留资源供重试，继续使用需重新 open。
not_ready                尚未绑定 Session 就 send；在已绑定句柄上 create_new_session；
                         或运行时要求 reload / 清理未完成。
                         处理：先 create_new_session；需要换会话用 change_session；必要时重新 open。
session_creation_failed  create_new_session 时 session/new 失败。
                         处理：检查 transport 的 session/new 与 agent 配置；disposition 为 unknown，不要盲目自动重试。
```
`code` 来自 `UserError`；`metadata` 通常是 `ActorError.toJSON()` / `PeriError.toJSON()`，chat action 错误另带 `{ requestId, disposition }`。`close_incomplete` 的 metadata 是 `{ incomplete: true, errors: [...] }`。SDK 的其它 `ChatActionError` code（如 `busy`）原样透传；未知异常原样抛给 Rivet，对外收敛为 `internal_error`。

```ts
// 可运行 · 客户端解码
import { decodePeriError } from "@peri-code/rivet/contracts";
import { RivetError } from "rivetkit";

export function describeError(error: unknown) {
  if (!RivetError.isRivetError(error)) throw error;
  return { code: error.code, peri: decodePeriError(error.metadata) };
}
```

### 13.2 activity 的语义

`activity()` 返回 `RuntimeActivity`：`revision`、`complete`、`rootRunning`、`activeInternalTasks`、`queuedInputs`、`pendingDeliveries`、`pendingInteractions`、`localOperations`、`closing`。

- `complete` 表示快照是否覆盖全部计数。为 `false` 时只能当作“仍需驻留”，不能据此判断空闲或释放实例。
- `rootRunning` 表示根 turn 正在运行；`queuedInputs` 是已接受但尚未派发的输入数。
- `localOperations` 是正在执行的 action / payload 读取数；`closing` 表示关闭准入已冻结，已接受的工作仍会收尾。
- 本包用这些字段驱动 keep-awake，不向应用承诺“计数为零即可回收”；需要等待静默的调用方用根入口的 `isQuiescent` / `requiresActivationContext` / `canReleaseActivation`，并自行设置超时。
- activity 投影启动失败时返回 `complete: false` 的 idle 快照；不要把它当成空闲信号。

## 14. resource：普通函数 + MCP handler

```ts
// 可运行 · resource.ts
import { parseResourceScope, type ResourceQueries } from "@peri-code/rivet";
import { parseConfigRevision, type ResourceSnapshot } from "@peri-code/rivet/contracts";
import { createResourceMcpHttpHandler } from "@peri-code/rivet/mcp";

const catalog = new Map<string, ResourceSnapshot>();
catalog.set("tenant_a/agent_main", {
  kind: "agent_config",
  revision: parseConfigRevision("rev_1"),
  value: { model: "sonnet" },
  observedAt: new Date().toISOString(),
});

const queries: ResourceQueries = {
  get: async (scope, resourceId) => catalog.get(`${scope}/${resourceId}`) ?? null,
};

export const resourceHandler = createResourceMcpHttpHandler({
  queries,
  resolveScope: (request) => {
    const header = request.headers.get("x-peri-scope");
    return header === null ? null : parseResourceScope(header);
  },
});
```

```ts
// 片段 · 任意 fetch 宿主都能挂；它不进 registry
export default { fetch: (request: Request) => resourceHandler.fetch(request) };
```
`resolveScope` 返回 `null` 得到 401；scope 只来自鉴权，`ResourceQueries` 类型在根入口，MCP 工厂同时从根与 `/mcp` 导出。`resource_get` 找不到目标时返回 `resource_not_found` 工具错误。

```ts
// 签名 · 服务端与 HTTP 两层
import type { ResourceQueries, ResourceScope } from "@peri-code/rivet";
import type { CreateResourceMcpServerOptions, ResourceMcpHttpHandler } from "@peri-code/rivet/mcp";
import type { McpServer } from "@modelcontextprotocol/server";

export function createResourceMcpServer(options: CreateResourceMcpServerOptions): McpServer;
export function createResourceMcpHttpHandler(options: ResourceMcpHttpOptions): ResourceMcpHttpHandler;

export interface ResourceMcpHttpOptions {
  readonly queries: ResourceQueries;
  readonly resolveScope: (request: Request) => ResourceScope | null | Promise<ResourceScope | null>;
  readonly legacy?: "stateless" | "reject";
  readonly keepAliveMs?: number;
  readonly maxRequestBodySize?: number;
  readonly onError?: (error: Error) => void;
}
```
`createResourceMcpHttpHandler` 每个请求重建一个 stateless server，MCP 会话不绑定资源。

### 为什么不是 actor

只读查询没有需要保活的执行状态，wake/sleep 只会增加延迟；`TursoStorage` 自己串行化查询与 `close()`。handler 是 `fetch` 函数，可直接挂在 Bun、Workers 等 fetch 宿主；Node 需自行转换。

## 15. 存储与 SDK 对接

`ChatOpenTarget.storage` 是必填字段。使用自定义 transport（包括 echo）时它只是占位：只有 `openDatabase(target.storage)` 会看到它，echo runtime 不读；quickstart 的 stub 目录因此可以工作。

真实 wasm runtime 需要可用的 Store（Turso / peri-loom）。本包不提供内存适配器，也不在 actor 里建表或迁移：

```ts
// 片段 · TursoStorage 提供三件事
const db = new TursoStorage({ url, ...(authToken === undefined ? {} : { authToken }) });

await db.listSessions(); // 只读会话目录，实现 ChatDatabase
db.storageConfig;        // { url, authToken? } → Peri 写侧启动配置
await db.close();        // 幂等；在途查询完成后才关闭
```

```ts
// 可运行 · 真实存储的 openDatabase 端口
import { TursoStorage } from "@peri-code/sdk";
import type { OpenChatDatabase } from "@peri-code/rivet/rivet";

export const openDatabase: OpenChatDatabase = (storage) => new TursoStorage(storage);
```

```text
// 片段 · open 与关闭的顺序
open(target):
  resetOwnership(ownership, key[0])
  database = openDatabase(target.storage)
  chat = createChatSession({ target, database, ownership, machineId, chatConfig })
close / sleep / destroy:
  chat.close() → database.close()
```
一个实例一条连接，查询与 `close()` 共用串行队列；`createChatSession` 只快照 `target.storage`，不解析 URL，也不关闭 database。构造失败时先 `database.close()` 再抛出；关闭时即使 `chat.close()` 失败，目录仍会关闭。

## 16. 术语

- actor：Rivet 的激活单元，一个 key 一个实例，会被 sleep 和重建；本文的会话 actor 是 `chat`。
- key：`getOrCreate([...])` 的路由键；chat 必须恰好是 `[chatId]`，sandbox 必须恰好是 `["sbx", sandboxId]`（两段、首段固定 `"sbx"`）；形状不符时 action 返回 `invalid_identity`。
- chatId：客户端创建 chat 时生成的稳定身份，也是 actor 的路由键与 ownership 键；与 ACP sessionId 无关。
- sessionId：ACP 会话身份，在 actor 内由 SDK 管理；`change_session` 可切换，因此不能用作 key。
- binding：`ChatBindingTarget`，`{ chatId, sessionId, bindingRevision }`；`binding === null` 是 open 后的正常初始状态。
- 激活：一次 `open` 建出的 `ChatService` 与目录连接，`close` / sleep 时一并释放。
- target：`ChatOpenTarget`，一次 open 的完整身份，决定 workspace、agent 与 storage。
- 代次：`bindingRevision`，SDK 每次重新绑定递增；facts 用它区分旧回放。
- facts：`ChatService` 的事实流；`facts()` 读当前代次回放，`chatFact` 事件推实时。
- 组合根：`main.ts` 一类的启动代码，只创建 client、registry 与端口。
- peri-loom：libSQL 后端，保存会话数据；`storage.url` 指向它。
- MCP handler：resource 的 `fetch` 入口，每请求重建，不参与 actor 生命周期。
- sandbox：可选的工具环境 actor，一个 `sandboxId` 一个实例；只维护 `holders` 引用计数，不持久化。

## 17. 现状

已实现：chat actor；可选 sandbox actor；resource MCP server/handler；EvidenceRecorder；activity ledger 与 keep-awake 桥；echo runtime；`createNoopChatDatabase`；PeriError / ActorError 契约。未实现：`/client` 的运行时实现。

注册、部署、扩缩容、信号处理、多实例协调、Store 运维与前端投影都不在包内；本包只提供 actor 定义、resource 查询与 MCP handler。
