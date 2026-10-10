# @peri-code/sdk

> 边界：SDK 不再包含任何进程控制。stdio transport、Workspace 进程管理、本机进程停止证明、JSONL dispatcher 与本地 SQLite 存储均已删除，不留 shim、别名或双写。ACP 连接的装配只有一条路径：宿主注入 `transportFactory`（WASM 用 `startPeriWasmHost`，其他部署注入自有实现）。

`Sandbox.transportFactory` 是必需的注入点，SDK 不启动 Peri 或 Workspace 进程。`bun run build` 构建 `peri-wasm` release 产物，并将 `peri-wasm.js` 与 `peri_wasm.wasm` 放入包内 `dist/wasm/`。模型请求仍由 Rust `peri-model` 发出，Agent 运行现有 RCRA loop。

`@peri-code/sdk/wasm` 提供不依赖 Node/Bun 的 transport 与 loader；`instantiatePeriWasm({ moduleFactory, env, moduleOptions })` 支持静态 factory 和独立实例，环境在调用时取快照并仅在启动前注入，不读取宿主环境变量。**注入 `moduleFactory` 时 `moduleUrl` 既不解析也不要求可解析**——workerd 打包模块的 `import.meta.url` 是 `undefined`，这类宿主只能走 factory 入口；未注入 factory 而又无法解析产物 URL 时，错误会明确说明该约束而不是抛出裸 `TypeError`。`WasmAcpTransport.start({ module, configJson })` 可直接消费该实例，不能同时指定 `env` 或 `moduleUrl`。`startPeriWasmHost` 由主入口与 `/wasm-host` 共同导出；`/wasm-host` 额外提供 Node-compatible 网络/DNS/timer 端口与关闭证明。`/portable` 暴露准入规则、注册表、投影与 Store 读取，但准入摘要仍依赖 `node:crypto`，不能宣称纯浏览器支持。

Bun SDK for Peri's ACP Host over a host-injected transport. `ManagedAgents.createAgent()` synchronously declares an Agent with one Session. `Session.start()` asynchronously claims their identities in the configured atomic KV, asks the injected transport factory for a connection, initializes ACP, and creates or loads the Session. Peri owns model execution and Session persistence.

```ts
import { ManagedAgents, MemoryKV, Sandbox, startPeriWasmHost } from "@peri-code/sdk";

const sandbox = new Sandbox({
  id: "local-workspace",
  // 凭证与 Store 位置由调用方显式提供；SDK 不读宿主环境变量。
  storage: new TursoStorage({ url: "http://127.0.0.1:8081", authToken: "local-dev" }),
  transportFactory: (path) => startPeriWasmHost({
    configJson: JSON.stringify({
      cwd: path,
      settings: myPeriConfig,
      storage: { url: "http://127.0.0.1:8081", authToken: "local-dev" },
      machineId: "00000000-0000-4000-8000-000000000001",
    }),
  }),
});
const managed = new ManagedAgents({ kv: new MemoryKV() });
const agent = managed.createAgent({
  id: "agent-42",
  sandbox,
  path: "/absolute/existing/workspace",
  instructions: "Help with this workspace.",
});
const session = await agent.session.start(null); // Supply an existing ID to load.
try {
  const { chat, session: sessionDoc } = agent.docs;
  chat.on("update", (update) => console.log("chat update", update));
  sessionDoc.on("update", (update) => console.log("session update", update));
  void (async () => {
    for await (const notification of session.stream()) console.log(notification);
  })();
  await session.send("Inspect the repository"); // Resolves when Peri emits user_input_delivered.
  await session.send("Now inspect the tests");
} finally {
  await managed.closeAgent(agent.id, { drainTimeoutMs: 10_000 });
}
```

`MemoryKV` is only for one process. For multiple host processes, inject an `AtomicManagedAgentKv` adapter whose shared backend implements `claimIfAbsent(key, owner)` and `releaseIfOwner(key, owner)` atomically. A plain unjs `Storage` object cannot guarantee first-wins ownership and is rejected. The shared KV keyspace is the coordination domain: the Session claim key must use only the Session ID, without `Sandbox.id`; the Agent claim key remains Sandbox + Agent. Every instance that might execute the same Session must share this KV. Equal Session IDs in different databases sharing the KV are conservatively rejected; isolate KV by business domain when needed, but never use a different Sandbox to bypass a claim for the same Session. The adapter owns its crash recovery or lease policy; the SDK does not renew or expire claims. Expiry alone is not proof that old execution has stopped.

`close({ drainTimeoutMs: 3000 })` sends `session/close` and replays it while the Host reports the close as incomplete (`-32010`), so the server CloseCoordinator can advance after a prompt unwinds. A three-second drain budget is the default; a zero budget performs exactly one attempt. Deadline expiry preserves transport and claims and throws `SessionCloseIncompleteError` carrying the last reason; a new failure after an incomplete attempt keeps both causes in an `AggregateError`. Non-retryable ACP errors propagate unchanged. Closing an already closed Session is a no-op, and concurrent calls share one operation. These options also apply to `Agent.close`, `ManagedAgents.closeAgent` and `closeAll`.

Execution scheduling belongs to Peri: the SDK publishes input and never drives a dispatcher, work query or admission ticket. If startup fails and transport cleanup is unconfirmed, the Session retains claims and transport in `cleanup-pending`, reporting the original and cleanup errors together. The narrow `cleanupStartup` APIs reject established Sessions. Close acceptance means the Host confirmed the CloseCoordinator settled; an incomplete Close is retried inside the drain budget and preserves claims and transport. `closeAll()` reports all failures rather than swallowing them. Unknown owner proof never authorizes takeover, and claims are released only after the transport is confirmed closed. See the [SDK code index](../../docs/code-index/peri-ts-sdk.md) for validation routes and boundaries.

A Sandbox receives a `SessionStorage` interface and an explicit `transportFactory`; it never starts a process and never resolves local paths. An Agent receives the Sandbox and supplies `path` for a new Session. On load, the SDK looks up the Session by ID in the Store and uses its persisted `cwd`; a loading Agent does not need `path`. `TursoStorage({ url, authToken })` reads metadata over the official `@tursodatabase/serverless` driver with credentials the caller supplies explicitly; there is no local SQLite file backend, no CLI deployment path and no environment-variable fallback for tokens. `agent.getSessions()` reads Session metadata directly from the Store for the Agent's path or loaded Session path; it does not call ACP `session/list`. Workspace-ID lookup is deferred until the Workspace persistence contract is settled.

`PeriConfig` 描述 WASM ACP 启动配置；settings、storage 与 machineId 一起经启动对象交给 Peri。`BareHarnessConfig` 是冻结的全关闭预设，按需开启能力。默认 [demo.ts](examples/demo/demo.ts) 为每个 Session 创建独立 WASM 实例，启用 `McpMiddleware` 与 `ToolSearch`；通过必填的 `PERI_WORKSPACE_MCP_URL` 连接外部 HTTP Workspace，不启动或关闭 Workspace 子进程。Workspace 服务由部署方负责就绪与生命周期。

`Sandbox.id` scopes Agent claims, not the Session execution coordination domain. The SDK does not pass it to Peri as `machine_id`; Peri resolves that identity itself. Do not assume `Sandbox.id` equals the Store's machine identity. Machine/workspace binding checks and SDK execution ownership are separate responsibilities.

`settings` is delivered to Peri inside the startup object the embedder builds (for WASM, `configJson`); omitting it lets Peri load its normal global and workspace configuration. `instructions` uses Peri's `session/new._meta["peri.instructions"]`; HTTP Workspace and extra MCP servers use `session/new.mcpServers`. `stream()` yields raw `session/update` and `peri/agent_event` notifications. Its diagnostic replay buffer retains at most 1,024 events and approximately 4 MiB; an absent or slow consumer exceeding either bound receives `EventStreamOverflowError`. The state projection and input delivery continue. After handling that error, opening a new `stream()` starts a fresh live diagnostic stream. Use the documents for current history.

Input and control use only the current ACP surface: `send(text)` enqueues through `session/input/enqueue` and resolves once Peri emits `user_input_delivered` for the same queue generation; `forceSend()` issues `session/input/dispatch` for that exact input; `takeBack(inputId)` withdraws a queued input and reports `withdrawn` as success. Scheduling belongs to Peri's mailbox, so the SDK has no execution admission, no `ensureProcessing`, no `stop`/`pause`/`resume`/`reopen`, no `controlState` and no `resolveControl`. `cancel()` sends the session-scoped `session/cancel` notification, and how an unfinished turn converges is Peri's decision. A Session whose transport has already shut down must be loaded through a new Agent. No turn-level `wait()` exists because one Session can receive multiple inputs over time.

`agent.docs` and `session.docs` reference the same `SessionDocs` object. Its `chat` and `session` fields are Yjs `Y.Doc` instances for the current Agent Session. The SDK projects ACP notifications into these documents as they arrive, including history replay during `session/load`; consuming `stream()` is not required. The documents are process-local live projections. Peri's Session Store remains the durable source for messages and execution recovery, so after a process restart start the Session again to rebuild them from ACP history. These documents may contain conversation text and other private session data; a Web or Hub transport needs its own authorized, versioned projection rather than forwarding raw Yjs updates.

The Chat Doc (schema 2) keeps ordered user/assistant entries and tool calls. The Session Doc retains schema 1. The Session Doc keeps the current turn, input queue, plans by turn, tasks, configuration, and pending human interactions. Real mailbox input is added to chat when delivered. If a background task notification skips a revision, the SDK requests `session/bg-tasks` and repairs the task projection. `onPermissionRequest` and `onElicitation` on `AgentOptions` handle Peri's reverse ACP requests; pending requests appear in the Session Doc and are rejected or declined when no callback is provided. Permission inputs and form answers are not copied into Yjs.

`readSessionView(chatDoc, sessionDoc)` and `SessionViewStore` are the shared, transport-independent frontend state layer. The reader returns ordered chat blocks with tool cards, turn and session status, tasks, plans, input queue and pending interactions. The store observes transactions without forcing an extra Yjs wire encoding. It invalidates changed types and tool reference edges, reuses unchanged entries/blocks/tools, coalesces notifications in a microtask, and replaces the pair together when a connection changes generation. The main SDK and `/view` builds share the package Yjs dependency; bundle that dependency when serving the view directly to a browser. Cached values are immutable; consumers must not modify snapshots. `readSessionView` is a one-shot reader; use `SessionViewStore` for streaming. A browser mirror starts from two empty `Y.Doc` instances and applies an authorized snapshot and subsequent updates; it must not construct `SessionDocs` or write a permission decision into Yjs. Hosts can connect `InteractionResponder` to `AgentOptions` and pass validated decisions through their chosen command transport.

## Durable Execution Admission

This section describes the portable admission core that embedders (`/portable`) may drive themselves. The SDK `Session` does not use it: input publication is an ACP queue command, and Peri's mailbox decides scheduling, so there is no SDK-side ticket, dispatcher generation or reverse admission endpoint.

`KvExecutionRegistry` and `MemoryExecutionRegistry` expose the same durable read/CAS apply/resolve contract; the KV adapter needs atomic versioned CAS, not just claim/release. `MemoryExecutionRegistry` is explicitly `bestEffort` and requires an explicit coordinator opt-in. Registry writes and receipts are atomic; resolving an absent original mutation seals NotApplied against late execution. Durable backends, ledgers and proof providers are injected by the embedder; the SDK ships no local database file and no process-based stopped proof.

Tickets bind the exact session, instance/generation, lifecycle/control generation, stable work/revision and `{turnId, attemptId}`. The persistent attempt moves through queued/entering/running. Pause/control changes invalidate unentered work, and the execution entry rechecks qualification. Replaying an original admission does not spend the stable-work budget again. Unknown execution entry resolves the original ticket; it never issues a blind second execute. Budgets survive instance replacement. Missing stopped proof yields Blocked, with no lease/TTL takeover.

`RpcError` preserves ACP `code` and `data`, including domain Incomplete responses, so a host-side controller can act on them without parsing messages.

A dispatcher instance is a generation plus its executor, and the embedder owns that lifecycle: the SDK ships no sidecar, no PID/process-group proof and no local registry database. `InstanceDescriptor.proofRoute` accepts `wasmHost` or `external` only; a stopped-instance proof is the embedder's responsibility through `InstanceProofProvider`. `startPeriWasmHost` reports `closed` only after the native port, injected ports and `free()` have all settled, and a timeout is not a stopped proof.

Admission snapshots contain only `sessionId`, `control`, `candidates`, and `blocked`; they never carry the message body or whole domain WorkState.

An accepted durable entry ACK is retained as `enteredEvidenceId`. If recovery of a running/exiting or previously entered ticket finds the domain admission missing (`NotApplied`), the coordinator persists a permanent typed `dataLoss` disaster, keeping its original ticket, entry/stopped evidence and accumulated budget. `coordinator.ensureProcessing` returns `status:"disaster"`, and recovery scans never clear it, mint replacement tickets or replay effects. Admission replies stay blocked with the persisted typed disaster attached. Only entering attempts with no confirmed entry evidence may safely settle an original `NotApplied` as never entered. `test/execution-disaster.test.ts` covers this invariant over the portable registry and ledger.

## Tool Payloads And Replication

Tool `arguments` and `result` values whose JSON encoding is at most 4,096 UTF-8 bytes stay inline. Larger values use `argumentsRef` / `resultRef`: `{ id, bytes, preview }`. Previews contain at most 512 UTF-16 code units and are explicitly incomplete. `session.docs.readPayload(ref.id)` returns the exact JSON string for that immutable version. Resolve it only when the user expands a tool card; do not fetch all references on every render. Small values are copied at admission so mutating the incoming ACP object cannot change replicated state.

References are scoped to one `SessionDocs` lifetime. Replacing a payload retires its previous reference; compact/rewind and destroy release discarded payloads. An unknown/stale reference returns `undefined`, so a remote endpoint should return 404 and refresh the card. The host retains current large payloads in memory. They are rebuilt from Rust's canonical history on a new process; the blob collection is not durable storage or a filesystem cache. Retaining or reading every payload still costs space proportional to the complete session.

`SessionDocSync` supplies a transport-neutral binary protocol (version 2) with Yjs V2 snapshots and updates. It batches to a 16 ms deadline, 64 KiB target or 256 updates, whichever comes first. An indivisible Yjs update can exceed the target. It attaches update encoders only while subscribers exist. `SessionDocs.acceptBatch(events)` additionally lets transports admitting an already collected ACP batch publish one transaction per document. Existing `accept` remains synchronous.

```ts
import { SessionDocSync, SessionDocReplica, SessionViewStore } from "@peri-code/sdk";

const sync = new SessionDocSync(agent.docs.chat, agent.docs.session);
const replica = new SessionDocReplica(); // Empty read-only replicas, including in a browser.
const connection = sync.subscribe((frame) => replica.applyUpdate(frame));
replica.applySnapshot(connection.snapshot);
const view = new SessionViewStore(replica.chat, replica.session);

// After a disconnect, retain the replica and supply its actual state vectors.
connection.unsubscribe();
const resumed = sync.subscribe((frame) => replica.applyUpdate(frame), {
  resume: replica.stateVector(),
  onError: (error) => console.error(error), // Close the wire and reconnect.
});
if (replica.applySnapshot(resumed.snapshot) === "replaced")
  view.replaceDocs(replica.chat, replica.session);

sync.flush(); // Explicit terminal boundary; includes pending tail text.
resumed.unsubscribe();
await sync.close(); // Drains admitted asynchronous sends. Unsubscribe aborts a subscriber.
view.destroy();
replica.destroy();
```

A resumed connection receives missing structs **and deletions**, including deletions that did not change a state vector. A different producer generation sends a fresh pair. Frames have one sequence across both documents; the replica rejects gaps and unknown generations so the transport can reconnect and repair with state vectors. This is one-way state replication; commands and permission decisions still use ACP. Authenticate/authorize the session before sending its snapshots or serving payload references.

Subscriber callbacks may return a promise, resolved only when the transport has accepted the frame. Each subscriber owns its frame buffers and may transfer them to a Worker. Each has a 4 MiB queue budget, including its in-flight update; overflow detaches that subscriber with `SyncBackpressureError`. A failing/slow callback does not stop other subscribers. `close()` waits for admitted sends, so transport aborts must settle their pending sends or unsubscribe. Initial snapshots are sent separately and can exceed the live queue budget. Tune limits with `DocSyncOptions`.

The [benchmark CLI](benchmarks/README.md) reproduces 12,000 tools, large payloads and 16 MiB streaming with exact content checks. Measurements and limits are recorded in the [experiment report](../../docs/experiment-yjs-sdk/conclusion.md).

The [Bun demo](examples/demo/demo.ts) serves a [single HTML page](examples/demo/demo.html) through Hono at `http://127.0.0.1:3000`; Hono logs HTTP requests to the terminal. Its management API uses POST throughout: `/api/session/list` reads Session metadata from storage without starting an Agent; `/api/session/create` accepts `{ sessionId: null, after: 0, requestId }` for a new Session or an existing ID to load, then streams `session`, `docs:snapshot` and batched `docs:update` events over SSE; `/api/session/send` accepts `{ sessionId, text }` and returns `{ inputId, delivered }` with HTTP 202 after delivery. `/api/session/interaction/respond` accepts `{ sessionId, interactionId, kind, decision }` for a pending permission or question. The browser renders the shared Session view from mirrored Y.Docs; reconnect sends state vectors to receive missing changes, while a new generation replaces both documents. The page initially renders the latest 200 blocks, reuses unchanged DOM nodes and offers earlier history on demand. Very long text has a bounded visible tail and a full-output download. Expanding a tool fetches its referenced JSON from `/api/session/payload`. Raw diagnostic `notification` events are opt in (`diagnostics: true`, or open the page with `?diagnostics=1`); the diagnostic log retains at most 256 events and approximately 1 MiB. The SSE adapter is limited to this loopback demo; it base64-encodes the SDK binary frames and bounds pending writes. Production clients can carry the same SDK frames directly over a binary transport with their own authorization. `requestId` makes retries of new Session creation reuse the same Agent within this demo process. The browser uses `@microsoft/fetch-event-source` to POST and resume the stream from an in-memory event cursor. Clicking a Session opens `/?sessionId=...` and loads the persisted Session through an Agent bound to it. The demo uses `MemoryKV`, which only guards one process, and stores sessions in a local libSQL HTTP server at `127.0.0.1:8081`, backed by `target/peri-sdk-local-turso`. On macOS arm64, `mise install` installs the pinned Turso CLI and its `sqld` server. Set `PERI_WORKSPACE` to an existing directory and configure `ANTHROPIC_API_KEY` and `ANTHROPIC_BASE_URL` in the package's ignored `.env` file. Bun loads it when run from this directory:

```bash
cd npm-packages/@peri-sdk
mise install
bun install --frozen-lockfile
bun run build # 构建 SDK 与 WASM；Workspace HTTP 服务由部署方提供
bun run db:dev # Keep this running in a separate terminal.
bun run dev
bun run typecheck
bun run test
bun run build
```

打开 `http://127.0.0.1:3000`。`PORT` 配置 demo HTTP 端口；必填 `PERI_WORKSPACE_MCP_URL` 指向部署方运行的 HTTP Workspace，`PERI_WORKSPACE` 是该工具环境中的路径。demo 不再读取 `PERI_BIN` 或 `PERI_WORKSPACE_BIND`，也不管理 Workspace 进程。`PERI_WASM_MODULE_URL` 可指定产物，默认使用包内 `dist/wasm/peri-wasm.js`；`PERI_WASM_MACHINE_ID` 显式设置实例机器身份。会话存储仍使用本地 libSQL HTTP listener，`PERI_DEMO_TURSO_URL` 可改为另一 loopback 地址。demo 使用未认证本地服务的占位凭证；不要据此部署公开数据库。

## WASM ACP transport demo

[demo-wasm.ts](examples/demo/demo-wasm.ts) 展示按 URL 缓存模块的 `WasmAcpTransport.start(...)`；默认 [demo.ts](examples/demo/demo.ts) 使用 `startPeriWasmHost` 为每个 Session 装配独立实例。两者共享 `Sandbox`、`ManagedAgents`、`Agent`、`Session` 和 [demo.html](examples/demo/demo.html)。factory 接收新 Agent 路径或历史 Session 的持久路径，ACP 传输仍是完整 JSON-RPC 帧。启动配置包含 `{ cwd, settings, storage: { url, authToken }, machineId }`；`session/new` 仍经 ACP 传入 instructions 和 MCP servers。

```bash
cd npm-packages/@peri-sdk
bun run db:dev # Separate terminal, or set PERI_DEMO_TURSO_URL.
PERI_WORKSPACE=/absolute/workspace ANTHROPIC_API_KEY=... ANTHROPIC_BASE_URL=... bun run demo:wasm
```

The server checks for the `PeriWasmAcp` export before listening and reports a missing ACP Host explicitly. `PERI_WASM_MODULE_URL` selects another WASM glue module; `PERI_WASM_MACHINE_ID` sets a persistent UUID execution identity (the demo defaults to `00000000-0000-4000-8000-000000000001`). `PERI_WORKSPACE_MCP_URL` can supply an external HTTP Workspace MCP endpoint. Builtin MCP servers are not started in WASM.

### WASM Langfuse 上报

通过 `WasmAcpTransport.start({ env })` 在 Emscripten 模块初始化时注入环境变量。SDK 使用 `preRun` 写入构建时导出的 `ENV`，Rust 继续复用现有配置解析、Controller 观测旁路和 Langfuse HTTP exporter，不需要 JS Langfuse SDK，也不修改 Rust 配置规则。

```ts
const transport = await WasmAcpTransport.start({
  env: {
    LANGFUSE_PUBLIC_KEY: "pk-lf-...",
    LANGFUSE_SECRET_KEY: "sk-lf-...",
    LANGFUSE_BASE_URL: "https://cloud.langfuse.com",
    LANGFUSE_USER_ID: "sdk-service",
    LANGFUSE_TRACE_SAMPLING: "1",
  },
  configJson: JSON.stringify({ cwd, settings, storage, machineId }),
});
```

`env` 的值必须为字符串，两把 key 都配置后才启用。`LANGFUSE_BASE_URL` 是服务根地址，不是完整 OTLP 路径；Rust exporter 上报到 `/api/public/otel/v1/traces`。采样、批次、容量和刷新周期的配置仍以[环境变量规范](../../docs/standards/environment-variables.md)为准。JavaScript 宿主的环境不会自动传播到 Rust，`settings.config.env` 也不是此注入入口。

Bun demo 自动显式传入宿主的 `LANGFUSE_*` 变量：在包内未跟踪的 `.env` 中配置凭证和可选服务地址，然后执行 `bun run demo:wasm`。首次使用新入口需要 `bun run build` 重建导出 `ENV` 的产物；旧的自定义 `PERI_WASM_MODULE_URL` 也需要重建。

环境属于 **WASM 模块实例**，不是 ACP Session。同一模块 URL 缓存并共享一个实例，重复加载只能传相同环境；显式传入不同环境会报错，不会静默覆盖其他 Agent 的凭证。先调用 `loadPeriWasm(moduleUrl, env)` 预加载时必须使用相同环境；省略 `env` 则复用已初始化环境。需要不同环境的部署应使用独立模块 URL 或独立 Worker/进程，初始化后不修改环境。

凭证只应由受信的 Bun 服务或 Workers secret binding 注入，不能把 Langfuse secret 放入浏览器前端或日志。浏览器直接运行 WASM 时应改为服务端执行或受鉴权的遥测代理，把凭证留在服务端。上报可能包含模型输入、输出及工具数据，启用前确认数据策略和网络可达性。

正常退出调用 `managed.closeAgent(id, { drainTimeoutMs })` / `managed.closeAll({ drainTimeoutMs })`：先让 Host 结算 `session/close`，再排空 transport、释放 claims 和遥测。直接 transport 的 `close()` 只关闭连接，不构成会话关闭；不要依赖强杀进程或页面卸载。遥测失败保持旁路诊断，不能把业务成功当成上报成功。`tests/wasm-acp-integration.test.ts` 使用本地模拟 OTLP 服务检查实际 WASM 上报，无需真实 Langfuse 凭证。

## Local Cloudflare Workers probe

The [Workers example](examples/workers/worker.js) uses the same SDK package and
its `bun.lock`. It starts the Peri ACP Host in Wrangler's local `workerd`, with
a local sqld and simulated model endpoint. Credentials and the Store location are
passed explicitly through `STORAGE_URL` / `MODEL_URL` vars; the example never
reads host process environment.

Each request performs one bounded probe, and every probe closes its Session with the
same bounded `session/close` replay the SDK uses:

| Probe | ACP path |
| --- | --- |
| `/` (default) | `session/new` → `session/input/snapshot` → `session/input/enqueue` → wait for `user_input_delivered` and `peri/agent_event_done` → `session/close` |
| `/?turn=prompt` | `session/new` → `session/prompt` → `session/close` |
| `/?sessionId=<id>` | `session/list` → `session/load` → `session/close` |
| `/?host=sdk` | SDK `startPeriWasmHost` 只注入 `moduleFactory`（peri-cf 形态）→ `initialize` → `session/new` → `session/close`；响应回带该运行时的 `import.meta.url`（workerd 为 `undefined`） |

The queue path is the one `Session.send` uses; the input queue and the event channels are
negotiated through `clientCapabilities._meta` at `initialize`. `session/prompt` is a current
Host method (`peri-acp/src/host/server_loop.rs` dispatches it) used by other clients, not a
legacy entry point. A failed turn is cancelled with the `session/cancel` notification before
closing, because an active turn keeps `session/close` incomplete.

```bash
cd npm-packages/@peri-sdk
bun install --frozen-lockfile
bun run smoke:workers
```

`bun run build:workers` builds the SDK's WASM artifact and copies it into the
example's ignored `dist/` directory for Wrangler. The probe does not validate
a hosted Cloudflare deployment.
