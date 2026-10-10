import { describe, expect, test } from "bun:test";
import { ChatRuntime } from "../worker/chat/session";
import { ChatSockets, type SyncSocket } from "../worker/chat/sync";
import { ChatRegistry } from "../worker/chat/registry";
import { promptCommand, replayChunk, runtimeFixture, type TransportPlan } from "./helpers/runtime";
import { FixtureSocket, FixtureSocketHost, until, untilAsync, untilState } from "./helpers/sockets";
import { encodeAckFrame, encodeAuthFrame, encodeSyncFrame, decodeSyncFrame, FRAME_AUTH, SYNC_WIRE_VERSION,
  MAX_AUTH_FRAME_BYTES } from "../worker/sdk";
import type { SyncState } from "../shared/sync";

/**
 * 无 DO 的同步连接：连接状态只在内存，鉴权、投递信用与恢复语义必须保持不变。
 * 重放复原由 hydrate-replay.test.ts 覆盖，这里只看连接层。
 */

const chatId = "00000000-0000-4000-8000-000000000001";

function entryText(entry: SyncState["entries"][number] | undefined): string {
  return entry ? entry.blocks.flatMap((block) => (block.type === "text" ? [block.text] : [])).join("") : "";
}

/** 打开连接并以首个二进制帧鉴权（连接层不认 Authorization 头）。 */
async function authenticate(app: ReturnType<typeof runtimeFixture>, chatId: string) {
  const { socket, response } = await app.connect(chatId, { authenticate: false });
  expect(response.status).toBe(101);
  socket.receive(encodeAuthFrame({ type: "auth", token: app.env.APP_AUTH_TOKEN! }));
  await until(() => socket.frames.length > 0 || !!socket.closed);
  return socket;
}

describe("session socket admission", () => {
  test("frontdoor forwards browser upgrades without Bearer headers and reads nothing before first-frame auth", async () => {
    const app = runtimeFixture();
    const chat = await app.create("Admission");
    const { socket, response } = await app.connect(chat.id);
    expect(response.status).toBe(101);
    expect(socket.frames).toEqual([]);
    expect(app.transports).toEqual([]);
    socket.close(1000, "Done");
  });

  test.each(["wrong", "trusted-app-token-extra", "trusted-app-token\r\nx", "🙂"])(
    "rejects invalid token %s without starting a host", async (token) => {
      const app = runtimeFixture();
      const chat = await app.create("Auth");
      const { socket } = await app.connect(chat.id, { authenticate: false });
      socket.receive(encodeAuthFrame({ type: "auth", token }));
      await until(() => !!socket.closed);
      expect(socket.closed?.code).toBe(4401);
      expect(socket.frames).toEqual([]);
      expect(app.transports).toEqual([]);
    });

  test("rejects an empty token frame and malformed first frames with 4401", async () => {
    const app = runtimeFixture();
    const chat = await app.create("Auth frames");
    for (const raw of [new Uint8Array([FRAME_AUTH, SYNC_WIRE_VERSION, 0, 0]),
      "{", new Uint8Array([FRAME_AUTH]), encodeAckFrame(1)]) {
      const { socket } = await app.connect(chat.id, { authenticate: false });
      socket.receive(raw);
      await until(() => !!socket.closed);
      expect(socket.closed?.code).toBe(4401);
    }
    expect(app.transports).toEqual([]);
  });

  test("closes oversized auth with 1009", async () => {
    const app = runtimeFixture();
    const chat = await app.create("Oversized");
    const { socket } = await app.connect(chat.id, { authenticate: false });
    socket.receive(new Uint8Array(MAX_AUTH_FRAME_BYTES + 1));
    await until(() => !!socket.closed);
    expect(socket.closed?.code).toBe(1009);
  });

  test("times out unauthenticated sockets after three seconds without starting a host", async () => {
    const app = runtimeFixture();
    const chat = await app.create("Timeout");
    const { socket } = await app.connect(chat.id, { authenticate: false });
    await until(() => !!socket.closed, 4_500);
    expect(socket.closed?.code).toBe(4401);
    expect(app.transports).toEqual([]);
  }, 6_000);

  test("admits at most sixteen sockets and releases disconnected slots", async () => {
    const app = runtimeFixture();
    const chat = await app.create("Capacity");
    const sockets: FixtureSocket[] = [];
    try {
      for (let count = 0; count < 16; count++) sockets.push(await authenticate(app, chat.id));
      expect((await app.connect(chat.id)).response.status).toBe(429);
      sockets[0]!.close(1000, "Release slot");
      expect((await app.connect(chat.id)).response.status).toBe(101);
    } finally { for (const socket of sockets) socket.close(1000, "Done"); }
  });

  test("missing chats close with 4404 only after authenticating", async () => {
    const app = runtimeFixture();
    const { socket } = await app.connect(chatId, { authenticate: false });
    socket.receive(encodeAuthFrame({ type: "auth", token: app.env.APP_AUTH_TOKEN! }));
    await until(() => !!socket.closed);
    expect(socket.closed?.code).toBe(4404);
    expect(socket.frames).toEqual([]);
  });

  test("rejects URL tokens and non-upgrade requests without opening sockets", async () => {
    const app = runtimeFixture();
    const chat = await app.create("Upgrade checks");
    for (const [suffix, headers, status] of [
      ["?token=trusted-app-token", { Upgrade: "websocket" }, 400],
      ["", {}, 426],
    ] as const) {
      const response = await app.request(`/api/v2/chats/${chat.id}/sync${suffix}`, { token: null, headers });
      expect(response.status).toBe(status);
      expect(response.headers.get("Cache-Control")).toBe("no-store");
    }
    expect(app.host.sockets).toEqual([]);
    expect(app.transports).toEqual([]);
  });

  test("authenticated binary writes and repeated auth violate the read-only boundary", async () => {
    const app = runtimeFixture();
    const chat = await app.create("Read only");
    const writable = encodeSyncFrame({ type: "update", delivery: 1, update: {
      protocol: 2, generation: "fixture", sequence: 0, chat: new Uint8Array([1]),
    } });
    for (const raw of [writable, encodeAuthFrame({ type: "auth", token: app.env.APP_AUTH_TOKEN! })]) {
      const socket = await authenticate(app, chat.id);
      socket.receive(raw);
      await until(() => !!socket.closed);
      expect(socket.closed?.code).toBe(4403);
      expect(socket.state().entries).toEqual([]);
    }
  });

  test("malformed text control frames close the connection without a result frame", async () => {
    const app = runtimeFixture();
    const chat = await app.create("Bad control frame");
    const socket = await authenticate(app, chat.id);
    socket.receive("{");
    await until(() => !!socket.closed);
    expect(socket.closed?.code).toBe(4403);
    expect(socket.results()).toEqual([]);
  });
});

describe("read-only document delivery", () => {
  test("syncs replayed history, live chunks and terminal state through the same DTO", async () => {
    const history = [replayChunk(chatId, "user-1", "旧问题", { role: "user", replay: true })];
    const app = runtimeFixture({ replay: history, chunks: [], blocked: true });
    const chat = await app.create("Delivery");
    const socket = await authenticate(app, chat.id);
    // 连接建立即看到重放复原的历史（冷 isolate 没有持久展示态）。
    expect(socket.state()).toMatchObject({ running: false, executionBlocked: false });
    // 重放的用户消息会开启一个 turn：助手条目此时为空，符合 SDK 的历史投影规则。
    expect(socket.state().entries.map(entryText)).toEqual(["旧问题", ""]);
    expect(await app.send(socket, promptCommand("Hello"))).toMatchObject({ ok: true });
    const transport = await app.runTransport();
    await transport.promptStarted;
    transport.userEcho("Hello");
    transport.emit("root");
    // 外部会话与子代理事件在 prompt 夹具里自动混入，必须被丢弃。
    await until(() => entryText(socket.state().entries.at(-1)) === "root");
    expect(socket.state().entries.map(entryText)).toEqual(["旧问题", "", "Hello", "root"]);
    const metadata = socket.replica.session.getMap("peri-cf").get("state");
    expect(metadata).not.toHaveProperty("messages");
    expect(metadata).not.toHaveProperty("entryMetadata");
    transport.finish();
    await app.drain();
    await untilState(socket, (state) => state.running === false
      && state.entries.map((entry) => entry.status).join("|") === "completed|completed|completed|completed");
    socket.close(1000, "Done");
  });

  test("HTTP 首屏与 WS 实时返回同一 DTO 与同一批对象身份", async () => {
    // blocked：由测试决定这一轮的结束时机，才能观察流式与终态的同一 DTO。
    const app = runtimeFixture({ chunks: [], blocked: true });
    const chat = await app.create("Parity");
    const socket = await authenticate(app, chat.id);
    await app.send(socket, promptCommand("Hello"));
    const transport = await app.runTransport();
    await transport.promptStarted;
    transport.emit("answer");
    await until(() => entryText(socket.state().entries.at(-1)) === "answer");
    transport.finish();
    await app.drain();
    const expected = await app.detail(chat.id);
    // 连接副本按泵节奏收敛：读到与 HTTP 首屏同一份 DTO（对象身份同源）。
    await untilState(socket, (state) => state.entries.length === expected.entries.length
      && state.entries.map((entry) => entry.status).join("|") === expected.entries.map((entry) => entry.status).join("|"));
    expect(socket.state().entries.map((entry) => entry.id)).toEqual(expected.entries.map((entry) => entry.id));
    expect(socket.state().activeTurn).toEqual(expected.activeTurn);
    expect(expected.readiness).toBe("ready");
    socket.close(1000, "Done");
  });

  test("disconnection leaves execution running and SDK state-vector resume repairs missed chunks", async () => {
    const app = runtimeFixture({ chunks: [], blocked: true });
    const chat = await app.create("Resume");
    const socket = await authenticate(app, chat.id);
    await app.send(socket, promptCommand("Hello"));
    const transport = await app.runTransport();
    await transport.promptStarted;
    transport.emit("first");
    await until(() => entryText(socket.state().entries.at(-1)) === "first");
    const resume = socket.replica.stateVector();
    socket.close(1000, "Offline");
    transport.emit(" missed");
    expect(transport.closed).toBe(false);
    const { socket: next } = await app.connect(chat.id, { authenticate: false });
    for (const frame of socket.frames) {
      if (typeof frame === "string") continue;
      const decoded = decodeSyncFrame(frame);
      if (decoded.type === "snapshot") next.replica.applySnapshot(decoded.snapshot);
      else next.replica.applyUpdate(decoded.update);
    }
    // 携带状态向量的首个鉴权帧就是断线续传点。
    next.receive(encodeAuthFrame({ type: "auth", token: app.env.APP_AUTH_TOKEN!, resume }));
    await until(() => next.frames.length > 0);
    const first = decodeSyncFrame(next.frames[0] as Uint8Array);
    expect(first.type === "snapshot" && first.snapshot.mode).toBe("delta");
    expect(entryText(next.state().entries.at(-1))).toBe("first missed");
    expect(await app.send(next, { type: "command", id: "cancel-1", op: "cancel" })).toMatchObject({ ok: true });
    await app.drain();
    await untilState(next, (state) => state.entries.at(-1)?.status === "cancelled");
    next.close(1000, "Done");
  });

  test("an unconfirmed host close blocks execution and reports the app error without rewriting the completed result", async () => {
    // 复原阶段的宿主正常关闭；只让执行轮次的宿主持有关闭失败（关闭证据必须真实）。
    const plan: TransportPlan = { chunks: [], blocked: true };
    const app = runtimeFixture(plan);
    const chat = await app.create("Close evidence");
    const socket = await authenticate(app, chat.id);
    plan.closeFailure = new Error("close failed");
    await app.send(socket, promptCommand("Hello"));
    const transport = await app.runTransport();
    await transport.promptStarted;
    transport.emit("partial");
    transport.done();
    transport.finish();
    await app.drain();
    await untilState(socket, (state) => state.executionBlocked === true && state.running === false
      && state.error === "close failed" && state.entries.at(-1)?.status === "completed");
    expect(socket.state()).toMatchObject({ running: false, executionBlocked: true, error: "close failed" });
    expect(entryText(socket.state().entries.at(-1))).toBe("partial");
    expect(await app.send(socket, promptCommand("Again"))).toMatchObject({ ok: false, error: { code: "blocked" } });
    socket.close(1000, "Done");
  });

  test("a slow socket is evicted without cancelling the background execution", async () => {
    const app = runtimeFixture({ chunks: [], blocked: true });
    const chat = await app.create("Slow peer");
    const socket = await authenticate(app, chat.id);
    await app.send(socket, promptCommand("Hello"));
    const transport = await app.runTransport();
    await transport.promptStarted;
    socket.autoAck = false;
    for (let chunk = 0; chunk < 20 && !socket.closed; chunk++) {
      transport.emit("reply");
      await Bun.sleep(20);
    }
    await until(() => !!socket.closed);
    expect(socket.closed?.code).toBe(1011);
    expect(transport.closed).toBe(false);
    transport.finish();
    await app.drain();
    expect(entryText((await app.detail(chat.id)).entries.at(-1))).toContain("reply");
  });
});

describe("in-memory socket registry", () => {
  test("closing peers retain their admission until the transport close is confirmed", async () => {
    const chat = { id: chatId, title: "Close confirmation", updatedAt: "2026-10-06T00:00:00.000Z" };
    const sockets = new ChatSockets("chat", { APP_AUTH_TOKEN: "token" }, {
      async hydrate() {}, sync: () => { throw new Error("Not used"); },
      async run() { return { type: "command_result", id: "x", ok: true }; }, inFlight: () => undefined,
    });
    const host = new FixtureSocketHost();
    const upgrade = () => new Request("https://app.invalid/api/v2/chats/chat/sync", { headers: { Upgrade: "websocket" } });
    for (let index = 0; index < 16; index++) expect(sockets.open(upgrade(), host).status).toBe(101);
    // 关闭未确认前保留名额（否则平台会接受超过上限的连接）。
    for (const socket of host.sockets) socket.deferClose = true;
    sockets.closeAll(1000, "Done");
    expect(sockets.size).toBe(16);
    for (const socket of host.sockets.slice(1)) socket.confirmClose();
    expect(sockets.size).toBe(1);
    host.sockets[0]!.confirmClose();
    expect(sockets.size).toBe(0);
    expect(sockets.open(upgrade(), host).status).toBe(101);
    expect(chat.id).toBe(chatId);
  });

  test("peek never hydrates or replays", () => {
    const app = runtimeFixture({ replay: [replayChunk(chatId, "user-1", "不应出现", { role: "user", replay: true })] });
    expect(app.registry.peek(chatId)).toBeUndefined();
    expect(app.transports).toEqual([]);
    const registry = new ChatRegistry();
    const runtime = registry.runtimeFor(chatId, app.env, { startTransport: async () => { throw new Error("unused"); },
      repository: app.repository });
    expect(runtime).toBeInstanceOf(ChatRuntime);
    expect(registry.peek(chatId)).toBe(runtime);
    expect(app.transports).toEqual([]);
  });
});
