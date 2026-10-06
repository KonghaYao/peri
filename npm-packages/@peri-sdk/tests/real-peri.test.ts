import { closeExpectation } from "../test/control-fixture";
import { afterEach, expect, test } from "bun:test";
import { mkdtemp, mkdir, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { createServer, type Server } from "node:http";
import { createServer as createTcpServer } from "node:net";
import { Database } from "bun:sqlite";
import { StdioTransport } from "../src/transport/stdio-transport.ts";
import type { Transport } from "../src/transport/types.ts";
import { Sandbox } from "../src/sandbox/sandbox.ts";
import { ManagedAgents } from "../src/managed/managed-agents.ts";
import { AgentClaimConflictError } from "../src/kv/agent-claim-conflict-error.ts";
import { MemoryKV } from "../src/kv/memory-kv.ts";
import { SqliteFileStorage } from "../src/storage/sqlite-file-storage.ts";
import { SqliteExecutionRegistry } from "../src/execution/sqlite-registry.ts";
import { SessionExecution } from "../src/execution/session-execution.ts";
import { SessionControl } from "../src/agent/session-control.ts";
import { drainClose } from "../src/agent/close-drain.ts";
import type { PeriConfig } from "../src/config/peri-config.ts";

const periBinary = resolve(import.meta.dir, "../../../target/debug/peri");
const temporaryRoots: string[] = [];
const modelServers: Server[] = [];

type Fixture = {
  home: string;
  workspace: string;
  database: string;
  settings: PeriConfig;
};

async function fixture(): Promise<Fixture> {
  const root = await mkdtemp(resolve(tmpdir(), "peri-sdk-real-"));
  temporaryRoots.push(root);
  const home = resolve(root, "home");
  const workspacePath = resolve(root, "workspace");
  await mkdir(resolve(home, ".peri"), { recursive: true });
  await mkdir(workspacePath);
  const workspace = await realpath(workspacePath);
  // A broken on-disk document proves that the trusted stdin settings take precedence.
  await writeFile(resolve(home, ".peri/settings.json"), "{not-json", "utf8");
  const model = createServer(async (request, response) => {
    for await (const _ of request) {
      /* Drain the request body. */
    }
    response.writeHead(200, { "content-type": "text/event-stream" });
    response.write(
      'data: {"choices":[{"index":0,"delta":{"content":"FIXTURE_REPLY"},"finish_reason":null}]}\n\n',
    );
    response.write(
      'data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\n',
    );
    response.end("data: [DONE]\n\n");
  });
  modelServers.push(model);
  await new Promise<void>((done, reject) => {
    model.once("error", reject);
    model.listen(0, "127.0.0.1", done);
  });
  const address = model.address();
  if (!address || typeof address === "string")
    throw new Error("fake model port unavailable");
  return {
    home,
    workspace,
    database: resolve(root, "sessions.db"),
    settings: {
      config: {
        active_alias: "sonnet",
        providers: [
          {
            id: "injected",
            type: "openai",
            apiKey: "unused-test-key",
            baseUrl: `http://127.0.0.1:${address.port}/v1`,
          },
        ],
        profiles: { sonnet: { provider: "injected", model: "fixture-model" } },
      },
    },
  };
}

async function start(f: Fixture): Promise<StdioTransport> {
  return StdioTransport.start({
    command: periBinary,
    args: [
      "--db-path",
      f.database,
      "acp",
      "--cwd",
      f.workspace,
      "--settings-stdin",
    ],
    cwd: f.workspace,
    env: { HOME: f.home },
    settings: f.settings,
  });
}

async function initialize(transport: Transport, executionProtocol = false) {
  return transport.request<{
    protocolVersion: number;
    agentCapabilities: object;
  }>("initialize", { protocolVersion: 1, ...(executionProtocol ? { clientCapabilities: { _meta: { "peri.executionProtocol": 2, "peri.userInputQueue": true } } } : {}) });
}

async function unusedLoopbackPort(): Promise<number> {
  const listener = createTcpServer();
  await new Promise<void>((done) => listener.listen(0, "127.0.0.1", done));
  const address = listener.address();
  if (!address || typeof address === "string")
    throw new Error("TCP listener has no port");
  await new Promise<void>((done, reject) => listener.close((error) => error ? reject(error) : done()));
  return address.port;
}

function taskScopeToken(sessionId: string): string {
  return `v1.${Buffer.from(sessionId).toString("base64url")}`;
}

async function workspaceRpc(
  url: string, method: string, id: number | undefined, params: object,
  mcpSessionId?: string,
): Promise<{ value: Record<string, any>; mcpSessionId?: string }> {
  const response = await fetch(url, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      accept: "application/json, text/event-stream",
      "mcp-protocol-version": "2025-11-25",
      ...(mcpSessionId ? { "mcp-session-id": mcpSessionId } : {}),
    },
    body: JSON.stringify({ jsonrpc: "2.0", ...(id === undefined ? {} : { id }), method, params }),
    signal: AbortSignal.timeout(10_000),
  });
  if (!response.ok) throw new Error(`Workspace ${method}: HTTP ${response.status}`);
  if (id === undefined) return { value: {}, mcpSessionId: response.headers.get("mcp-session-id") ?? mcpSessionId };
  const body = await response.text();
  const data = response.headers.get("content-type")?.includes("text/event-stream")
    ? body.split("\n").filter((line) => line.startsWith("data:"))
        .map((line) => line.slice(5).trim()).find(Boolean)
    : body;
  if (!data) throw new Error(`Workspace ${method}: empty response (${response.headers.get("content-type")}; ${body.slice(0, 300)})`);
  const value = JSON.parse(data) as Record<string, any>;
  if (value.error) throw new Error(`Workspace ${method}: ${JSON.stringify(value.error)}`);
  return { value, mcpSessionId: response.headers.get("mcp-session-id") ?? mcpSessionId };
}

afterEach(async () => {
  for (const server of modelServers.splice(0)) {
    server.closeAllConnections();
    await new Promise<void>((done) => server.close(() => done()));
  }
  for (const root of temporaryRoots.splice(0))
    await rm(root, { recursive: true, force: true });
});

test("trusted settings prelude reaches ACP despite invalid disk settings", async () => {
  const f = await fixture();
  const transport = await start(f);
  try {
    const result = await initialize(transport);
    expect(result.protocolVersion).toBe(1);
    expect(result.agentCapabilities).toBeDefined();
  } finally {
    await transport.close();
  }
}, 20_000);

test("omitting settings lets Peri load its global model configuration", async () => {
  const f = await fixture();
  await writeFile(
    resolve(f.home, ".peri/settings.json"),
    JSON.stringify(f.settings),
  );
  const sandbox = new Sandbox({
    id: "global-settings",
    storage: new SqliteFileStorage({ path: f.database }),
    stdio: { command: periBinary, env: { HOME: f.home } },
  });
  const transport = await sandbox.createTransport(f.workspace);
  try {
    const result = await initialize(transport);
    expect(result.protocolVersion).toBe(1);
  } finally {
    await transport.close();
  }
}, 20_000);

test("a session can be listed and loaded by a fresh Peri process", async () => {
  const f = await fixture();
  const first = await start(f);
  const execution = new SessionExecution(first, { database: resolve(f.home, "execution", "registry.db") });
  first.setRequestHandler((method, params) => execution.handle(method, params));
  let sessionId: string;
  try {
    await initialize(first, true);
    const created = await first.request<{ sessionId: string }>("session/new", {
      cwd: f.workspace,
      mcpServers: [],
    });
    sessionId = created.sessionId;
    expect(sessionId).toBeTruthy();
    const listed = await first.request<{
      sessions: Array<{ sessionId: string }>;
    }>("session/list", { cwd: f.workspace });
    // Peri deliberately hides sessions with no user messages from session/list.
    expect(listed.sessions).toEqual([]);
    const queue = await first.request<{ generation: string }>("session/input/snapshot", { sessionId });
    const inputId = crypto.randomUUID();
    await first.request("session/input/enqueue", { sessionId, generation: queue.generation,
      commandId: `${inputId}:enqueue`, inputId, content: "Reply once", originalDraft: "Reply once" });
    expect(await execution.activate(sessionId, "send")).toEqual({ status: "idle" });
    const visible = await first.request<{
      sessions: Array<{ sessionId: string }>;
    }>("session/list", { cwd: f.workspace });
    expect(
      visible.sessions.some((session) => session.sessionId === sessionId),
      JSON.stringify(visible),
    ).toBe(true);
    const controls = new SessionControl();
    const { state } = await controls.snapshot(first, sessionId);
    const command = { sessionId, commandId: crypto.randomUUID(), expectedLifecycle: state.lifecycle,
      expectedRevision: state.revision, expectedControlGeneration: state.controlGeneration, action: { kind: "close" as const } };
    await drainClose(command, { apply: () => controls.apply(first, command), resolve: () => controls.resolve(first, command),
      snapshot: () => controls.snapshot(first, sessionId), isUnresolved: () => controls.isUnresolved(command.commandId) }, {});
  } finally {
    execution.beginRetirement();
    await first.close();
    await execution.joinOwnedCalls();
    await execution.recordStopped();
    execution.close();
  }

  const second = await start(f);
  try {
    await initialize(second);
    const listed = await second.request<{
      sessions: Array<{ sessionId: string }>;
    }>("session/list", { cwd: f.workspace });
    expect(
      listed.sessions.some((session) => session.sessionId === sessionId),
      JSON.stringify(listed),
    ).toBe(true);
    const loaded = await second.request<{
      modes?: object;
      configOptions?: unknown[];
    }>("session/load", { sessionId, cwd: f.workspace, mcpServers: [] });
    expect(loaded.modes).toBeDefined();
    expect(loaded.configOptions).toBeArray();
  } finally {
    await second.close();
  }
}, 20_000);

test("ManagedAgents starts one real Peri session and rejects a second owner", async () => {
  const f = await fixture();
  const sandbox = new Sandbox({
    id: "sandbox-workspace-identity",
    storage: new SqliteFileStorage({ path: f.database }),
    stdio: { command: periBinary, env: { HOME: f.home }, settings: f.settings },
  });
  const kv = new MemoryKV();
  const firstManager = new ManagedAgents({ kv });
  const secondManager = new ManagedAgents({ kv });
  const first = firstManager.createAgent({
    path: f.workspace,
    id: "one-agent",
    sandbox,
  });
  let secondTransportStarted = false;
  const secondSandbox = new Sandbox({
    id: sandbox.id,
    transportFactory: (path) => {
      secondTransportStarted = true;
      return sandbox.createTransport(path);
    },
  });
  const second = secondManager.createAgent({
    path: f.workspace,
    id: "one-agent",
    sandbox: secondSandbox,
  });
  let sessionId = "";
  try {
    const session = await first.session.start(null);
    sessionId = session.id;
    expect(sessionId).toBeTruthy();
    await expect(second.session.start(session.id)).rejects.toBeInstanceOf(
      AgentClaimConflictError,
    );
    expect(secondTransportStarted).toBe(false);
  } finally {
    await firstManager.closeAgent(first.id, await closeExpectation(first.session));
    await secondManager.cleanupStartupAgent(second.id);
  }
  const database = new Database(f.database, { readonly: true });
  try {
    const row = database
      .query("SELECT w.machine_id FROM threads t JOIN workspaces w ON w.id = t.workspace_id WHERE t.id = ?")
      .get(sessionId) as { machine_id: string } | null;
    // Current Peri creates its own machine UUID; ACP has no Sandbox identity input.
    expect(row?.machine_id).toMatch(/^[0-9a-f-]{36}$/);
    expect(row?.machine_id).not.toBe(sandbox.id);
  } finally {
    database.close();
  }
}, 20_000);

test("one Session delivers multiple inputs through real Peri without a turn wait", async () => {
  const f = await fixture();
  const sandbox = new Sandbox({
    id: "workspace-continuous",
    storage: new SqliteFileStorage({ path: f.database }),
    stdio: { command: periBinary, env: { HOME: f.home }, settings: f.settings },
  });
  const manager = new ManagedAgents({ kv: new MemoryKV() });
  const agent = manager.createAgent({
    path: f.workspace,
    id: "continuous-agent",
    sandbox,
  });
  try {
    const session = await agent.session.start(null);
    const first = session.send("First input");
    await first;
    expect(first.isSent).toBe(true);
    const second = session.send("Second input");
    await second;
    expect(second.isSent).toBe(true);
    const sessions = await agent.getSessions();
    expect(sessions.some((entry) => entry.id === session.id)).toBe(true);
  } finally {
    const command = await closeExpectation(agent.session);
    await manager.closeAgent(agent.id, command, { drainTimeoutMs: 3_000 });
    expect(agent.session.isClosed).toBe(true);
  }
}, 30_000);

test("ManagedAgents directly creates Agents and loads a historical Session", async () => {
    const f = await fixture();
    const executionDatabase = resolve(f.home, "execution", "registry.db");
  const sandbox = new Sandbox({
    id: "demo-history",
    storage: new SqliteFileStorage({ path: f.database }),
    stdio: { command: periBinary, env: { HOME: f.home }, settings: f.settings },
  });
  const kv = new MemoryKV();
  const firstManager = new ManagedAgents({ kv });
  const firstAgent = firstManager.createAgent({ path: f.workspace, id: "first-agent", sandbox, execution: { database: executionDatabase } });
  const created = await firstAgent.session.start(null);
  const sessionId = created.id;
  const delivery = created.send("Hello through the Agent");
  await delivery;
  expect(delivery.isSent).toBe(true);
  expect(delivery.inputId).toBeTruthy();
  const anotherAgent = firstManager.createAgent({ path: f.workspace, id: "another-agent", sandbox });
  const another = await anotherAgent.session.start(null);
  expect(another.id).not.toBe(sessionId);
  expect((await sandbox.getSessions(f.workspace)).some((entry) => entry.id === sessionId)).toBe(true);
  const conflictingManager = new ManagedAgents({ kv });
  const conflictingAgent = conflictingManager.createAgent({ path: f.workspace, id: "conflicting-agent", sandbox });
  try {
    await expect(conflictingAgent.session.start(sessionId)).rejects.toBeInstanceOf(AgentClaimConflictError);
    expect(firstAgent.session.id).toBe(sessionId);
  } finally {
    await conflictingManager.cleanupStartupAgent(conflictingAgent.id);
  }
  await firstManager.closeAll(new Map([
    [firstAgent.id, await closeExpectation(firstAgent.session)],
    [anotherAgent.id, await closeExpectation(anotherAgent.session)],
  ]));
  const executionRegistry = new SqliteExecutionRegistry({ path: executionDatabase });
  const retired = await executionRegistry.read(sessionId);
  expect(retired.instance?.status).toBe("stopped");
  expect(retired.instance?.proofRoute.dispatcherPid).toBe(process.pid);
  executionRegistry.close();

  const secondManager = new ManagedAgents({ kv });
  const loadedAgent = secondManager.createAgent({ id: "loaded-agent", sandbox, execution: { database: executionDatabase } });
  let historyError: unknown;
  try {
    const loaded = await loadedAgent.session.start(sessionId);
    expect(loaded.id).toBe(sessionId);
    const { state } = await loaded.controlState();
    expect((await loaded.reopen({ commandId: crypto.randomUUID(), expectedLifecycle: state.lifecycle, expectedRevision: state.revision, expectedControlGeneration: state.controlGeneration })).decision.kind).toBe("accepted");
    await loaded.send("Continue after verified dispatcher retirement");
    const resumedRegistry = new SqliteExecutionRegistry({ path: executionDatabase });
    const resumed = await resumedRegistry.read(sessionId);
    expect(resumed.instance?.generationId).not.toBe(retired.instance?.generationId);
    expect(resumed.previousInstances[0]?.status).toBe("stopped");
    expect(resumed.budgets.find((budget) => budget.workId === retired.budgets[0]?.workId)?.attempts).toBe(retired.budgets[0]?.attempts);
    resumedRegistry.close();
    expect(loadedAgent.id).not.toBe(firstAgent.id);
  } catch (error) {
    historyError = error;
    throw error;
  } finally {
    try { await secondManager.closeAgent(loadedAgent.id, await closeExpectation(loadedAgent.session)); }
    catch (cleanupError) {
      if (historyError) throw new AggregateError([historyError, cleanupError], "Historical Session execution and domain cleanup failed", { cause: historyError });
      throw cleanupError;
    }
  }
}, 20_000);

test("Sandbox Workspace MCP is discovered before the real ACP Session and outlives Agent closure", async () => {
  const f = await fixture();
  const bind = `127.0.0.1:${await unusedLoopbackPort()}`;
  const sandbox = new Sandbox({
    id: "managed-workspace",
    storage: new SqliteFileStorage({ path: f.database }),
    stdio: { command: periBinary, env: { HOME: f.home }, settings: f.settings },
    workspaceProcess: { command: periBinary, bind, env: { HOME: f.home } },
  });
  const manager = new ManagedAgents({ kv: new MemoryKV() });
  const agent = manager.createAgent({ path: f.workspace, id: "workspace-agent", sandbox });
  try {
    const session = await agent.session.start(null);
    expect(session.id).toBeTruthy();
    expect(sandbox.getWorkspace().url).toBe(`http://${bind}/mcp`);
    expect(agent.mcpServers()).toEqual([
      { name: "workspace", type: "http", url: `http://${bind}/mcp`, headers: [] },
    ]);
    await manager.closeAgent(agent.id, await closeExpectation(agent.session));
    const response = await fetch(sandbox.getWorkspace().url, {
      method: "POST",
      headers: { "content-type": "application/json", accept: "application/json, text/event-stream" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 10, method: "initialize", params: {
        protocolVersion: "2025-11-25", capabilities: {}, clientInfo: { name: "test", version: "1" },
      } }),
    });
    expect(response.status).toBe(200);
  } finally {
    await manager.closeAgent(agent.id, await closeExpectation(agent.session));
    await sandbox.closeWorkspace();
  }
}, 30_000);

test.each([
  { title: "legacy session without persisted owner binding has honest incomplete cold close and retains intent", missingBinding: true },
  { title: "fresh ACP cold close restores persisted owner binding and settles the actual resource scope", missingBinding: false },
])("$title", async ({ missingBinding }) => {
  const f = await fixture();
  const bind = `127.0.0.1:${await unusedLoopbackPort()}`;
  const sandbox = new Sandbox({
    id: "crash-close-workspace",
    storage: new SqliteFileStorage({ path: f.database }),
    stdio: { command: periBinary, env: { HOME: f.home }, settings: f.settings },
    workspaceProcess: { command: periBinary, bind, env: { HOME: f.home } },
  });
  const workspace = await sandbox.startWorkspace(f.workspace);
  const first = await sandbox.createTransport(f.workspace) as StdioTransport;
  let second: StdioTransport | undefined;
  try {
    await first.request("initialize", {
      protocolVersion: 1,
      clientCapabilities: { _meta: { "peri.userInputQueue": true } },
    });
    const { sessionId } = await first.request<{ sessionId: string }>("session/new", {
      cwd: f.workspace,
      mcpServers: [{ type: "http", name: "workspace", url: workspace.url, headers: [] }],
    });
    await sandbox.registerSessionTransport(sessionId, first);
    const capability = taskScopeToken(sessionId);
    const initialized = await workspaceRpc(workspace.url, "initialize", 1, {
      protocolVersion: "2025-11-25", capabilities: { extensions: { "io.modelcontextprotocol/tasks": {} } },
      clientInfo: { name: "crash-close-test", version: "1" },
    });
    const peer = initialized.mcpSessionId;
    await workspaceRpc(workspace.url, "notifications/initialized", undefined, {}, peer);
    const started = await workspaceRpc(workspace.url, "tools/call", 2, {
      name: "Bash", arguments: { command: "sleep 120", run_in_background: true },
      _meta: { "peri/taskScope": capability },
    }, peer);
    const rawTaskId = started.value.result?.taskId as string | undefined;
    if (!rawTaskId) throw new Error(`Workspace Bash did not return Task: ${JSON.stringify(started.value)}`);

    const intentDb = new Database(f.database);
    const intent = intentDb.query(
      "INSERT INTO session_close_intents(thread_id, requested_at) VALUES (?, ?)",
    ).run(sessionId, new Date().toISOString());
    intentDb.close();
    expect(intent.changes).toBe(1);
    process.kill(first.pid, "SIGKILL");
    expect(await first.waitForTerminationProof()).toBe(true);
    if (missingBinding) {
      const legacy = new Database(f.database);
      expect(legacy.query("UPDATE session_work_state SET state_json=json_set(state_json,'$.resourceOwners',json('{}')) WHERE session_id=?")
        .run(sessionId).changes).toBe(1);
      legacy.close();
    }

    const before = await workspaceRpc(workspace.url, "workspace/taskSnapshot", 3, {
      _meta: { "peri/taskScope": capability },
    }, peer);
    const beforeTask = (before.value.result?.tasks as Array<{ task?: { taskId?: string; status?: string } }>)
      .find((row) => row.task?.taskId === rawTaskId);
    expect(beforeTask?.task?.status).toBe("working");

    second = await sandbox.createTransport(f.workspace) as StdioTransport;
    await initialize(second);
    if (missingBinding) {
      await expect(second.request("session/close", { sessionId })).rejects.toMatchObject({ code: -32010 });
    } else {
      const controls = new SessionControl();
      const { state } = await controls.snapshot(second, sessionId);
      const command = { sessionId, commandId: crypto.randomUUID(), expectedLifecycle: state.lifecycle,
        expectedRevision: state.revision, expectedControlGeneration: state.controlGeneration, action: { kind: "close" as const } };
      await drainClose(command, { apply: () => controls.apply(second!, command), resolve: () => controls.resolve(second!, command),
        snapshot: () => controls.snapshot(second!, sessionId), isUnresolved: () => controls.isUnresolved(command.commandId) }, {});
      expect(await controls.snapshot(second, sessionId)).toMatchObject({ state: { status: "closed" }, settlement: { status: "settled" } });
    }
    const settled = new Database(f.database, { readonly: true });
    const remainingIntent = settled.query("SELECT 1 FROM session_close_intents WHERE thread_id = ?")
      .get(sessionId);
    settled.close();
    if (missingBinding) expect(remainingIntent).not.toBeNull();
    else expect(remainingIntent).toBeNull();
    const snapshot = await workspaceRpc(workspace.url, "workspace/taskSnapshot", 4, {
      _meta: { "peri/taskScope": taskScopeToken(sessionId) },
    }, peer);
    const task = (snapshot.value.result?.tasks as Array<{ task?: { taskId?: string; status?: string } }>)
      .find((row) => row.task?.taskId === rawTaskId);
    expect(task).toBeDefined();
    if (missingBinding) expect(task?.task?.status).toBe("working");
    else expect(["completed", "failed", "cancelled"]).toContain(task?.task?.status ?? "missing");
  } finally {
    await second?.close().catch(() => {});
    await first.close().catch(() => {});
    await sandbox.closeWorkspace();
  }
}, 90_000);
