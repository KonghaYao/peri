import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { Miniflare, convertV4MiniflareOptions, type WebSocket as MiniflareWebSocket } from "miniflare";
import { SessionDocReplica, decodeSyncFrame, encodeAckFrame, encodeAuthFrame, type SyncFrame } from "../worker/sdk";
import { readSyncState } from "../shared/sync-state";
import type { SyncState } from '../shared/sync';

const chatId = "00000000-0000-4000-8000-000000000001";
const token = "workerd-test-token";
let runtime: Miniflare;
const sockets: MiniflareWebSocket[] = [];

beforeAll(async () => {
  // workerd provides Node builtins through nodejs_compat; the bundler only has to keep them out of the fixture.
  const bundle = await Bun.build({ entrypoints: [new URL("./workerd-ws-fixture.ts", import.meta.url).pathname],
    target: "browser", external: ["node:*", "dns", "net"], minify: false });
  if (!bundle.success) throw new AggregateError(bundle.logs, "Workerd test fixture build failed");
  runtime = new Miniflare(convertV4MiniflareOptions({ name: "ws-contract", modules: true, script: await bundle.outputs[0]!.text(),
    compatibilityDate: "2026-10-01", compatibilityFlags: ["nodejs_compat"],
    bindings: { APP_AUTH_TOKEN: token }, durableObjects: { CHAT_SESSIONS: { className: "TestSession", useSQLite: true } },
  }));
  await runtime.ready;
}, 30_000);

afterAll(async () => {
  for (const socket of sockets) if (socket.readyState === 1) socket.close(1000, "Test complete");
  await runtime?.dispose();
});

function entryText(entry: SyncState['entries'][number] | undefined): string {
  return entry ? entry.blocks.flatMap((block) => (block.type === 'text' ? [block.text] : [])).join('') : '';
}

function lastText(state: SyncState | undefined): string {
  return entryText(state?.entries.at(-1));
}

async function until(predicate: () => boolean, timeout = 4_000): Promise<void> {
  const deadline = Date.now() + timeout;
  while (!predicate()) {
    if (Date.now() > deadline) throw new Error("Workerd WS transition timed out");
    await Bun.sleep(10);
  }
}

async function untilAsync(predicate: () => Promise<boolean>): Promise<void> {
  const deadline = Date.now() + 4_000;
  while (!await predicate()) {
    if (Date.now() > deadline) throw new Error("Workerd WS state did not settle");
    await Bun.sleep(10);
  }
}

async function stats(): Promise<{ identity: string; sockets: number; bufferedAmountType: string; starts: number; cancels: number;
  attachments: { pending: unknown[]; credential: string; nextDelivery: number }[] }> {
  return await (await runtime.dispatchFetch(new URL("/stats", await runtime.ready), { headers: { Authorization: `Bearer ${token}` } })).json() as Awaited<ReturnType<typeof stats>>;
}

async function command(path: string, content = "fixture prompt") {
  return runtime.dispatchFetch(new URL(`/api/v2/chats/${chatId}/${path}`, await runtime.ready), { method: "POST",
    headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
    ...(path === "messages" ? { body: JSON.stringify({ content }) } : {}),
  });
}

async function connect(autoAck = true) {
  const response = await runtime.dispatchFetch(new URL(`/api/v2/chats/${chatId}/sync`, await runtime.ready), { headers: { Upgrade: "websocket" } });
  expect(response.status).toBe(101);
  const socket = response.webSocket!;
  sockets.push(socket);
  const frames: SyncFrame[] = [];
  const replica = new SessionDocReplica();
  const received = { binary: 0, text: 0 };
  let closeCode: number | undefined;
  socket.addEventListener("message", (event) => {
    if (typeof event.data === "string") received.text++;
    else received.binary++;
    const frame = decodeSyncFrame(event.data);
    if (frame.type === "snapshot") replica.applySnapshot(frame.snapshot);
    else replica.applyUpdate(frame.update);
    frames.push(frame);
    if (autoAck) socket.send(encodeAckFrame(frame.delivery));
  });
  socket.addEventListener("close", (event) => { closeCode = event.code; });
  socket.accept();
  return { socket, frames, replica, received, state: () => readSyncState(replica.chat, replica.session), closeCode: () => closeCode };
}

describe("actual workerd read-only WS delivery and hibernation", () => {
  test("hibernates an authenticated idle socket, reconstructs attachments and replaces the document generation", async () => {
    const client = await connect();
    expect(client.frames).toHaveLength(0);
    client.socket.send(encodeAuthFrame({ type: "auth", token }));
    await until(() => client.frames.length > 0);
    expect(client.received.text).toBe(0);
    expect(client.received.binary).toBeGreaterThan(0);
    await untilAsync(async () => (await stats()).attachments.every((attachment) => attachment.pending.length === 0));
    const before = await stats();
    expect(before.bufferedAmountType).toBe("undefined");
    expect(before.attachments[0]!.credential).not.toContain(token);
    expect(JSON.stringify(before.attachments).includes(token)).toBe(false);
    const first = client.frames[0]!;
    const oldGeneration = first.type === "snapshot" ? first.snapshot.generation : "";
    await runtime.unsafeEvictDurableObject("ws-contract", "TestSession", { name: "chat", webSockets: "hibernate" });
    const restored = await stats();
    expect(restored.identity).not.toBe(before.identity);
    expect(restored.sockets).toBe(1);
    expect(restored.starts).toBe(0);
    expect((await command("messages")).status).toBe(202);
    await until(() => client.frames.filter((frame) => frame.type === "snapshot").length >= 2);
    const snapshots = client.frames.filter((frame) => frame.type === "snapshot");
    expect(snapshots[1]!.snapshot.generation).not.toBe(oldGeneration);
    await until(() => lastText(client.state()).includes("piece"));
    expect((await command("cancel")).status).toBe(200);
    await until(() => !client.state().running);
    expect(client.state().entries.at(-1)?.status).toBe("cancelled");
    expect((await command("messages", "explicit continuation")).status).toBe(202);
    await until(() => client.state().entries.length >= 4 && client.state().running);
    expect((await command("cancel")).status).toBe(200);
    await until(() => !client.state().running);
    expect((await stats()).cancels).toBe(2);
    const history = client.state().entries.map(entryText);
    const hostStarts = (await stats()).starts;
    await untilAsync(async () => (await stats()).attachments.every((attachment) => attachment.pending.length === 0));
    await runtime.unsafeEvictDurableObject("ws-contract", "TestSession", { name: "chat", webSockets: "hibernate" });
    await runtime.dispatchFetch(new URL(`/api/v2/chats/${chatId}`, await runtime.ready), { headers: { Authorization: `Bearer ${token}` } });
    await until(() => client.frames.filter((frame) => frame.type === "snapshot").length >= 3);
    // 旧记录持久化路径能重建文本；精确身份往返由 P3 checkpoint import 保证。
    expect(client.state().entries.map(entryText)).toEqual(history);
    expect((await stats()).starts).toBe(hostStarts);
    client.socket.close(1000, "Done");
    client.replica.destroy();
  }, 20_000);

  test("取消在响应前读完分段请求体，且保持空请求体可用", async () => {
    let finishBody!: () => void;
    let bodyCancelled = false;
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode("{"));
        finishBody = () => { if (!bodyCancelled) { controller.enqueue(new TextEncoder().encode("}")); controller.close(); } };
      },
      cancel() { bodyCancelled = true; },
    });
    let responded = false;
    const response = runtime.dispatchFetch(new URL(`/api/v2/chats/${chatId}/cancel`, await runtime.ready), {
      method: "POST", headers: { Authorization: `Bearer ${token}` }, body,
    }).then((result) => { responded = true; return result; });
    try {
      await Bun.sleep(200);
      expect(responded).toBe(false);
    } finally { finishBody(); }
    expect((await response).status).toBe(200);
    expect(bodyCancelled).toBe(false);
    expect((await command("cancel")).status).toBe(200);
    for (const [size, status] of [[65_536, 200], [65_537, 413]] as const) {
      const bounded = await runtime.dispatchFetch(new URL(`/api/v2/chats/${chatId}/cancel`, await runtime.ready), {
        method: "POST", headers: { Authorization: `Bearer ${token}` }, body: "x".repeat(size),
      });
      expect(bounded.status).toBe(status);
    }
  });

  test("native sockets without bufferedAmount evict unacknowledged peers by credit without stopping execution", async () => {
    const client = await connect(false);
    client.socket.send(encodeAuthFrame({ type: "auth", token }));
    await until(() => client.frames.length > 0);
    expect((await command("messages")).status).toBe(202);
    await until(() => client.closeCode() !== undefined);
    expect(client.closeCode()).toBe(1011);
    await Bun.sleep(1_100);
    const response = await runtime.dispatchFetch(new URL(`/api/v2/chats/${chatId}`, await runtime.ready), { headers: { Authorization: `Bearer ${token}` } });
    const record = await response.json() as SyncState;
    expect(record.entries.at(-1)?.status).toBe("completed");
    expect(entryText(record.entries.at(-1))).toContain("piece");
    client.replica.destroy();
  }, 10_000);

  test("rejects replayed acknowledgements and document writes without changing history", async () => {
    const client = await connect(false);
    client.socket.send(encodeAuthFrame({ type: "auth", token }));
    await until(() => client.frames.length > 0);
    client.socket.send(encodeAckFrame(client.frames[0]!.delivery));
    client.socket.send(encodeAckFrame(client.frames[0]!.delivery));
    await until(() => client.closeCode() !== undefined);
    expect(client.closeCode()).toBe(4403);
    client.replica.destroy();
  });

  test("times out an unacknowledged idle snapshot without starting a host", async () => {
    const starts = (await stats()).starts;
    const client = await connect(false);
    client.socket.send(encodeAuthFrame({ type: "auth", token }));
    await until(() => client.frames.length > 0);
    await until(() => client.closeCode() !== undefined, 12_000);
    expect(client.closeCode()).toBe(1013);
    expect((await stats()).starts).toBe(starts);
    client.replica.destroy();
  }, 15_000);
});

describe("P5 closed-loop acceptance over real workerd storage", () => {
  const authed = { Authorization: `Bearer ${token}` };
  const json = { ...authed, "Content-Type": "application/json" };
  const fixture = async (path: string, body: unknown = {}) =>
    runtime.dispatchFetch(new URL(`/fixture/${path}`, await runtime.ready), { method: "POST", body: JSON.stringify(body) });

  test("a tool payload lives in real DO storage and is served by the authenticated endpoint", async () => {
    expect((await fixture("tool-result", { text: "P".repeat(6000) })).status).toBe(200);
    const client = await connect();
    client.socket.send(encodeAuthFrame({ type: "auth", token }));
    await until(() => client.frames.length > 0);
    expect((await command("messages")).status).toBe(202);
    await untilAsync(async () => {
      const tool = client.state().toolCalls.find((item) => item.toolCallId === "tool-1");
      return tool?.resultRef !== undefined;
    });
    const ref = client.state().toolCalls.find((item) => item.toolCallId === "tool-1")!.resultRef!;
    expect(ref.bytes).toBeGreaterThan(4096);
    const path = new URL(`/api/v2/chats/${chatId}/payloads/${encodeURIComponent(ref.id)}`, await runtime.ready);
    const response = await runtime.dispatchFetch(path, { headers: authed });
    expect(response.status).toBe(200);
    expect(response.headers.get("Cache-Control")).toBe("no-store");
    const payload = await response.json() as { id: string; bytes: number; json: string };
    expect(payload).toMatchObject({ id: ref.id, bytes: ref.bytes });
    expect(JSON.parse(payload.json)).toMatchObject({ contentText: "big result" });
    // 未鉴权 / 未知引用 / 其它会话不泄露。
    expect((await runtime.dispatchFetch(path)).status).toBe(401);
    const missing = new URL(`/api/v2/chats/${chatId}/payloads/missing-ref`, await runtime.ready);
    expect((await runtime.dispatchFetch(missing, { headers: authed })).status).toBe(404);
    const otherChat = new URL(`/api/v2/chats/00000000-0000-4000-8000-000000000099/payloads/${encodeURIComponent(ref.id)}`, await runtime.ready);
    expect([404]).toContain((await runtime.dispatchFetch(otherChat, { headers: authed })).status);
    client.socket.close(1000, "Done");
  });

  test("permission answers travel browser command → live responder → ACP response", async () => {
    expect((await fixture("blocked")).status).toBe(200);
    const client = await connect();
    client.socket.send(encodeAuthFrame({ type: "auth", token }));
    await until(() => client.frames.length > 0);
    expect((await command("messages")).status).toBe(202);
    const pending = fixture("permission", { toolCallId: "tool-1" });
    await untilAsync(async () => client.state().pendingInteractions.length === 1);
    const id = client.state().pendingInteractions[0]!.id;
    const answerPath = new URL(`/api/v2/chats/${chatId}/interactions/${encodeURIComponent(id)}`, await runtime.ready);
    const body = JSON.stringify({ kind: "permission", decision: "allow_once" });
    const accepted = await runtime.dispatchFetch(answerPath, { method: "POST", headers: json, body });
    expect(accepted.status).toBe(202);
    expect(await (await pending).json()).toEqual({ result: { outcome: { outcome: "selected", optionId: "allow_once" } } });
    await untilAsync(async () => client.state().pendingInteractions.length === 0);
    expect(client.state().interactions.at(-1)).toMatchObject({ id, status: "resolved", outcome: "allow_once" });
    // 重复回答与未经鉴权的回答都被拒绝。
    expect((await runtime.dispatchFetch(answerPath, { method: "POST", headers: json, body })).status).toBe(409);
    expect((await runtime.dispatchFetch(answerPath, { method: "POST", headers: { "Content-Type": "application/json" }, body })).status).toBe(401);
    expect((await command("cancel"))).toBeDefined();
    expect((await command("cancel")).status).toBe(200);
    client.socket.close(1000, "Done");
  });

  test("an unconfirmed host close blocks the chat and expires pending interactions", async () => {
    expect((await fixture("blocked")).status).toBe(200);
    expect((await fixture("close-failure")).status).toBe(200);
    const client = await connect();
    client.socket.send(encodeAuthFrame({ type: "auth", token }));
    await until(() => client.frames.length > 0);
    expect((await command("messages", "third turn")).status).toBe(202);
    const pending = fixture("permission", { toolCallId: "tool-9" });
    await untilAsync(async () => client.state().pendingInteractions.length === 1);
    const id = client.state().pendingInteractions[0]!.id;
    // 取消请求成立以宿主关闭确认为准：未确认时命令以错误收尾（不伪装成功停止）。
    expect((await command("cancel")).status).toBe(500);
    await untilAsync(async () => client.state().executionBlocked === true);
    // 断宿主：未决交互失效，回答被拒，后续命令保持阻断。
    expect(client.state().pendingInteractions).toEqual([]);
    expect(client.state().interactions.at(-1)).toMatchObject({ id, status: "expired" });
    await expect(pending.then((response) => response.json()))
      .resolves.toEqual({ result: { outcome: { outcome: "selected", optionId: "reject_once" } } });
    const answerPath = new URL(`/api/v2/chats/${chatId}/interactions/${encodeURIComponent(id)}`, await runtime.ready);
    expect((await runtime.dispatchFetch(answerPath, { method: "POST", headers: json,
      body: JSON.stringify({ kind: "permission", decision: "allow_once" }) })).status).toBe(409);
    expect((await command("messages", "after block")).status).toBe(503);
    client.socket.close(1000, "Done");
  }, 20_000);
});
