import { expect, test } from "bun:test";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { createServer, connect } from "node:net";
import { once } from "node:events";
import {
  ChatManager,
  ChatUserAction,
  SessionBindingConflictError,
  TursoStorage,
  startWasmRuntime,
} from "../dist/index.js";
import { ChatDocument } from "../src/view/chat-document";
import { readSessionView } from "../src/view/session-view";
import { OwnershipProbe } from "./ownership-probe";

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

async function until(predicate: () => boolean, budgetMs = 20_000): Promise<void> {
  const deadline = Date.now() + budgetMs;
  while (!predicate() && Date.now() < deadline) await Bun.sleep(50);
  expect(predicate(), "the expected document state was not reached in time").toBe(true);
}

/** 正文按 magic 判压缩：遥测默认 gzip，两种情况都解出 OTLP JSON（同 langfuse-client 的 batcher_concurrency_test.rs）。 */
function otlpBody(bytes: Uint8Array): unknown {
  const raw = bytes[0] === 0x1f && bytes[1] === 0x8b ? Bun.gunzipSync(bytes) : bytes;
  return JSON.parse(new TextDecoder().decode(raw));
}

/** Chat 事实 → 纯展示文档；测试不直接读运行侧内部状态。 */

/**
 * 真实链路：打包的 peri-wasm 产物 + 真实 Turso listener + 模拟模型端点。
 * ChatManager 按激活装配独立 WASM 宿主；SDK 不启动任何 Peri 或 Workspace 子进程。
 */
async function fixture() {
  const root = await mkdtemp(resolve(tmpdir(), "peri-sdk-wasm-chat-"));
  const sqlPort = await unusedPort();
  // 本地走 SDK 的 mise 工具；CI 用 PERI_TEST_SQLD_BIN 指向预下载的 Linux sqld。
  const sqldCommand = process.env.PERI_TEST_SQLD_BIN
    ? [process.env.PERI_TEST_SQLD_BIN]
    : ["mise", "exec", "--", "sqld"];
  const sqld = Bun.spawn([
    ...sqldCommand, "--no-welcome", "--http-listen-addr",
    `127.0.0.1:${sqlPort}`, "--db-path", resolve(root, "sessions"),
  ], { cwd: resolve(import.meta.dir, ".."), stdin: "ignore", stdout: "ignore", stderr: "pipe" });
  const sqldLog = new Response(sqld.stderr).text();
  const prompts: string[] = [];
  const telemetry: unknown[] = [];
  const langfuse = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    expect(new URL(request.url).pathname).toBe("/api/public/otel/v1/traces");
    expect(request.headers.get("authorization")).toBe(`Basic ${btoa("pk-test:sk-test")}`);
    expect(request.headers.get("x-langfuse-ingestion-version")).toBe("4");
    telemetry.push(otlpBody(new Uint8Array(await request.arrayBuffer())));
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
  // ACP cwd 必须与 Chat path 一致；macOS 的 /var 是指向 /private/var 的符号链接。
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
  const agentConfig = {
    cwd: workspace,
    settings,
    storage: { url: databaseUrl, authToken: "local-dev" },
    machineId: "00000000-0000-4000-8000-000000000003",
  };
  const wasmRuntime = {
    kind: "wasm" as const,
    moduleUrl,
    env: {
      LANGFUSE_PUBLIC_KEY: "pk-test",
      LANGFUSE_SECRET_KEY: "sk-test",
      LANGFUSE_BASE_URL: `http://127.0.0.1:${langfuse.port}`,
      LANGFUSE_USER_ID: "wasm-test-user",
    },
  };
  const managerFor = (storagePort: OwnershipProbe): ChatManager => new ChatManager({
    agentConfig,
    database: storage,
    kv: storagePort,
    chatConfig: {
      runtime: wasmRuntime,
      sessionInit: { path: workspace },
      lifecycle: { activationCloseTimeoutMs: 20_000, closeDrainTimeoutMs: 20_000 },
    },
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
  return { workspace, storage, managerFor, prompts: () => prompts.length, telemetry, dispose };
}

test("create, send two inputs, cancel, close and reload run through the bundled WASM ACP Host", async () => {
  const f = await fixture();
  const manager = f.managerFor(new OwnershipProbe());
  let primaryError: unknown;
  try {
    const chat = manager.createChat({ path: f.workspace });
    const display = new ChatDocument();
    chat.subscribeFacts({
      onBinding(_target, replay) { display.applyFacts(replay); },
      onFact(envelope) { display.applyEnvelope(envelope); },
      onClosed() {},
    });
    const ready = await chat.execute(ChatUserAction.createNewSession({}));
    const sessionId = ready.sessionId;
    expect(sessionId).toBeTruthy();

    const receipt = chat.execute(ChatUserAction.send({ text: "Reply once" }));
    await receipt;
    expect(receipt.isSent).toBe(true);
    await settled(f.prompts, 1);
    expect(f.prompts()).toBeGreaterThanOrEqual(1);

    const second = chat.execute(ChatUserAction.send({ text: "Reply twice" }));
    await second;
    expect(second.isSent).toBe(true);

    const cancelled = await chat.execute(ChatUserAction.cancel({}));
    expect(["requested", "idle"]).toContain(cancelled.status);
    await chat.close();
    expect(f.telemetry.length).toBeGreaterThan(0);
    expect(JSON.stringify(f.telemetry)).toContain("resourceSpans");
    expect((await f.storage.listSessions(f.workspace)).some((entry) => entry.sessionId === sessionId)).toBe(true);

    // 新 Chat 用新的 WASM 实例加载同一会话：身份保持，历史回放进入展示文档。
    const loadedChat = manager.createChat({ path: f.workspace });
    const loaded = new ChatDocument();
    loadedChat.subscribeFacts({
      onBinding(_target, replay) { loaded.applyFacts(replay); },
      onFact(envelope) { loaded.applyEnvelope(envelope); },
      onClosed() {},
    });
    const changed = await loadedChat.execute(ChatUserAction.changeSessionTo(sessionId));
    expect(changed.sessionId).toBe(sessionId);
    await until(() => JSON.stringify(loaded.chat.toJSON()).includes("Reply once"));
    const transcript = JSON.stringify(loaded.chat.toJSON());
    expect(transcript).toContain("Reply once");
    expect(transcript).toContain("WASM_SDK_REPLY");
    expect(readSessionView(loaded.chat, loaded.session).entries.length).toBeGreaterThan(0);
    await loadedChat.close();
  } catch (error) {
    primaryError = error;
    throw error;
  } finally {
    const cleanupErrors: unknown[] = [];
    try { await manager.shutdown(); } catch (error) { cleanupErrors.push(error); }
    await f.dispose(primaryError, cleanupErrors);
  }
}, 120_000);

test("a second manager cannot own the same real Session and never starts its transport", async () => {
  const f = await fixture();
  const kv = new OwnershipProbe();
  const firstManager = f.managerFor(kv);
  let secondTransportStarted = false;
  let primaryError: unknown;
  try {
    const first = firstManager.createChat({ path: f.workspace });
    const session = await first.execute(ChatUserAction.createNewSession({}));

    // 第二管理器只提供会记录启动的 transport 运行后端；ownership 必须先于激活。
    const transportManager = new ChatManager({
      agentConfig: { cwd: f.workspace, settings: {}, storage: { url: "http://127.0.0.1:1", authToken: "x" }, machineId: "m" },
      database: f.storage,
      kv: kv,
      chatConfig: {
        runtime: {
          kind: "transport",
          factory: () => {
            secondTransportStarted = true;
            throw new Error("transport must not start before ownership acquisition succeeds");
          },
        },
        sessionInit: { path: f.workspace },
      },
    });
    const second = transportManager.createChat({ path: f.workspace });
    await expect(second.execute(ChatUserAction.changeSessionTo(session.sessionId)))
      .rejects.toBeInstanceOf(SessionBindingConflictError);
    expect(secondTransportStarted).toBe(false);

    await first.close();
    await transportManager.shutdown();
  } catch (error) {
    primaryError = error;
    throw error;
  } finally {
    const cleanupErrors: unknown[] = [];
    try { await firstManager.shutdown(); } catch (error) { cleanupErrors.push(error); }
    await f.dispose(primaryError, cleanupErrors);
  }
}, 120_000);
