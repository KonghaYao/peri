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

const periBinary = resolve(import.meta.dir, "../../../target/debug/peri");
const temporaryRoots: string[] = [];
const modelServers: Server[] = [];

type Fixture = {
  home: string;
  workspace: string;
  database: string;
  settings: object;
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

async function initialize(transport: Transport) {
  return transport.request<{
    protocolVersion: number;
    agentCapabilities: object;
  }>("initialize", { protocolVersion: 1 });
}

async function unusedLoopbackPort(): Promise<number> {
  const listener = createTcpServer();
  await new Promise<void>((done) => listener.listen(0, "127.0.0.1", done));
  const address = listener.address();
  if (!address || typeof address === "string")
    throw new Error("TCP listener has no port");
  await new Promise<void>((done) => listener.close(done));
  return address.port;
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
    path: f.workspace,
    storage: new SqliteFileStorage({ path: f.database }),
    stdio: { command: periBinary, env: { HOME: f.home } },
  });
  const transport = await sandbox.createTransport();
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
  let sessionId: string;
  try {
    await initialize(first);
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
    const completed = await first.request<{ stopReason: string }>(
      "session/prompt",
      {
        sessionId,
        prompt: [{ type: "text", text: "Reply once" }],
      },
    );
    expect(completed.stopReason).toBe("end_turn");
    const visible = await first.request<{
      sessions: Array<{ sessionId: string }>;
    }>("session/list", { cwd: f.workspace });
    expect(
      visible.sessions.some((session) => session.sessionId === sessionId),
      JSON.stringify(visible),
    ).toBe(true);
  } finally {
    await first.close();
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
    path: f.workspace,
    storage: new SqliteFileStorage({ path: f.database }),
    stdio: { command: periBinary, env: { HOME: f.home }, settings: f.settings },
  });
  const kv = new MemoryKV();
  const firstManager = new ManagedAgents({ kv });
  const secondManager = new ManagedAgents({ kv });
  const first = firstManager.createAgent({
    id: "one-agent",
    sandbox,
  });
  let secondTransportStarted = false;
  const secondSandbox = new Sandbox({
    id: sandbox.id,
    path: f.workspace,
    transportFactory: () => {
      secondTransportStarted = true;
      return sandbox.createTransport();
    },
  });
  const second = secondManager.createAgent({
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
    await firstManager.closeAgent(first.id);
    await secondManager.closeAgent(second.id);
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
    path: f.workspace,
    storage: new SqliteFileStorage({ path: f.database }),
    stdio: { command: periBinary, env: { HOME: f.home }, settings: f.settings },
  });
  const manager = new ManagedAgents({ kv: new MemoryKV() });
  const agent = manager.createAgent({
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
    await manager.closeAgent(agent.id);
  }
}, 30_000);

test("ManagedAgents directly creates Agents and loads a historical Session", async () => {
  const f = await fixture();
  const sandbox = new Sandbox({
    id: "demo-history",
    path: f.workspace,
    storage: new SqliteFileStorage({ path: f.database }),
    stdio: { command: periBinary, env: { HOME: f.home }, settings: f.settings },
  });
  const kv = new MemoryKV();
  const firstManager = new ManagedAgents({ kv });
  const firstAgent = firstManager.createAgent({ id: "first-agent", sandbox });
  const created = await firstAgent.session.start(null);
  const sessionId = created.id;
  const delivery = created.send("Hello through the Agent");
  await delivery;
  expect(delivery.isSent).toBe(true);
  expect(delivery.inputId).toBeTruthy();
  const anotherAgent = firstManager.createAgent({ id: "another-agent", sandbox });
  const another = await anotherAgent.session.start(null);
  expect(another.id).not.toBe(sessionId);
  expect((await sandbox.getSessions()).some((entry) => entry.id === sessionId)).toBe(true);
  const conflictingManager = new ManagedAgents({ kv });
  try {
    const conflictingAgent = conflictingManager.createAgent({ id: "conflicting-agent", sandbox });
    await expect(conflictingAgent.session.start(sessionId)).rejects.toBeInstanceOf(AgentClaimConflictError);
    expect(firstAgent.session.id).toBe(sessionId);
  } finally {
    await conflictingManager.closeAll();
  }
  await firstManager.closeAll();

  const secondManager = new ManagedAgents({ kv });
  try {
    const loadedAgent = secondManager.createAgent({ id: "loaded-agent", sandbox });
    const loaded = await loadedAgent.session.start(sessionId);
    expect(loaded.id).toBe(sessionId);
    expect(loadedAgent.id).not.toBe(firstAgent.id);
  } finally {
    await secondManager.closeAll();
  }
}, 20_000);

test("Sandbox Workspace MCP is discovered before the real ACP Session and outlives Agent closure", async () => {
  const f = await fixture();
  const bind = `127.0.0.1:${await unusedLoopbackPort()}`;
  const sandbox = new Sandbox({
    id: "managed-workspace",
    path: f.workspace,
    storage: new SqliteFileStorage({ path: f.database }),
    stdio: { command: periBinary, env: { HOME: f.home }, settings: f.settings },
    workspaceProcess: { command: periBinary, bind, env: { HOME: f.home } },
  });
  const manager = new ManagedAgents({ kv: new MemoryKV() });
  const agent = manager.createAgent({ id: "workspace-agent", sandbox });
  try {
    const session = await agent.session.start(null);
    expect(session.id).toBeTruthy();
    expect(sandbox.getWorkspace().url).toBe(`http://${bind}/mcp`);
    expect(agent.mcpServers()).toEqual([
      { name: "workspace", type: "http", url: `http://${bind}/mcp`, headers: [] },
    ]);
    await manager.closeAgent(agent.id);
    const response = await fetch(sandbox.getWorkspace().url, {
      method: "POST",
      headers: { "content-type": "application/json", accept: "application/json, text/event-stream" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 10, method: "initialize", params: {
        protocolVersion: "2025-11-25", capabilities: {}, clientInfo: { name: "test", version: "1" },
      } }),
    });
    expect(response.status).toBe(200);
  } finally {
    await manager.closeAgent(agent.id);
    await sandbox.closeWorkspace();
  }
}, 30_000);
