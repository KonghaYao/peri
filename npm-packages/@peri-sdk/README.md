# @peri-code/sdk

Bun SDK for Peri's existing ACP stdio endpoint. `ManagedAgents` synchronously declares one Agent with one Session. `Session.start()` asynchronously claims the Agent and Session in shared KV, starts Peri, initializes ACP, and creates or loads the Session. Peri owns model execution and the Session Store.

```ts
import { ManagedAgents, Sandbox, TursoStorage, type SessionStorage } from "@peri-code/sdk";

const storage: SessionStorage = new TursoStorage({
  url: "libsql://your-database.turso.io",
  authToken: "...",
});
const sandbox = new Sandbox({
  id: "workspace-42",
  path: "/absolute/workspace/path",
  storage,
  stdio: { command: "peri" },
  workspace: { url: "https://workspace.example.com/mcp" },
});
const managed = new ManagedAgents({ kv: atomicKv });
const agent = managed.createAgent({
  id: "agent-42",
  sandbox,
  instructions: "Help with this workspace.",
});
const session = await agent.session.start(null); // Supply an existing ID to load.
try {
  void (async () => {
    for await (const notification of session.stream()) console.log(notification);
  })();
  await session.send("Inspect the repository"); // Resolves when Peri emits user_input_delivered.
  await session.send("Now inspect the tests");
} finally {
  await managed.closeAgent(agent.id);
}
```

`atomicKv` must implement `claimIfAbsent(key, owner)` and `releaseIfOwner(key, owner)` atomically on a shared backend. A plain unjs `Storage` object cannot guarantee first-wins ownership and is rejected. The supplied adapter is responsible for crash recovery/lease policy; the current SDK does not renew or expire claims.

`MemoryKV` is the SDK's local single-process implementation for demos and tests. It does not share claims across processes or retain them after exit. A Sandbox receives a `SessionStorage` interface, owns transport startup, and uses the storage deployment when the Session starts. An Agent receives the Sandbox. Use `SqliteFileStorage({ path })` for a local SQLite file or `TursoStorage({ url, authToken })` for Turso. `agent.getSessions()` delegates to `Sandbox.getSessions()` and reads Session metadata directly from the Store for the Sandbox path; it does not call ACP `session/list`. Remote reads use the official `@tursodatabase/serverless` driver. Workspace-ID lookup is deferred until the Workspace persistence contract is settled.

`PeriConfig` types the settings document passed through `Sandbox.stdio.settings`. This document replaces Peri's global settings at startup. `BareHarnessConfig` is a frozen SDK preset that disables every known MetaHarness key; spread it into `meta_harness` and enable only the capabilities needed. The runnable demo defines a complete Anthropic provider and Sonnet profile in [demo.ts](examples/demo/demo.ts), enables `McpMiddleware` and `ToolSearch`, and starts a local Workspace HTTP MCP process. The Sandbox waits for MCP initialization and a real `tools/list` before the Agent Session starts. Closing the Agent does not stop Workspace; the Sandbox closes it when the demo exits.

`Sandbox.id` scopes SDK ownership. Unchanged Peri has no trusted ACP or stdio input for machine identity, so its persisted `machine_id` remains Peri's own UUID and does **not** equal `Sandbox.id`. This is an outstanding contract gap for cross-instance execution admission; the SDK does not claim to solve it through ACP or an environment variable.

When `settings` is supplied, it is sent once before ACP as a four-byte big-endian length followed by raw JSON. Omit `settings` to let Peri load its normal global and workspace configuration. `instructions` uses Peri's `session/new._meta["peri.instructions"]`; HTTP Workspace and extra MCP servers use `session/new.mcpServers`. `stream()` yields raw `session/update` and `peri/agent_event` notifications. `cancel()` sends `session/cancel` without closing the Session. No turn-level `wait()` exists because one Session can receive multiple inputs over time.

The [Bun demo](examples/demo/demo.ts) serves a [single HTML page](examples/demo/demo.html) through Hono at `http://127.0.0.1:3000`; Hono logs HTTP requests to the terminal. Its management API uses POST throughout: `/api/session/list` reads Session metadata from storage without starting an Agent; `/api/session/create` accepts `{ sessionId: null, after: 0, requestId }` for a new Session or an existing ID to load, then streams a `session` event followed by `notification` events over SSE; `/api/session/send` accepts `{ sessionId, text }` and returns the delivery receipt. `requestId` makes retries of a new Session creation reuse the same Agent in this demo process. The browser uses `@microsoft/fetch-event-source` to POST and resume the stream from an event cursor. Clicking a Session opens `/?sessionId=...` and loads its persisted history through a new Agent bound to that Session. The demo calls `ManagedAgents.createAgent()`, `Session.start()`, and `Session.send()` directly; a local Session ID map lets concurrent HTTP requests share an Agent and its event replay buffer. The chat shows actual ACP tool calls and their results alongside assistant text; the information dialog retains the raw events for the lifetime of the demo process. Opened Agents remain alive until the demo stops. Cross-instance exclusivity requires a shared atomic KV adapter, and persisted Session state remains in the Store. The demo uses `MemoryKV`, which only guards one process. It stores sessions in a local libSQL HTTP server at `127.0.0.1:8081`, backed by `target/peri-sdk-local-turso`. On macOS arm64, `mise install` installs the pinned Turso CLI and its `sqld` server. Fill in `PERI_WORKSPACE` and `ANTHROPIC_API_KEY` in the package's ignored `.env` file. Bun loads it when run from this directory:

```bash
./scripts/cargo-rmcp-patched.sh build --locked -p peri-tui --bin peri -p peri-mcp-workspace --bin peri-mcp-workspace
cd npm-packages/@peri-sdk
mise install
bun install --frozen-lockfile
bun run db:dev # Keep this running in a separate terminal.
bun run dev
bun run typecheck
bun test
bun run build
```

Open `http://127.0.0.1:3000` while `bun run dev` is running. `PORT` changes the demo HTTP port; `PERI_WORKSPACE_BIND` changes the Workspace MCP listener (default `127.0.0.1:8765`); `PERI_BIN` selects another Peri binary, with `peri-mcp-workspace` beside it. The demo uses the local libSQL HTTP listener at `127.0.0.1:8081`; `PERI_DEMO_TURSO_URL` can select another loopback HTTP listener for isolated runs. Existing remote `PERI_TURSO_URL` and `PERI_TURSO_AUTH_TOKEN` values do not affect the demo. The workspace files and local Session Store persist when the demo stops. The server starts via `sqld` directly because `turso dev` binds its HTTP listener to all interfaces. Peri currently requires a nonempty remote credential source, so the demo supplies a harmless placeholder for this unauthenticated loopback server.
