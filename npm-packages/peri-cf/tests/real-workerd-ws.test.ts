import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { Miniflare, convertV4MiniflareOptions, type WebSocket as MiniflareWebSocket } from "miniflare";
import { SessionDocReplica, decodeSyncFrame, encodeAckFrame, encodeAuthFrame, type SyncFrame } from "../worker/sdk";
import { replayChunk, replayToolCall } from "./helpers/runtime";
import { until, untilAsync, untilState } from "./helpers/sockets";
import { readSyncState } from "../shared/sync-state";
import type { ChatCommand, ChatCommandResult } from "../shared/sync";
import type { SyncState } from "../shared/sync";

/**
 * 真实 workerd（Miniflare）验收：无 DO、无 attachment、无持久连接状态。
 * 平台 seam（WebSocketPair + waitUntil）与帧分流（文本=控制帧、二进制=文档同步）都在真机语义下验证。
 */

const chatId = "00000000-0000-4000-8000-000000000001";
const token = "workerd-test-token";
const history = [
  replayChunk(chatId, "user-1", "历史提问", { role: "user", replay: true }),
  replayChunk(chatId, "assistant-1", "历史回答", { role: "assistant", replay: true }),
];
let runtime: Miniflare;
const sockets: MiniflareWebSocket[] = [];

async function start(allowedHistory = history) {
  const bundle = await Bun.build({ entrypoints: [new URL("./workerd-ws-fixture.ts", import.meta.url).pathname],
    target: "browser", external: ["node:*", "dns", "net"], minify: false });
  if (!bundle.success) throw new AggregateError(bundle.logs, "Workerd test fixture build failed");
  runtime = new Miniflare(convertV4MiniflareOptions({ name: "ws-contract", modules: true, script: await bundle.outputs[0]!.text(),
    compatibilityDate: "2026-10-01", compatibilityFlags: ["nodejs_compat"],
    bindings: { APP_AUTH_TOKEN: token, REPLAY_HISTORY: JSON.stringify(allowedHistory) },
  }));
  await runtime.ready;
}

beforeAll(async () => { await start(); }, 30_000);

afterAll(async () => {
  for (const socket of sockets) if (socket.readyState === 1) socket.close(1000, "Test complete");
  await runtime?.dispose();
});

function entryText(entry: SyncState["entries"][number] | undefined): string {
  return entry ? entry.blocks.flatMap((block) => (block.type === "text" ? [block.text] : [])).join("") : "";
}

function lastText(state: SyncState | undefined): string {
  return entryText(state?.entries.at(-1));
}

async function stats(): Promise<{ identity: string; starts: number; cancels: number; running: boolean;
  executionBlocked: boolean; resident: boolean; permissionOutcome: string | null }> {
  return await (await runtime.dispatchFetch(new URL("/stats", await runtime.ready), {
    headers: { Authorization: `Bearer ${token}` },
  })).json() as Awaited<ReturnType<typeof stats>>;
}

async function status(path: string): Promise<number> {
  return (await runtime.dispatchFetch(new URL(`/api/v2/chats/${chatId}${path}`, await runtime.ready), {
    headers: { Authorization: `Bearer ${token}` },
  })).status;
}

async function fixture(path: string, body: unknown = {}) {
  return runtime.dispatchFetch(new URL(`/fixture/${path}`, await runtime.ready), { method: "POST", body: JSON.stringify(body) });
}

/** 连接并（可选）鉴权；返回镜像与「收到的文本帧」记录，用于验证帧分流。 */
async function connect(autoAck = true, resume?: Parameters<typeof encodeAuthFrame>[0]["resume"]) {
  const response = await runtime.dispatchFetch(new URL(`/api/v2/chats/${chatId}/sync`, await runtime.ready), {
    headers: { Upgrade: "websocket" },
  });
  expect(response.status).toBe(101);
  const socket = response.webSocket!;
  sockets.push(socket);
  const frames: SyncFrame[] = [];
  const text: string[] = [];
  const replica = new SessionDocReplica();
  let closeCode: number | undefined;
  socket.addEventListener("message", (event) => {
    if (typeof event.data === "string") { text.push(event.data); return; }
    const frame = decodeSyncFrame(event.data);
    if (frame.type === "snapshot") replica.applySnapshot(frame.snapshot);
    else replica.applyUpdate(frame.update);
    frames.push(frame);
    if (autoAck) socket.send(encodeAckFrame(frame.delivery));
  });
  socket.addEventListener("close", (event) => { closeCode = event.code; });
  socket.accept();
  if (resume !== undefined || token !== undefined) socket.send(encodeAuthFrame({ type: "auth", token, resume }));
  return { socket, frames, text, replica, state: () => readSyncState(replica.chat, replica.session),
    closeCode: () => closeCode };
}

/** 文本帧发送控制帧命令并等待同一 id 的回执。 */
async function command(client: Awaited<ReturnType<typeof connect>>, raw: ChatCommand, budgetMs = 8_000): Promise<ChatCommandResult> {
  client.socket.send(JSON.stringify(raw));
  const deadline = Date.now() + budgetMs;
  for (;;) {
    const found = client.text.map((frame) => JSON.parse(frame) as ChatCommandResult).find((result) => result.id === raw.id);
    if (found) return found;
    if (Date.now() > deadline) throw new Error(`No control result for ${raw.id}`);
    await Bun.sleep(10);
  }
}

const prompt = (content: string): ChatCommand => ({ type: "command", id: `prompt-${content}`, op: "prompt", content });
const cancel = (id: string): ChatCommand => ({ type: "command", id, op: "cancel" });

describe("real workerd socket delivery without durable objects", () => {
  test("重放复原历史、文本帧只承载 command_result、执行与取消都经 WS 完成", async () => {
    const client = await connect();
    await until(() => client.frames.length > 0);
    // 首个快照即重放复原的历史（没有 DO 记录可读）。
    expect(client.state().entries.map(entryText)).toEqual(["历史提问", "历史回答"]);
    expect(client.text).toEqual([]);

    expect(await command(client, prompt("fixture prompt"))).toEqual({ type: "command_result", id: "prompt-fixture prompt", ok: true });
    await until(() => lastText(client.state()).includes("piece"));
    expect((await stats()).running).toBe(true);
    expect(await command(client, cancel("cancel-1"))).toEqual({ type: "command_result", id: "cancel-1", ok: true });
    await until(() => !client.state().running);
    expect(client.state().entries.at(-1)?.status).toBe("cancelled");
    expect((await stats()).cancels).toBe(1);
    // 文本帧仍然只有控制回执（帧分流硬约束）。
    for (const frame of client.text) {
      const parsed = JSON.parse(frame) as ChatCommandResult;
      expect(parsed.type).toBe("command_result");
      expect(typeof parsed.id).toBe("string");
    }
    expect(client.text.length).toBeGreaterThanOrEqual(2);
    client.socket.close(1000, "Done");
    client.replica.destroy();
  }, 20_000);

  test("isolate 重启后没有内存展示态：重连经真实重放复原外部存储里的历史且身份一致", async () => {
    // 先在本 isolate 产生实时内容（证明它只存在于内存）。
    const first = await connect();
    await until(() => first.frames.length > 0);
    expect((await command(first, prompt("restart prompt"))).ok).toBe(true);
    await until(() => lastText(first.state()).includes("piece"));
    await command(first, cancel("cancel-restart"));
    const before = await (await runtime.dispatchFetch(new URL(`/api/v2/chats/${chatId}`, await runtime.ready), {
      headers: { Authorization: `Bearer ${token}` },
    })).json() as SyncState;
    expect(before.entries.length).toBeGreaterThan(2);
    const liveEntries = before.entries.slice(2);
    const beforeStats = await stats();
    expect(beforeStats.resident).toBe(true);
    first.socket.close(1000, "Done");
    first.replica.destroy();

    // 重新加载 worker：模块状态（注册表、运行期、宿主计数）全部丢弃；
    // 夹具的持久面只有绑定 REPLAY_HISTORY（对应真实的 Turso 会话历史）。
    await start();
    const after = await stats();
    expect(after.identity).not.toBe(beforeStats.identity);
    expect(after.resident).toBe(false);
    expect(after.starts).toBe(0);

    const second = await connect();
    await until(() => second.frames.length > 0);
    expect(second.state().entries.map(entryText)).toEqual(["历史提问", "历史回答"]);
    expect(second.state().entries.map((entry) => entry.id)).toEqual(["turn:user-1:user", "turn:user-1:assistant"]);
    // 实时内容不属于外部存储：夹具不提供，复原后不会凭空出现；这也说明真实部署里
    // 每轮内容必须由 Rust 写入 Turso 才可复原（本仓库不验证 Turso 写入）。
    expect(liveEntries.length).toBeGreaterThan(0);
    expect(second.state().entries.map((entry) => entry.id)).not.toContain(liveEntries.at(-1)!.id);
    second.socket.close(1000, "Done");
    second.replica.destroy();
  }, 20_000);

  test("native sockets without bufferedAmount evict unacknowledged peers by credit without stopping execution", async () => {
    const client = await connect(false);
    await until(() => client.frames.length > 0);
    expect((await command(client, prompt("credit prompt"))).ok).toBe(true);
    await until(() => client.closeCode() !== undefined, 20_000);
    expect(client.closeCode()).toBe(1011);
    // 慢消费者被断开，但后台执行继续并在读面收敛（无 DO 时执行驻留在该 isolate）。
    await Bun.sleep(1_100);
    const state = await (await runtime.dispatchFetch(new URL(`/api/v2/chats/${chatId}`, await runtime.ready), {
      headers: { Authorization: `Bearer ${token}` },
    })).json() as SyncState;
    expect(lastText(state)).toContain("piece");
    client.replica.destroy();
  }, 30_000);

  test("rejects replayed acknowledgements and document writes without changing history", async () => {
    const client = await connect(false);
    await until(() => client.frames.length > 0);
    client.socket.send(encodeAckFrame(client.frames[0]!.delivery));
    client.socket.send(encodeAckFrame(client.frames[0]!.delivery));
    await until(() => client.closeCode() !== undefined);
    expect(client.closeCode()).toBe(4403);
    client.replica.destroy();
  }, 20_000);

  test("times out an unacknowledged idle snapshot without starting a host", async () => {
    const starts = (await stats()).starts;
    const client = await connect(false);
    await until(() => client.frames.length > 0);
    await until(() => client.closeCode() !== undefined, 20_000);
    expect(client.closeCode()).toBe(1013);
    expect((await stats()).starts).toBe(starts);
    client.replica.destroy();
  }, 30_000);
});

describe("command surface over the real platform", () => {
  test("a tool payload lives only in the session document and is served by the authenticated WS command", async () => {
    expect((await fixture("tool-result", { text: "P".repeat(6000) })).status).toBe(200);
    const client = await connect();
    await until(() => client.frames.length > 0);
    expect((await command(client, prompt("tool prompt"))).ok).toBe(true);
    await until(() => client.state().toolCalls.some((tool) => tool.toolCallId === "tool-1"));
    const ref = client.state().toolCalls.find((tool) => tool.toolCallId === "tool-1")!.resultRef!;
    expect(ref.bytes).toBeGreaterThan(4096);
    const read = await command(client, { type: "command", id: "payload-1", op: "payload", refId: ref.id, bytes: ref.bytes });
    expect(read).toMatchObject({ ok: true, payload: { id: ref.id, bytes: ref.bytes } });
    expect(JSON.parse(read.payload!.json)).toMatchObject({ contentText: "big result" });
    // 未知引用与字节不符按 SDK 失败码答复。
    expect(await command(client, { type: "command", id: "payload-2", op: "payload", refId: "gen:missing" }))
      .toMatchObject({ ok: false, error: { code: "unavailable" } });
    expect(await command(client, { type: "command", id: "payload-3", op: "payload", refId: ref.id, bytes: ref.bytes + 1 }))
      .toMatchObject({ ok: false, error: { code: "integrity_error" } });
    await command(client, cancel("cancel-2"));
    // 命令面 HTTP 路径已删除：载荷下载只能走 WS。
    expect(await status(`/payloads/${encodeURIComponent(ref.id)}`)).toBe(426);
    client.socket.close(1000, "Done");
    client.replica.destroy();
  }, 30_000);

  test("permission answers travel browser command → live responder → ACP response", async () => {
    expect((await fixture("blocked")).status).toBe(200);
    const client = await connect();
    await until(() => client.frames.length > 0);
    expect((await fixture("permission", { toolCallId: "tool-1" })).status).toBe(200);
    expect((await command(client, prompt("permission prompt"))).ok).toBe(true);
    await untilState(client, (state) => state.pendingInteractions.length === 1);
    const id = client.state().pendingInteractions[0]!.id;
    const answer = { type: "command", id: "answer-1", op: "interaction", interactionId: id,
      answer: { kind: "permission", decision: "allow_once" } } as const;
    expect(await command(client, answer)).toEqual({ type: "command_result", id: "answer-1", ok: true });
    await untilAsync(async () => (await stats()).permissionOutcome === JSON.stringify({ outcome: { outcome: "selected", optionId: "allow_once" } }));
    await untilState(client, (state) => state.pendingInteractions.length === 0);
    expect(client.state().interactions.at(-1)).toMatchObject({ id, status: "resolved", outcome: "allow_once" });
    // 重复回答按已结算处理（前端据此提示刷新重试）。
    expect(await command(client, { ...answer, id: "answer-2" })).toMatchObject({ ok: false, error: { code: "settled" } });
    await command(client, cancel("cancel-3"));
    client.socket.close(1000, "Done");
    client.replica.destroy();
  }, 30_000);

  test("an unconfirmed host close blocks the chat and expires pending interactions", async () => {
    const client = await connect();
    await until(() => client.frames.length > 0);
    // 复原阶段的宿主正常关闭；只让执行轮次的宿主持有关闭失败。
    expect((await fixture("blocked")).status).toBe(200);
    expect((await fixture("close-failure")).status).toBe(200);
    expect((await fixture("permission", { toolCallId: "tool-9" })).status).toBe(200);
    expect((await command(client, prompt("close-failure prompt"))).ok).toBe(true);
    await untilState(client, (state) => state.pendingInteractions.length === 1);
    const id = client.state().pendingInteractions[0]!.id;
    // 取消请求成立以宿主关闭确认为准：未确认时命令以错误收尾（不伪装成功停止）。
    const cancelled = await command(client, cancel("cancel-4"));
    expect(cancelled.ok).toBe(false);
    expect(cancelled.error?.code).toBe("cancel_failed");
    await untilState(client, (state) => state.executionBlocked && state.pendingInteractions.length === 0);
    expect(client.state().interactions.at(-1)).toMatchObject({ id, status: "expired" });
    // 宿主关闭导致的挂起按安全默认结算（拒绝），不会被记成用户允许。
    await untilAsync(async () => (await stats()).permissionOutcome === JSON.stringify({ outcome: { outcome: "selected", optionId: "reject_once" } }));
    expect(await command(client, prompt("after block")))
      .toMatchObject({ ok: false, error: { code: "blocked" } });
    client.socket.close(1000, "Done");
    client.replica.destroy();
  }, 30_000);
});

describe("cross-request runtime reuse on the real platform", () => {
  test("HTTP 首屏先创建运行期，后到的 WS 仍能收到流式更新（跨请求上下文）", async () => {
    // 干净 isolate：保证运行期由下面这次 HTTP 请求创建（同步通道的内部定时器属于该请求上下文）。
    await start();
    const detail = await runtime.dispatchFetch(new URL(`/api/v2/chats/${chatId}`, await runtime.ready), {
      headers: { Authorization: `Bearer ${token}` },
    });
    expect(detail.status).toBe(200);
    expect((await stats()).resident).toBe(true);
    const client = await connect();
    await until(() => client.frames.length > 0);
    expect((await command(client, prompt("cross-context prompt"))).ok).toBe(true);
    // 流式分块必须跨越「创建运行期的请求」到达后连上的连接。
    await untilState(client, (state) => lastText(state).includes("piece"), 8_000);
    expect(client.state().running).toBe(true);
    await command(client, cancel("cancel-cross"));
    await untilState(client, (state) => !state.running, 8_000);
    client.socket.close(1000, "Done");
    client.replica.destroy();
  }, 30_000);
});
