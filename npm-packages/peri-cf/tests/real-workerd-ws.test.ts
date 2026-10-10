import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { Miniflare, convertV4MiniflareOptions, type WebSocket as MiniflareWebSocket } from "miniflare";
import { SessionDocReplica, decodeSyncFrame, encodeAckFrame, encodeAuthFrame, type SyncFrame } from "../worker/sdk";
import { readSyncState } from "../shared/sync-state";

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
  return runtime.dispatchFetch(new URL(`/api/chats/${chatId}/${path}`, await runtime.ready), { method: "POST",
    headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
    ...(path === "messages" ? { body: JSON.stringify({ content }) } : {}),
  });
}

async function connect(autoAck = true) {
  const response = await runtime.dispatchFetch(new URL(`/api/chats/${chatId}/sync`, await runtime.ready), { headers: { Upgrade: "websocket" } });
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
    await until(() => client.state().messages.at(-1)?.content.includes("piece") === true);
    expect((await command("cancel")).status).toBe(200);
    await until(() => !client.state().running);
    expect(client.state().messages.at(-1)?.status).toBe("cancelled");
    expect((await command("messages", "explicit continuation")).status).toBe(202);
    await until(() => client.state().messages.length >= 4 && client.state().running);
    expect((await command("cancel")).status).toBe(200);
    await until(() => !client.state().running);
    expect((await stats()).cancels).toBe(2);
    const history = client.state().messages;
    const hostStarts = (await stats()).starts;
    await untilAsync(async () => (await stats()).attachments.every((attachment) => attachment.pending.length === 0));
    await runtime.unsafeEvictDurableObject("ws-contract", "TestSession", { name: "chat", webSockets: "hibernate" });
    await runtime.dispatchFetch(new URL(`/api/chats/${chatId}`, await runtime.ready), { headers: { Authorization: `Bearer ${token}` } });
    await until(() => client.frames.filter((frame) => frame.type === "snapshot").length >= 3);
    expect(client.state().messages).toEqual(history);
    expect((await stats()).starts).toBe(hostStarts);
    client.socket.close(1000, "Done");
    client.replica.destroy();
  }, 20_000);

  test("native sockets without bufferedAmount evict unacknowledged peers by credit without stopping execution", async () => {
    const client = await connect(false);
    client.socket.send(encodeAuthFrame({ type: "auth", token }));
    await until(() => client.frames.length > 0);
    expect((await command("messages")).status).toBe(202);
    await until(() => client.closeCode() !== undefined);
    expect(client.closeCode()).toBe(1011);
    await Bun.sleep(1_100);
    const response = await runtime.dispatchFetch(new URL(`/api/chats/${chatId}`, await runtime.ready), { headers: { Authorization: `Bearer ${token}` } });
    const record = await response.json() as { messages: { status: string; content: string }[] };
    expect(record.messages.at(-1)?.status).toBe("completed");
    expect(record.messages.at(-1)?.content).toContain("piece");
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
