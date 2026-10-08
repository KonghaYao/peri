import { once } from "node:events";
import { SessionDocReplica } from "@peri-code/sdk/view";
import { encodeAuthFrame, encodeAckFrame, decodeSyncFrame } from "../shared/sync.ts";
import { readSyncState } from "../shared/sync-state.ts";
import { createServer, connect } from "node:net";
import { mkdtemp, open, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const scratch = await mkdtemp(resolve(tmpdir(), "peri-cf-smoke-"));
const config = resolve(root, "dist/peri_cf/wrangler.json");
const cli = resolve(root, "node_modules/wrangler/bin/wrangler.js");
const token = "local-smoke-only-no-production-access";
const logPath = resolve(scratch, "worker.log");
const log = await open(logPath, "w");
let database;
let worker;
let model;
let modelCalls = 0;
let predictionCalls = 0;
const modelRequests = [];
const syncConnections = new Set();

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

async function connectSync(base, id) {
  const replica = new SessionDocReplica();
  const socket = new WebSocket(`${base.replace(/^http/, "ws")}/api/chats/${id}/sync`);
  let current;
  let failure;
  const waiters = new Set();
  const fail = (error) => {
    failure = error;
    for (const waiter of waiters) waiter.reject(error);
    waiters.clear();
  };
  socket.addEventListener("open", () => socket.send(encodeAuthFrame({ type: "auth", token })));
  socket.addEventListener("message", ({ data }) => {
    try {
      const frame = decodeSyncFrame(data);
      if (frame.type === "snapshot") replica.applySnapshot(frame.snapshot);
      else replica.applyUpdate(frame.update);
      current = readSyncState(replica.chat, replica.session);
      if (frame.delivery !== undefined) socket.send(encodeAckFrame(frame.delivery));
      for (const waiter of [...waiters]) {
        if (waiter.predicate(current)) { waiters.delete(waiter); waiter.resolve(current); }
      }
    } catch (error) { fail(error); socket.close(); }
  });
  socket.addEventListener("error", () => fail(new Error("Smoke document sync failed")));
  socket.addEventListener("close", ({ code }) => fail(new Error(`Smoke document sync closed (${code})`)));
  const connection = {
    wait(predicate, timeout = 45_000) {
      if (failure) return Promise.reject(failure);
      if (current && predicate(current)) return Promise.resolve(current);
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          waiters.delete(waiter);
          reject(new Error(`Smoke document sync timed out: ${JSON.stringify(current)}`));
        }, timeout);
        const waiter = { predicate,
          resolve(value) { clearTimeout(timer); resolve(value); },
          reject(error) { clearTimeout(timer); reject(error); },
        };
        waiters.add(waiter);
      });
    },
    close() { socket.close(); replica.destroy(); syncConnections.delete(connection); },
  };
  syncConnections.add(connection);
  await connection.wait(() => true);
  return connection;
}

async function freePort() {
  const server = createServer();
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const port = server.address().port;
  await new Promise((done) => server.close(done));
  return port;
}

async function stop(process) {
  if (!process) return;
  process.kill("SIGINT");
  await process.exited;
}

async function waitForPort(port, process) {
  for (let attempt = 0; attempt < 150; attempt++) {
    assert(process.exitCode === null, `Process exited before listening: ${process.exitCode}`);
    try {
      const socket = connect(port, "127.0.0.1");
      await once(socket, "connect");
      socket.destroy();
      return;
    } catch {}
    await Bun.sleep(100);
  }
  throw new Error(`Port ${port} did not become ready`);
}

function wrangler(args) {
  return Bun.spawn(["node", cli, ...args], {
    cwd: root, stdout: log.fd, stderr: log.fd,
    env: { ...process.env, CI: "true", WRANGLER_SEND_METRICS: "false" },
  });
}

try {
  assert(await Bun.file(config).exists(), "Run bun run build first");
  const sqlPort = await freePort();
  database = Bun.spawn([
    process.env.SQLD_BIN || "sqld", "--no-welcome", "--http-listen-addr",
    `127.0.0.1:${sqlPort}`, "--db-path", resolve(scratch, "rust-store"),
  ], { stdout: log.fd, stderr: log.fd });
  await waitForPort(sqlPort, database);

  model = Bun.serve({ hostname: "127.0.0.1", port: 0, idleTimeout: 60, async fetch(request) {
    const body = await request.json();
    modelRequests.push(body.messages);
    assert(new URL(request.url).pathname === "/v1/chat/completions", "Unexpected model route");
    assert(request.headers.get("Authorization") === "Bearer fixture-key", "Model key did not reach provider");
    const prediction = body.messages?.some((message) => message.role === "system"
      && typeof message.content === "string" && message.content.startsWith("<prediction_directive>"));
    if (prediction) predictionCalls++;
    else modelCalls++;
    if (!prediction && modelCalls > 1)
      assert(JSON.stringify(body).includes("LOCAL_WASM_REPLY_1"), "ACP load lost prior model context");
    if (!prediction && body.messages?.at(-1)?.content === "Wait for cancellation") {
      let timer;
      const encoder = new TextEncoder();
      return new Response(new ReadableStream({
        start(controller) {
          const chunk = { choices: [{ index: 0, delta: { content: "LOCAL_CANCEL_PARTIAL" }, finish_reason: null }] };
          controller.enqueue(encoder.encode(`data: ${JSON.stringify(chunk)}\n\n`));
          timer = setTimeout(() => {
            controller.enqueue(encoder.encode("data: [DONE]\n\n"));
            controller.close();
          }, 30_000);
        },
        cancel() { clearTimeout(timer); },
      }), { headers: { "Content-Type": "text/event-stream" } });
    }
    const content = prediction ? "Fixture next input" : `LOCAL_WASM_REPLY_${modelCalls}`;
    return new Response(
      `data: ${JSON.stringify({ choices: [{ index: 0, delta: { content }, finish_reason: null }] })}\n\n`
      + 'data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\n'
      + "data: [DONE]\n\n",
      { headers: { "Content-Type": "text/event-stream" } },
    );
  } });

  const port = await freePort();
  const base = `http://127.0.0.1:${port}`;
  async function startWorker() {
    worker = wrangler([
      "dev", "--config", config, "--local", "--ip", "127.0.0.1", "--port", String(port),
      "--persist-to", resolve(scratch, "state"),
      "--var", `APP_AUTH_TOKEN:${token}`,
      "--var", `PERI_STORAGE_URL:http://127.0.0.1:${sqlPort}`,
      "--var", "PERI_STORAGE_TOKEN:local-dev",
      "--var", "PERI_MACHINE_ID:00000000-0000-4000-8000-000000000004",
      "--var", `MODEL_BASE_URL:http://127.0.0.1:${model.port}/v1`,
      "--var", "MODEL_PROVIDER:openai",
      "--var", "MODEL_API_KEY:fixture-key", "--var", "MODEL_ID:fixture-model",
    ]);
    await waitForPort(port, worker);
  }
  async function api(path, options = {}) {
    const response = await fetch(`${base}/api/chats${path}`, {
      ...options, signal: AbortSignal.timeout(60_000),
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json", ...options.headers },
    });
    if (!response.ok) throw new Error(`API ${path} returned ${response.status}: ${await response.text()}`);
    return response;
  }

  await startWorker();
  assert((await fetch(`${base}/api/chats`)).status === 401, "Unauthenticated API was not denied");
  assert((await (await api("")).json()).chats.length === 0, "Fresh Turso Store did not return an empty list");
  const html = await (await fetch(base)).text();
  assert(html.includes('<div id="root"></div>'), "React SPA was not served");
  const chat = await (await api("", { method: "POST", body: JSON.stringify({ title: "Local WASM smoke" }) })).json();
  assert(typeof chat.id === "string", "Creation did not return a bare Chat");
  assert((await (await api("")).json()).chats.some((item) => item.id === chat.id && item.title === chat.title),
    "New ACP session was not queryable directly from Turso before its first prompt");
  await api(`/${chat.id}`);
  const renamedTitle = "Changed directly in Rust Store";
  const mutation = await fetch(`http://127.0.0.1:${sqlPort}/v2/pipeline`, {
    method: "POST", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ requests: [{ type: "execute", stmt: {
      sql: "UPDATE threads SET title = ? WHERE id = ?",
      args: [{ type: "text", value: renamedTitle }, { type: "text", value: chat.id }],
    } }, { type: "close" }] }),
  });
  assert(mutation.ok && (await mutation.json()).results[0].type === "ok", "Fixture Turso rename failed");
  assert((await (await api("")).json()).chats.find((item) => item.id === chat.id)?.title === renamedTitle,
    "Chat list did not reflect a direct Turso change");
  assert((await (await api(`/${chat.id}`)).json()).chat.title === renamedTitle,
    "Cached DO metadata masked the authoritative Turso title");
  let synchronization = await connectSync(base, chat.id);
  async function turn(text, expected) {
    const before = await synchronization.wait(() => true);
    const count = before.messages.length;
    const response = await api(`/${chat.id}/messages`, { method: "POST", body: JSON.stringify({ content: text }) });
    assert(response.status === 202 && (await response.json()).accepted === true, "Expected JSON command acknowledgement");
    const result = await synchronization.wait((state) => state.messages.length >= count + 2 && !state.running);
    const assistant = result.messages.at(-1);
    assert(assistant.status === "completed" && assistant.content.includes(expected) && !result.error,
      `WASM synchronized turn failed: ${JSON.stringify(result)}`);
  }
  await turn("Reply once", "LOCAL_WASM_REPLY_1");
  let detail = await (await api(`/${chat.id}`)).json();
  assert(detail.messages.length === 2 && detail.messages[1].status === "completed", "First turn did not persist");
  synchronization.close();
  await stop(worker);
  worker = undefined;
  await startWorker();
  synchronization = await connectSync(base, chat.id);
  assert((await synchronization.wait(() => true)).messages[1].content === "LOCAL_WASM_REPLY_1",
    "Restarted DO did not synchronize persisted history");
  detail = await (await api(`/${chat.id}`)).json();
  assert(detail.messages[1].content === "LOCAL_WASM_REPLY_1", "DO restart lost history");
  await turn("Continue once", "LOCAL_WASM_REPLY_2");
  detail = await (await api(`/${chat.id}`)).json();
  assert(detail.messages.length === 4 && detail.messages[3].status === "completed", "Second turn did not persist");
  const listed = await (await api("")).json();
  assert(listed.chats.some((item) => item.id === chat.id), "Turso session query lost chat after Worker restart");
  const cancellable = await api(`/${chat.id}/messages`, {
    method: "POST", body: JSON.stringify({ content: "Wait for cancellation" }),
  });
  assert(cancellable.status === 202, "Cancellation fixture command was not accepted");
  await synchronization.wait((state) => state.running && state.messages.at(-1)?.content.includes("LOCAL_CANCEL_PARTIAL"));
  await api(`/${chat.id}/cancel`, { method: "POST" });
  detail = await synchronization.wait((state) => !state.running && state.messages.at(-1)?.status === "cancelled");
  assert(detail.messages.length === 6 && detail.messages[5].content.includes("LOCAL_CANCEL_PARTIAL"),
    "Cancelled partial reply was not synchronized and preserved");
  await turn("Reply after cancellation", "LOCAL_WASM_REPLY_4");
  detail = await (await api(`/${chat.id}`)).json();
  assert(detail.messages.length === 8 && detail.messages[7].status === "completed", "Chat remained blocked after confirmed cancellation");
  assert(modelCalls === 4, `Unexpected primary model calls: ${modelCalls}`);
  console.log(JSON.stringify({ passed: true, runtime: "workerd", model: "local fixture", wasm: true, turns: 3,
    primaryModelCalls: modelCalls, predictionCalls, historyAfterRestart: true, acpContextAfterRestart: true,
    cancelledTurn: true, partialReplyPersisted: true, continuedAfterCancel: true,
    webSocketSync: true, commandHttp202: true, tursoDirectory: true, listBeforeFirstPrompt: true, externalTitleVisible: true }));
} catch (error) {
  console.error(JSON.stringify({ modelRequests }).slice(-8000));
  console.error((await Bun.file(logPath).text()).slice(-8000));
  throw error;
} finally {
  for (const connection of syncConnections) connection.close();
  await stop(worker);
  await stop(database);
  model?.stop(true);
  await log.close();
  await rm(scratch, { recursive: true, force: true });
}
