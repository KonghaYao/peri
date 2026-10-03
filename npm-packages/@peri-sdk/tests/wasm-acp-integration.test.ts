import { expect, test } from "bun:test";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { createServer, connect } from "node:net";
import { once } from "node:events";
import {
  ManagedAgents, MemoryKV, Sandbox, TursoStorage, WasmAcpTransport,
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

/** Full SDK route through the bundled WASM ACP Host, a real Turso listener and a simulated model endpoint. */
test("Agent start, send, list and load work through WASM ACP", async () => {
  const root = await mkdtemp(resolve(tmpdir(), "peri-sdk-wasm-acp-"));
  const sqlPort = await unusedPort();
  const sqld = Bun.spawn([
    "mise", "exec", "--", "sqld", "--no-welcome", "--http-listen-addr",
    `127.0.0.1:${sqlPort}`, "--db-path", resolve(root, "sessions"),
  ], { cwd: resolve(import.meta.dir, ".."), stdin: "ignore", stdout: "ignore", stderr: "pipe" });
  let modelCalls = 0;
  const model = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    const body = await request.json() as { messages?: unknown };
    expect(request.headers.get("authorization")).toBe("Bearer fixture-key");
    expect(JSON.stringify(body.messages)).toContain("Reply once");
    modelCalls++;
    return new Response(
      'data: {"choices":[{"index":0,"delta":{"content":"WASM_SDK_REPLY"},"finish_reason":null}]}\n\n' +
      'data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\n' +
      "data: [DONE]\n\n",
      { headers: { "content-type": "text/event-stream" } },
    );
  } });
  // Keep the ACP cwd identical to Sandbox.path on hosts with /var -> /private/var.
  const workspace = await realpath(root);
  const databaseUrl = `http://127.0.0.1:${sqlPort}`;
  const settings = {
    config: {
      active_alias: "sonnet",
      providers: [{ id: "fixture", type: "openai", apiKey: "fixture-key", baseUrl: `http://127.0.0.1:${model.port}/v1` }],
      profiles: { sonnet: { provider: "fixture", model: "fixture-model" } },
    },
  };
  const sandbox = new Sandbox({
    id: "wasm-acp-integration", path: workspace,
    storage: new TursoStorage({ url: databaseUrl, engine: "libsql", authToken: "local-dev" }),
    transportFactory: () => WasmAcpTransport.start({ configJson: JSON.stringify({
      cwd: workspace, settings, storage: { url: databaseUrl, authToken: "local-dev" },
      machineId: "00000000-0000-4000-8000-000000000003",
    }) }),
  });
  const manager = new ManagedAgents({ kv: new MemoryKV() });
  try {
    await waitForSqld(sqlPort, sqld);
    const createdAgent = manager.createAgent({ id: "created", sandbox });
    const created = await createdAgent.session.start(null);
    const sessionId = created.id;
    const receipt = created.send("Reply once");
    await receipt;
    expect(receipt.isSent).toBe(true);
    for (let attempt = 0; attempt < 200 && modelCalls === 0; attempt++) await Bun.sleep(50);
    expect(modelCalls).toBe(1);
    expect((await sandbox.getSessions()).some((entry) => entry.id === sessionId)).toBe(true);
    await manager.closeAgent(createdAgent.id);

    const loadedAgent = manager.createAgent({ id: "loaded", sandbox });
    const loaded = await loadedAgent.session.start(sessionId);
    expect(loaded.id).toBe(sessionId);
  } finally {
    await manager.closeAll();
    model.stop(true);
    sqld.kill("SIGTERM");
    await sqld.exited;
    await rm(root, { recursive: true, force: true });
  }
}, 90_000);
