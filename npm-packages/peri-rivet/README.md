# @peri-code/rivet

> 本地交付（不发布）：`pack:local` 从当前 TypeScript 源码生成 SDK、rivet 与 peri-sandbox 三个 tarball；SDK 的 WASM 只复制仓库已有的 `dist/wasm` 产物，兼容性未重建验证。`WorkspaceURI`、Chat 的 URI 入口、SDK 的 `ChatState`、显式宿主资源 owner、`createSandboxMcpActor` 与真实 `peri-sandbox` adapter 均已实现。URI 是 workspace 的唯一身份，边界为字符串。
>
> `createPeriChatActor` 与 `createSandboxMcpActor` 都必须由组合根显式传入 `createHostResourceOwner()` 创建的 owner，并强持有到进程结束；包内没有隐式全局兜底。Rivet 清理 activation vars 后，未确认关闭的 SDK、database、gate 与 runtime 记录仍归宿主，可由 `owner.closeAll()` 枚举并重试。

Rivet actor 适配层：`chat` actor 承载一次会话，独立的 Sandbox MCP 服务按 workspace 管理环境并提供 MCP HTTP；`examples/chat.mjs` 是真实 wasm runtime 的多轮 REPL，`examples/sandbox-mcp.mjs` 是真实 `peri-sandbox` 宿主适配的独立服务。包尚未发布，以下流程只用本地 tarball。

## 安装与运行（本地 tarball）

在仓库内生成三个 tarball。脚本在隔离 staging 目录重新生成 SDK 的 JS 与 declarations，只复制已存在且核验过 magic/snippet 的 WASM glue、`peri_wasm.wasm` 与 snippets；不会删除或覆盖现有 `dist`，也不会调用 Emscripten。

```bash
cd npm-packages/peri-rivet
bun run pack:local --out /tmp/peri-rivet-pack
```

`dist/wasm` 缺失时，先在有 Emscripten 的主机于 `npm-packages/@peri-sdk` 执行 `bun run build`；本次交付未重建 WASM，`build-provenance.json` 会标记兼容性为 unverified。

```bash
# Chat + Sandbox：同一输出目录的三个 tarball 一起装
npm install /tmp/peri-rivet-pack/peri-code-sdk-0.0.0.tgz \
  /tmp/peri-rivet-pack/peri-code-rivet-0.1.0.tgz \
  /tmp/peri-rivet-pack/peri-sandbox-0.1.0.tgz rivetkit@3.1.1

# 纯 Chat：不安装 peri-sandbox
npm install /tmp/peri-rivet-pack/peri-code-sdk-0.0.0.tgz \
  /tmp/peri-rivet-pack/peri-code-rivet-0.1.0.tgz rivetkit@3.1.1

# 项目目录；engine 平台包缺失时按打印命令安装
node node_modules/@peri-code/rivet/scripts/check-engine.mjs
```

## Chat

保存为项目目录下的 `one-shot.ts`。先运行后文的独立 Sandbox MCP 服务（`examples/sandbox-mcp.mjs` 或按后文自建），取得不含凭据的 `SANDBOX_MCP_URL`，再把 URL 与 `SANDBOX_MCP_TOKEN` 传入 Chat 进程。包内 `examples/chat.mjs` 是可直接运行的多轮 REPL。

```bash
# 项目目录；占位值换成真实值，不要把 key 写进仓库
export ANTHROPIC_AUTH_TOKEN='<token>'      # 或 ANTHROPIC_API_KEY
export ANTHROPIC_BASE_URL='<https://...>'
export PERI_STORAGE_URL='<http://...>'
export PERI_STORAGE_TOKEN='<project_api_key>'
export SANDBOX_MCP_URL='<sandbox_mcp_url>'     # 后文 Sandbox MCP 服务打印，不含凭据
export SANDBOX_MCP_TOKEN='<actor_token>'       # 服务与 Chat 共享的 actor 凭据，不进 URL
export SANDBOX_MCP_GATEWAY_TOKEN='default'     # 本地 spawn engine 的 gateway token；远程按其配置
export RIVETKIT_STORAGE_PATH="$PWD/.rivetkit"  # engine 状态目录；相对当前执行目录，不设置会落到 ~/.rivetkit
```

```ts
import { BareHarnessConfig, ChatState, ChatUserAction, TursoStorage } from '@peri-code/sdk';
import {
  createHostResourceOwner,
  createMemoryChatOwnership,
  createPeriChatActor,
  WorkspaceURI,
} from '@peri-code/rivet/rivet';
import { setup } from 'rivetkit';
import { createClient } from 'rivetkit/client';

const model = 'deepseek-v4-flash';
const chatId = `rt_chat_${crypto.randomUUID()}`;
// 部署身份，由运行本 actor 的宿主提供；请求里的 URI 必须与它同机。
const machineId = '00000000-0000-4000-8000-000000000001';

// 宿主强持有；Rivet 清掉 activation vars 后，未确认关闭仍归这里。
const owner = createHostResourceOwner();
const chat = createPeriChatActor({
  owner,
  openDatabase: (target) => new TursoStorage(target),
  ownership: createMemoryChatOwnership(),
  machineId,
  chatConfig: { runtime: { kind: 'wasm' } },
});

const registry = setup({
  use: { chat },
  startEngine: true,
  enginePort: 8961,
  startServices: false,
  shutdown: { disableSignalHandlers: true },
});
await registry.startAndWait();

const client = createClient({
  endpoint: 'http://127.0.0.1:8961',
  disableMetadataLookup: true,
});
const handle = client.chat.getOrCreate([chatId]);
const connection = handle.connect();

let unsubscribe: (() => void) | undefined;
let detachFacts: (() => void) | undefined;
let finishTurn: (() => void) | undefined;
let turnStarted = false;

try {
  await connection.ready;
  const opened = await handle.open({
    chatId,
    workspace: new WorkspaceURI({
      machineId,
      runtimePath: '/Users/me/project',
    }).toString(),
    // settings 是 PeriConfig，只包一层；WASM 从 settings.config 读 provider。
    agent: {
      settings: {
        config: {
          active_alias: 'sonnet',
          providers: [{
            id: 'anthropic',
            type: 'anthropic',
            apiKey: process.env.ANTHROPIC_AUTH_TOKEN ?? process.env.ANTHROPIC_API_KEY,
            baseUrl: process.env.ANTHROPIC_BASE_URL!,
            name: 'anthropic',
            models: { fable: model, opus: model, sonnet: model, haiku: model },
          }],
          profiles: {
            fable: { provider: 'anthropic', model },
            opus: { provider: 'anthropic', model },
            sonnet: { provider: 'anthropic', model },
            haiku: { provider: 'anthropic', model },
          },
          meta_harness: {
            ...BareHarnessConfig,
            McpMiddleware: true,
            ToolSearch: true,
            WorkspaceMiddleware: false,
          },
        },
      },
    },
    session: {
      instructions: '你是 Peri。请用清晰、简洁的中文回答。',
      // Agent 建立会话时要连接的 MCP；http/stdio/acp 三种描述见 @peri-code/sdk。
      mcpServers: {
        sandbox: {
          type: 'http',
          url: process.env.SANDBOX_MCP_URL!,
          headers: {
            'x-rivet-token': process.env.SANDBOX_MCP_GATEWAY_TOKEN ?? 'default',
            'x-peri-sandbox-token': process.env.SANDBOX_MCP_TOKEN!,
          },
        },
      },
    },
    storage: {
      url: process.env.PERI_STORAGE_URL!,
      authToken: process.env.PERI_STORAGE_TOKEN!,
    },
  });
  await handle.execute(ChatUserAction.createNewSession({ requestId: `${chatId}:create` }));
  console.log(`open ${opened.chatId}`);

  const state = ChatState.empty();
  unsubscribe = state.subscribe((snapshot) => {
    console.log(snapshot.messages.map(({ role, text }) => `${role}> ${text}`).join('\n'));
    if (snapshot.running) turnStarted = true;
    else if (turnStarted) {
      turnStarted = false;
      finishTurn?.();
      finishTurn = undefined;
    }
  });
  state.apply(await handle.facts());
  detachFacts = connection.on('chatFact', (message) => state.apply(message));

  const done = new Promise<void>((resolve) => {
    finishTurn = resolve;
  });
  await handle.execute(ChatUserAction.send({ text: '用一句话解释 ACP。', requestId: `${chatId}:1` }));
  await done;
  console.log('turn complete');
} finally {
  unsubscribe?.();
  detachFacts?.();
  // 先关闭新执行准入；closeAll 处理仍由宿主拥有的未确认资源。
  owner.beginShutdown();
  await handle.close().catch(() => undefined);
  await connection.dispose().catch(() => undefined);
  await client.dispose().catch(() => undefined);
  const report = await owner.closeAll({ timeoutMs: 5_000 });
  if (!report.complete) console.error(`host resources incomplete: ${report.pending}`);
  // registry 收尾独立执行，不能因上一项失败被跳过。
  await registry.shutdown();
}
```

`one-shot.ts` 的 `open.workspace` 应填 Sandbox 服务打印的 `SANDBOX_MCP_WORKSPACE`（同一 canonical URI）；代码里的 `/Users/me/project` 只是占位。

`runtime` 与 `machineId` 是部署共用的引擎/机器接入；每个 Chat 的模型/provider 由 `agent` 提供，`instructions`/MCP 由每次 `open` 的 `session` 提供。`open.workspace` 的 URI 必须与部署 `machineId` 同机；`workspace` 本身不是 per-Chat 的两个字段，而是 canonical URI 字符串。`meta_harness.McpMiddleware: true` 只是启用 MCP 能力；Agent 实际连接哪些 server 由该 `session.mcpServers` 声明。Chat 侧只接收 Sandbox 的 HTTP 地址；`x-rivet-token` 过 gateway，`x-peri-sandbox-token` 由 Sandbox actor 校验，两者都不进 URL。

`createPeriChatActor` 是部署级静态工厂：`owner`、`openDatabase`、`ownership`、`machineId` 与默认 `chatConfig` 在创建时固定；每个 Chat 的 `open` 再提供自己的 `workspace`/`agent`/`storage`，`session`、`lifecycle` 覆盖默认值。`TursoStorage` 只实现宿主的 `openDatabase` 端口：actor 不解析 Store URL、不建表、不迁移，连接与关闭都归宿主；未确认关闭由显式 `owner` 持有。

## ChatState

`ChatState` 是 SDK 的会话状态容器：`empty()` 给出合法初值，`apply` 原地吸收快照并返回 `void`，`subscribe` 提供只读快照并返回退订函数。输入结构非法时抛 SDK 的 `ChatStateInputError`；重复、迟到或旧代次事实不会改写当前状态。接上一节的 `handle` 与 `connection`：

```ts
import { ChatState } from '@peri-code/sdk';

const state = ChatState.empty();
const unsubscribe = state.subscribe((snapshot) => console.log(snapshot.messages));
state.apply(await handle.facts());
const detachFacts = connection.on('chatFact', (message) => state.apply(message));

// 结束时解除订阅与监听
detachFacts();
unsubscribe();
```

`facts()` 给出整份快照，`chatFact` 投递原始消息，两者都直接交给 `apply`。快照字段来自 SDK 的 `SessionView`，另带 `messages`（`{ entryId, role, text }`）；对象被冻结，外部不能改写内部状态。内容更新按微任务合并，但增量里的 `running` 迁移不会被吞掉：极速 turn 仍会先看到 `running: true`，再看到终态。

`open.workspace` 只接受 `WorkspaceURI.toString()` 的 canonical 字符串；`createPeriChatActor` 的 `machineId` 是部署身份，URI 属于其它机器时 `open` 会在触碰 ownership、数据库与 runtime 之前以 `invalid_identity` 拒绝。`open` 的复用指纹覆盖 workspace、agent、storage、session、sessionPath 与 lifecycle：配置有任何变化都会以 `binding_conflict` 拒绝，不会悄悄沿用旧激活。同一 activation 的 `open`/`close`/sleep/destroy 串行执行：并发同目标 `open` 只构造一次激活；不同目标明确 `binding_conflict`。显式 `close` 与 `onSleep`/`onDestroy` 共用宿主 owner 的关闭路径，失败时保留记录与已确认进度并显式失败（SDK 关闭未确认映射为 `close_incomplete`），重试或下一次 `open` 会先完成关闭；只有全部资源确认关闭后才清空 vars。SDK 关闭失败时不会继续关闭可能仍被它使用的 database。RivetKit 3.1.1 在 hook 的 `finally` 中清理 activation vars，但未确认的 SDK/database 已归显式宿主 owner：同 key 的新 activation 必须先确认旧 lease 关闭，旧关闭不完整会阻止 `resetOwnership`/`openDatabase`；进程停机用 `owner.beginShutdown()` 关闭准入，再 `owner.closeAll({ timeoutMs, maxAttempts })` 收尾（默认最多 2 轮，只重试明确可重试的 incomplete，未知错误立即暴露）。

## Sandbox MCP

`createSandboxMcpActor({ owner, runtime, token, machineId, authorizedWorkspaces })` 用一个 actor 同时管理 workspace 环境的引用计数与生命周期，并在自身的 `/mcp` 上提供 MCP HTTP。`owner` 由组合根创建并强持有；每个 activation 的 gate 与关闭 closure 存在 owner 记录里，vars 丢失后仍可收尾。actor key 恰好是 `['sbx', workspaceUri]`；`workspaceUri` 直接是 `WorkspaceURI.toString()` 的 canonical 字符串，不哈希、不映射、不拼 root。`runtime` 是宿主实现：管理环境生命周期，并把 `/mcp` 请求交给环境内的 MCP（`fetchMcp(workspaceUri, request)`）。

真实宿主适配器在 `@peri-code/rivet/sandbox`：`createPeriSandboxMcpRuntime({ machineId, workspaces, settingsPath, sandboxesRoot })` 复用 `peri-sandbox` 的真实环境与内部 MCP。每个 grant 同时给出 canonical URI 与 host 认可的真实目录；工厂只做逐字比较：URI 的 machineId 必须等于部署 `machineId`，URI 路径必须与授权的真实目录完全相同（不接受 symlink/realpath 别名），目录必须是已存在的 0700 真实目录，且不能与 `sandboxesRoot` 互相包含。适配器不把 URI 哈希、截断或编码成 `sessionKey`/`sandboxId`；`peri-sandbox` 内部的 `sbx_...` 只是进程/状态目录句柄，仅用于资源控制，不参与 workspace 查找、授权或恢复。显式 workspace 的托管状态随实例关闭删除，授权目录本身不动。授权配置只来自部署组合根，不来自请求；URI/key 不是授权。

宿主保存为 `sandbox-mcp.ts` 单独运行。服务 acquire 成功（环境与 `/mcp` 就绪）后才打印不含凭据的 URL，并一直持有 holder 到停机；收到信号后先 release，再 `beginShutdown()`/`owner.closeAll()`，只有 owner 报告 `complete` 才调用适配器的 `runtime.closeAll()`，且只有 owner 与适配器关闭都确认后才删除临时 state 目录，最后关闭 client 与 registry。任何一步未确认都设置非零退出码，client 与 registry 的收尾始终执行；registry 返回不代表资源已确认。先停所有 Chat 使用者，再停这个服务。`examples/sandbox-mcp.mjs` 是可直接运行的版本（临时授权 workspace、临时 settings、`127.0.0.1`、固定 machineId）。

```ts
// sandbox-mcp.ts · 独立进程；run with bun after building this package
import { createHostResourceOwner, createSandboxMcpActor, WorkspaceURI } from '@peri-code/rivet/rivet';
import { createPeriSandboxMcpRuntime } from '@peri-code/rivet/sandbox';
import { setup } from 'rivetkit';
import { createClient } from 'rivetkit/client';

const token = process.env.SANDBOX_MCP_TOKEN!;
const machineId = '00000000-0000-4000-8000-000000000001';
const workspace = new WorkspaceURI({
  machineId,
  runtimePath: '/Users/me/project',
}).toString();
const holder = 'sandbox-mcp:host';

const runtime = createPeriSandboxMcpRuntime({
  machineId,
  workspaces: [{ uri: workspace, path: '/Users/me/project' }],
  settingsPath: '/Users/me/.peri-sandbox/settings.json',
  sandboxesRoot: '/Users/me/.peri-sandbox/sandboxes',
});
const owner = createHostResourceOwner();
const sandbox = createSandboxMcpActor({
  owner,
  runtime,
  token,
  machineId,
  authorizedWorkspaces: [workspace],
});

const registry = setup({
  use: { sandbox },
  startEngine: true,
  engineHost: '127.0.0.1',
  enginePort: 8962,
  startServices: false,
  namespace: 'default',
  shutdown: { disableSignalHandlers: true },
});
const client = createClient({
  endpoint: 'http://127.0.0.1:8962',
  namespace: 'default',
  disableMetadataLookup: true,
  headers: { 'x-peri-sandbox-token': token },
});

let held = false;
try {
  await registry.startAndWait();
  const sbx = client.sandbox.getOrCreate(['sbx', workspace]);
  const acquired = await sbx.acquire(holder, token);
  if (!acquired.ok) throw new Error(`${acquired.error.code}: ${acquired.error.message}`);
  held = true;

  // 只含地址、actor id 与 path；凭据由 Chat 以 header 发送。
  const url = new URL(
    `/gateway/${encodeURIComponent(await sbx.resolve())}/request/mcp`,
    'http://127.0.0.1:8962',
  );
  console.log(`SANDBOX_MCP_URL=${url.href}`);

  await new Promise<void>((resolve) => {
    process.once('SIGINT', resolve);
    process.once('SIGTERM', resolve);
  });
} finally {
  try {
    if (held) {
      const released = await client.sandbox.getOrCreate(['sbx', workspace]).release(holder, token);
      if (!released.ok) {
        console.error(`release incomplete: ${released.error.code}`);
        process.exitCode = 1;
      }
    }
  } catch (error) {
    console.error(`release failed: ${error instanceof Error ? error.name : typeof error}`);
    process.exitCode = 1;
  }
  // 先停 Chat，再关闭 actor 准入；host owner 报告已确认的 gate/instance 进度。
  owner.beginShutdown();
  let hostConfirmed = false;
  try {
    const report = await owner.closeAll({ timeoutMs: 5_000 });
    hostConfirmed = report.complete;
    if (!report.complete) {
      console.error(`host resources incomplete: ${report.pending}`);
      process.exitCode = 1;
    }
  } catch (error) {
    console.error(`host close failed: ${error instanceof Error ? error.name : typeof error}`);
    process.exitCode = 1;
  }
  // 兜底 runtime 回收只在 owner 确认全部 gate 已排空后执行；未确认的 stream 仍可能在用实例与
  // 临时 state 目录，不能用进程级 closeAll/rmSync 假装清理完成。
  if (hostConfirmed) {
    try {
      await runtime.closeAll();
    } catch (error) {
      console.error(`runtime closeAll failed: ${error instanceof Error ? error.name : typeof error}`);
      process.exitCode = 1;
    }
  } else {
    console.error('runtime closeAll deferred: host resources are not confirmed');
  }
  // client 与 engine 收尾不依赖资源确认，失败时也要执行；未确认由退出码反映。
  await client.dispose().catch(() => undefined);
  await registry.shutdown().catch(() => undefined);
}
```

holder 由宿主定义：同一 workspace 的多个 Chat 可以各自 acquire 并共享环境，HTTP 连接断开不改变引用计数；只有显式 release 才减少引用，最后一个 release 才关闭环境。示例用一个常驻 holder 服务所有 Chat。

管理动作 `acquire/release/status/closeAll` 都需要同一个 `token` 作为显式参数。本地 engine 的 gateway token 是 `default`（RivetKit 本地 spawn 行为），不是本项目的 secret；actor 的 HTTP 与 action 保护由 `SANDBOX_MCP_TOKEN` 承担。缺少/错误凭据在触达 runtime 前拒绝；workspace 授权（machineId + canonical URI allowlist + 逐字 canonical 目录）独立校验。

`onRequest` 只接受 `/mcp`（也接受 gateway 原始形态 `/gateway/{actorId}/request/mcp`），转发 method/body/协议 headers/stream 与 abort signal；转发请求用 URL + init 显式构造，`authorization`/`cookie`/`proxy-authorization`、网关的 `x-rivet-token` 与 actor 的 `x-peri-sandbox-token` 在构造前移除，不依赖 `new Request(request, { headers })` 的覆盖语义（Bun 1.4.3 在空 Headers 覆盖时会保留原 headers）。内部 MCP 只用 `peri-sandbox` 实例自己的 Bearer token。没有 active holder 时返回 503，不隐式 acquire。最后一个 release 先关闭新入口，主动 abort 并 cancel 实际 response reader/stream；只有 cancel resolve、或取消前已观察到 read done/error 才算 settled，cancel 抛错不算确认并保留 inflight/holder，也不对已结束的 stream 重复 cancel。收束未确认或超时返回 `sandbox_close_incomplete`（`drain_failed`）并保留 holder/实例供重试。late response 的 body cancel 失败同样不结束请求。`onSleep`/`onDestroy` 走同一收束，无法确认时保留诊断且不关闭实例。gate、inflight 与关闭 closure 归显式宿主 owner：`vars` 被框架清理后，同 key 的新 activation 仍必须先确认旧 gate 关闭，HTTP 在旧记录未清时返回 `sandbox_closing`；`owner.beginShutdown()` 之后 action 准入在 refs 队列内再次校验 retired lease，不再启动新 runtime，lease retire 同时解除该 activation 的 keepAwake；`retire(vars)` 是终态，即使确认关闭删除了 owner 记录，同一 vars 的 acquire 仍返回 `closed`，同 key 的新 activation 不受影响；`owner.closeAll` 可枚举重试，只有它确认排空后才允许适配器兜底 `runtime.closeAll()`，cancel 永久拒绝时保持 incomplete，不承诺恢复。诊断出口会把已知 actor token 从错误的 message/stack/cause 中投影为 `[redacted]`，原始错误不会进入日志 sink。

原始 HTTP 路由见 [Rivet request handler](https://rivet.dev/actors/docs/request-handler/)：`/gateway/{actorId}/request/{path}`。URL 只含地址、actor ID 与 namespace；token 由 Chat 以 header 发送，不写进 URL 或日志。

本地示例绑定 `127.0.0.1`，Rivet 状态由 `RIVETKIT_STORAGE_PATH` 固定到执行目录（示例默认 `./.rivetkit`），不用全局目录。远程可达部署还须认证 Chat 身份并授权 workspace；URI/key 不是凭证，actor token 不替代业务授权。远程平台若已管理环境生命周期，直接用其 MCP endpoint/client/adapter，不必再建这个 actor。

## 检查

```bash
# 包目录
bun run check:engine   # engine 平台二进制；缺失时按打印命令安装
bun run test           # 单元测试，不启动真实模型

# 消费项目目录
node node_modules/@peri-code/rivet/scripts/check-engine.mjs
```

自定义 transport 见 `docs/custom-runtime.md`；设计见 `DESIGN.md`。

本次本地打包（源 `git c5293b861bcfdd578f34881f093ffafed784c096`，dirty）输出到
`/tmp/peri-rivet-pack-hostowner-01a1df34-final-r3`；三个 tarball 的 bytes 与 sha256
由该目录的 `pack-manifest.json` 记录。不把 tarball 自身 digest 写进包内 README：修改 README
会改变 `peri-code-rivet` tarball 的 digest，无法自洽。

source fingerprint `b9606484ef8e748b7be8a127e23d637f66297c4cbb73f0c64dc8b333b1a16313`：
对 `git diff HEAD --binary --` 下列路径的字节，加上按路径排序的未跟踪文件
（`path NUL content NUL`）做 SHA-256；路径为 `peri-rivet/{src,tests,examples}`、
`peri-rivet/{package.json,bun.lock,scripts/pack-local.mjs}`、`@peri-sdk/{src,scripts/build.ts,package.json,tsconfig.type-tests.json}`、
`peri-sandbox/{src,package.json}`；README/DESIGN/docs 不参与，避免文档自引用。

SDK WASM 为 `copied-prebuilt`，兼容性 `unverified`（未重建 Rust/WASM）。

包内自检：`bun run typecheck`、`bun test`（211 pass，25 files）、`bun run format:check`；SDK 局部
`chat-state`/`chat-fact-replica`/`public-view` 23 pass 与 SDK `typecheck`。未运行真实模型、Store、
完整 Chat turn 或真实 Sandbox engine 往返；`closeAll` 也不承诺 SIGKILL/进程崩溃后的清理。
