import { expect, test } from "bun:test";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { createServer, connect } from "node:net";
import { once } from "node:events";
import {
  AgentClaimConflictError, ManagedAgents, MemoryKV, Sandbox, TursoStorage, startPeriWasmHost,
} from "../dist/index.js";

async function unusedPort(): Promise<number> {
  const listener = createServer();
  listener.listen(0, "127.0.0.1");
  await once(listener, "listening");
  const address = listener.address();
  if (!address || typeof address === "string") throw new Error("no TCP port");
  await new Promise<void>((done) => listener.close(done));
  return address.port;
}

async function waitForSqld(port: number, child: Bun.Subprocess): Promise<void> {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (child.exitCode !== null) throw new Error(`sqld exited (${child.exitCode})`);
    try {
      const socket = connect(port, "127.0.0.1");
      await once(socket, "connect");
      socket.destroy();
      return;
    } catch {
      await Bun.sleep(50);
    }
  }
  throw new Error("sqld did not listen within five seconds");
}

async function settled(model: () => number, expected: number, budgetMs = 20_000): Promise<void> {
  const deadline = Date.now() + budgetMs;
  while (model() < expected && Date.now() < deadline) await Bun.sleep(50);
  expect(model(), "the fixture model endpoint was not reached in time").toBeGreaterThanOrEqual(expected);
}

/**
 * 真实链路：打包的 peri-wasm 产物 + 真实 Turso listener + 模拟模型端点。
 * SDK 不启动任何 Peri 或 Workspace 子进程；每个 Session 由 `transportFactory` 装配独立 WASM 实例。
 */
async function fixture() {
  const root = await mkdtemp(resolve(tmpdir(), "peri-sdk-wasm-acp-"));
  const sqlPort = await unusedPort();
  const sqld = Bun.spawn([
    "mise", "exec", "--", "sqld", "--no-welcome", "--http-listen-addr",
    `127.0.0.1:${sqlPort}`, "--db-path", resolve(root, "sessions"),
  ], { cwd: resolve(import.meta.dir, ".."), stdin: "ignore", stdout: "ignore", stderr: "pipe" });
  const sqldLog = new Response(sqld.stderr).text();
  const prompts: string[] = [];
  const telemetry: unknown[] = [];
  const langfuse = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    expect(new URL(request.url).pathname).toBe("/api/public/otel/v1/traces");
    expect(request.headers.get("authorization")).toBe(`Basic ${btoa("pk-test:sk-test")}`);
    expect(request.headers.get("x-langfuse-ingestion-version")).toBe("4");
    telemetry.push(await request.json());
    return Response.json({});
  } });
  const model = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    const body = await request.json() as { messages?: unknown };
    expect(request.headers.get("authorization")).toBe("Bearer fixture-key");
    prompts.push(JSON.stringify(body.messages));
    return new Response(
      'data: {"choices":[{"index":0,"delta":{"content":"WASM_SDK_REPLY"},"finish_reason":null}]}\n\n' +
      'data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\n' +
      "data: [DONE]\n\n",
      { headers: { "content-type": "text/event-stream" } },
    );
  } });
  try { await waitForSqld(sqlPort, sqld); }
  catch (error) { model.stop(true); langfuse.stop(true); await rm(root, { recursive: true, force: true }); throw error; }
  // ACP cwd 必须与 Agent path 一致；macOS 的 /var 是指向 /private/var 的符号链接。
  const workspace = await realpath(root);
  const moduleUrl = resolve(import.meta.dir, "../dist/wasm/peri-wasm.js");
  const databaseUrl = `http://127.0.0.1:${sqlPort}`;
  const settings = {
    config: {
      active_alias: "sonnet",
      providers: [{ id: "fixture", type: "openai", apiKey: "fixture-key", baseUrl: `http://127.0.0.1:${model.port}/v1` }],
      profiles: { sonnet: { provider: "fixture", model: "fixture-model" } },
    },
  };
  const storage = new TursoStorage({ url: databaseUrl, authToken: "local-dev" });
  const sandboxFor = (id: string) => new Sandbox({
    id,
    storage,
    transportFactory: (path) => startPeriWasmHost({
      moduleUrl,
      env: {
        LANGFUSE_PUBLIC_KEY: "pk-test",
        LANGFUSE_SECRET_KEY: "sk-test",
        LANGFUSE_BASE_URL: `http://127.0.0.1:${langfuse.port}`,
        LANGFUSE_USER_ID: "wasm-test-user",
      },
      configJson: JSON.stringify({
        cwd: path, settings,
        storage: { url: databaseUrl, authToken: "local-dev" },
        machineId: "00000000-0000-4000-8000-000000000003",
      }),
    }),
  });
  const dispose = async (primaryError?: unknown, cleanupErrors: unknown[] = []) => {
    model.stop(true);
    langfuse.stop(true);
    sqld.kill("SIGTERM");
    await sqld.exited;
    const serverLog = await sqldLog;
    if (primaryError && serverLog) console.error(serverLog);
    await rm(root, { recursive: true, force: true });
    if (cleanupErrors.length) throw new AggregateError(primaryError ? [primaryError, ...cleanupErrors] : cleanupErrors,
      "WASM integration domain or startup cleanup failed", { cause: primaryError });
    if (primaryError) throw primaryError;
  };
  return { workspace, sandboxFor, storage, prompts: () => prompts.length, telemetry, dispose };
}

test("start, send two inputs, cancel, list, close and reload run through the bundled WASM ACP Host", async () => {
  const f = await fixture();
  const manager = new ManagedAgents({ kv: new MemoryKV() });
  const agents: Array<ReturnType<typeof manager.createAgent>> = [];
  const started = new Set<string>();
  let primaryError: unknown;
  let sessionId = "";
  try {
    const sandbox = f.sandboxFor("wasm-acp-integration");
    const createdAgent = manager.createAgent({ path: f.workspace, id: "created", sandbox });
    agents.push(createdAgent);
    const created = await createdAgent.session.start(null);
    started.add(createdAgent.id);
    sessionId = created.id;
    expect(sessionId).toBeTruthy();

    const receipt = created.send("Reply once");
    await receipt;
    expect(receipt.isSent).toBe(true);
    await settled(f.prompts, 1);
    expect(f.prompts()).toBeGreaterThanOrEqual(1);

    const second = created.send("Reply twice");
    await second;
    expect(second.isSent).toBe(true);

    await created.cancel();
    await manager.closeAgent(createdAgent.id, { drainTimeoutMs: 20_000 });
    expect(createdAgent.session.isClosed).toBe(true);
    expect(f.telemetry.length).toBeGreaterThan(0);
    expect(JSON.stringify(f.telemetry)).toContain("resourceSpans");
    expect((await sandbox.getSessions(f.workspace)).some((entry) => entry.id === sessionId)).toBe(true);

    // 新 Agent 用新的 WASM 实例加载同一会话：身份保持，历史回放进入文档投影。
    const loadedAgent = manager.createAgent({ id: "loaded", sandbox });
    agents.push(loadedAgent);
    const loaded = await loadedAgent.session.start(sessionId);
    started.add(loadedAgent.id);
    expect(loaded.id).toBe(sessionId);
    const transcript = JSON.stringify(loaded.docs.chat.toJSON());
    expect(transcript).toContain("Reply once");
    expect(transcript).toContain("WASM_SDK_REPLY");
  } catch (error) {
    primaryError = error;
    throw error;
  } finally {
    const cleanupErrors: unknown[] = [];
    for (const agent of agents) {
      if (agent.session.isClosed) continue;
      try {
        if (started.has(agent.id)) await manager.closeAgent(agent.id, { drainTimeoutMs: 20_000 });
        else await agent.session.cleanupStartup();
      } catch (error) { cleanupErrors.push(error); }
    }
    await f.dispose(primaryError, cleanupErrors);
  }
}, 120_000);

test("a second ManagedAgents owner cannot claim the same real Session and never starts its transport", async () => {
  const f = await fixture();
  const kv = new MemoryKV();
  const firstManager = new ManagedAgents({ kv });
  const secondManager = new ManagedAgents({ kv });
  const sandbox = f.sandboxFor("wasm-acp-claims");
  const first = firstManager.createAgent({ path: f.workspace, id: "one-agent", sandbox });
  let secondTransportStarted = false;
  const secondSandbox = new Sandbox({
    id: sandbox.id,
    storage: f.storage,
    transportFactory: (path) => {
      secondTransportStarted = true;
      return sandbox.createTransport(path);
    },
  });
  const second = secondManager.createAgent({ path: f.workspace, id: "one-agent", sandbox: secondSandbox });
  let primaryError: unknown;
  try {
    const session = await first.session.start(null);
    expect(session.id).toBeTruthy();
    await expect(second.session.start(session.id)).rejects.toBeInstanceOf(AgentClaimConflictError);
    expect(secondTransportStarted).toBe(false);
  } catch (error) {
    primaryError = error;
    throw error;
  } finally {
    const cleanupErrors: unknown[] = [];
    try { await firstManager.closeAgent(first.id, { drainTimeoutMs: 20_000 }); }
    catch (error) { cleanupErrors.push(error); }
    try { await secondManager.cleanupStartupAgent(second.id); }
    catch (error) { cleanupErrors.push(error); }
    await f.dispose(primaryError, cleanupErrors);
  }
}, 120_000);
