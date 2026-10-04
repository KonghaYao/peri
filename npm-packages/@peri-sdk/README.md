# @peri-code/sdk

SDK 的 `Sandbox` 可通过 `transportFactory` 使用 Emscripten WASM 中的现有 ACP Host；`ManagedAgents`、`Agent` 和 `Session` 接口与 stdio 模式共用。`bun run build` 构建 `peri-wasm` release 产物，并将 `peri-wasm.js` 与 `peri_wasm.wasm` 放入包内 `dist/wasm/`。模型请求仍由 Rust `peri-model` 发出，Agent 运行现有 RCRA loop。

Bun SDK for Peri's existing ACP stdio endpoint. `ManagedAgents.createAgent()` synchronously declares an Agent with one Session. `Session.start()` asynchronously claims their identities in the configured atomic KV, starts Peri, initializes ACP, and creates or loads the Session. Peri owns model execution and Session persistence.

```ts
import { ManagedAgents, MemoryKV, Sandbox, SqliteFileStorage } from "@peri-code/sdk";

const sandbox = new Sandbox({
  id: "local-workspace",
  storage: new SqliteFileStorage({ path: "/absolute/store/sessions.db" }),
  stdio: { command: "peri" },
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
  await managed.closeAgent(agent.id);
}
```

`MemoryKV` is only for one process. For multiple host processes, inject an `AtomicManagedAgentKv` adapter whose shared backend implements `claimIfAbsent(key, owner)` and `releaseIfOwner(key, owner)` atomically. A plain unjs `Storage` object cannot guarantee first-wins ownership and is rejected. The adapter owns its crash recovery or lease policy; the SDK does not renew or expire claims.

A Sandbox receives a `SessionStorage` interface, owns transport startup, and supplies the Store deployment when the Session starts. An Agent receives the Sandbox and supplies `path` for a new Session. On load, the SDK looks up the Session by ID in the Store and uses its persisted `cwd`; a loading Agent does not need `path`. Use `SqliteFileStorage({ path })` for a local SQLite file, or `TursoStorage({ url: "turso://your-database.turso.io", authToken })` for Turso Cloud. For a `libsql://` URL, set `engine: "libsql"` explicitly. `agent.getSessions()` reads Session metadata directly from the Store for the Agent's path or loaded Session path; it does not call ACP `session/list`. Remote reads use the official `@tursodatabase/serverless` driver. Workspace-ID lookup is deferred until the Workspace persistence contract is settled.

`PeriConfig` types the settings document passed through `Sandbox.stdio.settings` or the WASM ACP startup object. This document replaces Peri's global settings at startup. `BareHarnessConfig` is a frozen SDK preset that disables every known MetaHarness key; spread it into `meta_harness` and enable only the capabilities needed. The runnable demo defines a complete Anthropic provider and Sonnet profile in [demo.ts](examples/demo/demo.ts), enables `McpMiddleware` and `ToolSearch`, and starts a local Workspace HTTP MCP process. The Sandbox waits for MCP initialization and a real `tools/list` before the Agent Session starts. Closing the Agent does not stop Workspace; the Sandbox closes it when the demo exits.

`Sandbox.id` scopes SDK ownership claims. The SDK does not pass it to Peri as `machine_id`; Peri resolves that identity itself. Do not assume `Sandbox.id` equals the Store's machine identity. Cross-instance execution admission across this boundary remains an open contract.

When `settings` is supplied, it is sent once before ACP as a four-byte big-endian length followed by raw JSON. Omit `settings` to let Peri load its normal global and workspace configuration. `instructions` uses Peri's `session/new._meta["peri.instructions"]`; HTTP Workspace and extra MCP servers use `session/new.mcpServers`. `stream()` yields raw `session/update` and `peri/agent_event` notifications. `cancel()` sends `session/cancel` without closing the Session. No turn-level `wait()` exists because one Session can receive multiple inputs over time.

`agent.docs` and `session.docs` reference the same `SessionDocs` object. Its `chat` and `session` fields are Yjs `Y.Doc` instances for the current Agent Session. The SDK projects ACP notifications into these documents as they arrive, including history replay during `session/load`; consuming `stream()` is not required. The documents are process-local live projections. Peri's Session Store remains the durable source for messages and execution recovery, so after a process restart start the Session again to rebuild them from ACP history. These documents may contain conversation text and other private session data; a Web or Hub transport needs its own authorized, versioned projection rather than forwarding raw Yjs updates.

The Chat Doc keeps ordered user/assistant entries and tool calls. The Session Doc keeps the current turn, input queue, plans by turn, tasks, configuration, and pending human interactions. Real mailbox input is added to chat when delivered. If a background task notification skips a revision, the SDK requests `session/bg-tasks` and repairs the task projection. `onPermissionRequest` and `onElicitation` on `AgentOptions` handle Peri's reverse ACP requests; pending requests appear in the Session Doc and are rejected or declined when no callback is provided. Permission inputs and form answers are not copied into Yjs.

`readSessionView(chatDoc, sessionDoc)` and `SessionViewStore` are the shared, transport-independent frontend state layer. The reader returns ordered chat blocks with tool cards, turn and session status, tasks, plans, input queue and pending interactions. The store observes both docs, coalesces their updates and replaces the pair together when a connection changes generation. A browser mirror starts from two empty `Y.Doc` instances and applies an authorized snapshot and subsequent updates; it must not construct `SessionDocs` or write a permission decision into Yjs. Hosts can connect `InteractionResponder` to `AgentOptions` and pass validated decisions through their chosen command transport.

The [Bun demo](examples/demo/demo.ts) serves a [single HTML page](examples/demo/demo.html) through Hono at `http://127.0.0.1:3000`; Hono logs HTTP requests to the terminal. Its management API uses POST throughout: `/api/session/list` reads Session metadata from storage without starting an Agent; `/api/session/create` accepts `{ sessionId: null, after: 0, requestId }` for a new Session or an existing ID to load, then streams `session`, `docs:snapshot`, `docs:update` and diagnostic `notification` events over SSE; `/api/session/send` accepts `{ sessionId, text }` and returns `{ inputId, delivered }` with HTTP 202 after delivery. `/api/session/interaction/respond` accepts `{ sessionId, interactionId, kind, decision }` for a pending permission or question. The browser renders the shared Session view from mirrored Y.Docs; reconnect starts with a fresh document pair and snapshot, so cold `session/load` history is visible. The Yjs SSE bridge is limited to this loopback demo; production clients need authorization and their own transport. `requestId` makes retries of new Session creation reuse the same Agent within this demo process. The browser uses `@microsoft/fetch-event-source` to POST and resume the stream from an in-memory event cursor. Clicking a Session opens `/?sessionId=...` and loads the persisted Session through an Agent bound to it. The demo uses `MemoryKV`, which only guards one process, and stores sessions in a local libSQL HTTP server at `127.0.0.1:8081`, backed by `target/peri-sdk-local-turso`. On macOS arm64, `mise install` installs the pinned Turso CLI and its `sqld` server. Set `PERI_WORKSPACE` to an existing directory and configure `ANTHROPIC_API_KEY` and `ANTHROPIC_BASE_URL` in the package's ignored `.env` file. Bun loads it when run from this directory:

```bash
./scripts/cargo-rmcp-patched.sh build --locked -p peri-tui --bin peri -p peri-mcp-workspace --bin peri-mcp-workspace
cd npm-packages/@peri-sdk
mise install
bun install --frozen-lockfile
bun run db:dev # Keep this running in a separate terminal.
bun run dev
bun run typecheck
bun run test
bun run build
```

Open `http://127.0.0.1:3000` while `bun run dev` is running. `PORT` changes the demo HTTP port; `PERI_WORKSPACE_BIND` changes the Workspace MCP listener (default `127.0.0.1:8765`); `PERI_BIN` selects another Peri binary, with `peri-mcp-workspace` beside it. The demo uses the local libSQL HTTP listener at `127.0.0.1:8081`; `PERI_DEMO_TURSO_URL` can select another loopback HTTP listener for isolated runs. Existing remote `PERI_TURSO_URL` and `PERI_TURSO_AUTH_TOKEN` values do not affect the demo. The workspace files and local Session Store persist when the demo stops. The server starts via `sqld` directly because `turso dev` binds its HTTP listener to all interfaces. Peri currently requires a nonempty remote credential source, so the demo supplies a harmless placeholder for this unauthenticated loopback server.

## WASM ACP transport demo

[demo-wasm.ts](examples/demo/demo-wasm.ts) uses the same `Sandbox`, `ManagedAgents`, `Agent`, `Session`, API routes, and [demo.html](examples/demo/demo.html) as the native demo. Its only client change is `Sandbox.transportFactory: (path) => WasmAcpTransport.start(...)`. The factory receives the new Agent path or the loaded Session's persisted path. The adapter sends and receives complete JSON-RPC frames through `PeriWasmAcp`; the shared `JsonRpcTransport` handles IDs, notifications and reverse requests for both stdio and WASM. The startup object contains `{ cwd, settings, storage: { url, authToken }, machineId }`. `session/new` still supplies instructions and MCP servers through ACP.

```bash
cd npm-packages/@peri-sdk
bun run db:dev # Separate terminal, or set PERI_DEMO_TURSO_URL.
PERI_WORKSPACE=/absolute/workspace ANTHROPIC_API_KEY=... ANTHROPIC_BASE_URL=... bun run demo:wasm
```

The server checks for the `PeriWasmAcp` export before listening and reports a missing ACP Host explicitly. `PERI_WASM_MODULE_URL` selects another WASM glue module; `PERI_WASM_MACHINE_ID` sets a persistent UUID execution identity (the demo defaults to `00000000-0000-4000-8000-000000000001`). `PERI_WORKSPACE_MCP_URL` can supply an external HTTP Workspace MCP endpoint. Builtin MCP servers are not started in WASM.

## Local Cloudflare Workers probe

The [Workers example](examples/workers/worker.js) uses the same SDK package and
its `bun.lock`. It starts the Peri ACP Host in Wrangler's local `workerd`, with
a local sqld and simulated model endpoint. The smoke checks a prompt, model
HTTP, session listing, and loading after the Host closes.

```bash
cd npm-packages/@peri-sdk
bun install --frozen-lockfile
bun run smoke:workers
```

`bun run build:workers` builds the SDK's WASM artifact and copies it into the
example's ignored `dist/` directory for Wrangler. The probe does not validate
a hosted Cloudflare deployment.
