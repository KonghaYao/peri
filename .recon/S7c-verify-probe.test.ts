/**
 * S7c 独立复核探针（verification 产物，不属于切片；复核结束即删除）。
 *
 * 只用于独立复现实现者报告中的 S1~S5 与 A22 断言，不进入交付。
 */
import { describe, expect, test } from "bun:test";
import * as Y from "yjs";
import { HistoryPageUnavailableError } from "../src/agent/session";
import { MemoryKV } from "../src/kv/memory-kv";
import { ManagedAgents } from "../src/managed/managed-agents";
import { Sandbox } from "../src/sandbox/sandbox";
import { SessionDocs } from "../src/state/session-docs";
import { readSessionView } from "../src/view/session-view";
import type { SessionView } from "../src/view/session-view";
import type { JsonRpcNotification, Transport } from "../src/transport/types";
import {
  AWAITING_RESULT_STATUS,
  COMPACTION_STATUS,
  Session as ExportedSession,
  HistoryPageUnavailableError as ExportedHistoryPageUnavailableError,
} from "../src/sdk/index";

const SESSION = "s1";
const TOKEN = "1:524288:range-token";

function legacyEvent(sessionId: string, type: string, value: Record<string, unknown>): JsonRpcNotification {
  return { jsonrpc: "2.0", method: "peri/agent_event", params: { sessionId, event_json: JSON.stringify({ type, value }) } };
}

function update(sessionId: string, sessionUpdate: string, fields: Record<string, unknown>): JsonRpcNotification {
  return { jsonrpc: "2.0", method: "session/update", params: { sessionId, update: { sessionUpdate, ...fields } } };
}

function replayUpdate(entryId: string, position: number | undefined, text: string, generation: string): JsonRpcNotification {
  return {
    jsonrpc: "2.0",
    method: "session/update",
    params: {
      sessionId: SESSION,
      update: {
        sessionUpdate: "agent_message_chunk",
        content: { type: "text", text },
        _meta: {
          periReplay: true, periLoadGeneration: generation, periEntryId: entryId,
          ...(position === undefined ? {} : { periPosition: position }),
        },
      },
    },
  };
}

function sessionInfo(docs: SessionDocs): Y.Map<unknown> {
  return docs.session.getMap<unknown>("root").get("session") as Y.Map<unknown>;
}

function view(docs: SessionDocs): SessionView {
  return readSessionView(docs.chat, docs.session);
}

// ── S1：Compact 失败 / 取消收敛 ───────────────────────────────────────────────

describe("S1 独立探针：Compact 失败的展示收敛", () => {
  test("S1-a command_feedback(error) 让 compactionStatus 离开 running 并在 view 可见", () => {
    const docs = new SessionDocs();
    docs.accept(update("s1", "user_message_chunk", { messageId: "u1", content: { type: "text", text: "Keep me" } }));
    docs.accept(legacyEvent("s1", "compact_started", {}));
    expect(view(docs).session.compactionStatus).toBe(COMPACTION_STATUS.running);

    // wire 形状照服务端：AcpEvent::CommandFeedback { level: "error", message, channel: "uiOnly" }
    docs.accept(legacyEvent("s1", "command_feedback", {
      level: "error", message: "compact persistence context mismatch", channel: "uiOnly",
    }));

    const snapshot = view(docs);
    expect(snapshot.session.compactionStatus).toBe("failed");
    expect(snapshot.session.compaction).toMatchObject({
      status: "failed", message: "compact persistence context mismatch",
    });
    // 失败只更新展示：已展示的消息区不被清空（首条仍是这次用户输入）。
    expect(snapshot.entries.length).toBeGreaterThan(0);
    expect(snapshot.entries[0]!.blocks.map((block) => (block.type === "text" ? block.text : ""))).toEqual(["Keep me"]);
    docs.destroy();
  });

  test("S1-b command_feedback(warning) 收敛为 cancelled", () => {
    const docs = new SessionDocs();
    docs.accept(legacyEvent("s1", "compact_started", {}));
    docs.accept(legacyEvent("s1", "command_feedback", {
      level: "warning", message: "compact cancelled", channel: "uiOnly",
    }));
    expect(view(docs).session.compactionStatus).toBe("cancelled");
    docs.destroy();
  });

  test("S1-c【缺口探针】compact_started 之前的真实失败反馈没有任何展示", () => {
    // 服务端 `compact_pipeline.rs` 在 `emit_compact_started`（阶段 6）之前就有多条
    // EarlyReturn（"no history to compact"、"compact persistence is unavailable"、
    // "compact persistence context mismatch" …），它们同样只经 CommandFeedback(Error)
    // 到达客户端（`execute_compact` → `emit_command_feedback`）。
    const docs = new SessionDocs();
    docs.accept(legacyEvent("s1", "command_feedback", {
      level: "error", message: "no history to compact", channel: "uiOnly",
    }));

    const snapshot = view(docs);
    // 记录当前行为：既没有 compactionStatus（不存在「卡在 running」），也没有任何失败展示。
    expect(snapshot.session.compactionStatus).toBeUndefined();
    expect(snapshot.session.compaction).toBeUndefined();
    docs.destroy();
  });

  test("S1-d 终态不被后续反馈改写，且其它命令的反馈不改写已完成的 Compact", () => {
    const docs = new SessionDocs();
    docs.accept(legacyEvent("s1", "compact_started", {}));
    docs.accept(legacyEvent("s1", "compact_completed", {
      trigger: "manual", strategy: "full", affected_count: 2, estimated_tokens_saved: 10, messages_json: "[]",
    }));
    docs.accept(legacyEvent("s1", "command_feedback", { level: "error", message: "/clear failed", channel: "uiOnly" }));
    expect(view(docs).session.compactionStatus).toBe("completed");
    docs.destroy();
  });

  test("S1-e 自动 Compact 失败经 agent_execution_failed 收敛（轮次失败路径）", () => {
    const docs = new SessionDocs();
    docs.accept(update("s1", "user_message_chunk", { messageId: "u1", content: { type: "text", text: "work" } }));
    docs.accept(legacyEvent("s1", "compact_started", {}));
    docs.accept(legacyEvent("s1", "agent_execution_failed", { message: "compact failed under pressure" }));
    expect(view(docs).session.compactionStatus).toBe("failed");
    expect(sessionInfo(docs).get("activeTurnStatus")).toBe("error");
    docs.destroy();
  });

  test("S1-f【风险探针】Compact 运行期间其它命令的 warning 会被记成 cancelled", () => {
    const docs = new SessionDocs();
    docs.accept(legacyEvent("s1", "compact_started", {}));
    docs.accept(legacyEvent("s1", "command_feedback", {
      level: "warning", message: "rewind cancelled", channel: "uiOnly",
    }));
    // 记录当前行为：反馈不带命令名，窗口期内任何 warning 都会被当作 Compact 的取消。
    expect(view(docs).session.compactionStatus).toBe("cancelled");
    docs.destroy();
  });
});

// ── S3：页 / 回放两条路径的块身份 ─────────────────────────────────────────────

describe("S3 独立探针：跨路径块身份", () => {
  const MESSAGE_ID = "00000000-0000-4000-8000-00000000000a";

  function pageWith(entryId: string, position: number, subItems: Array<{ position: number; update: Record<string, unknown> }>) {
    return { sessionId: "s1", snapshotToken: TOKEN, nextCursor: null, items: [{ entryId, position, oversize: false, subItems }] };
  }
  function textSub(value: string, subPosition: number) {
    return { position: subPosition, update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text: value }, messageId: MESSAGE_ID } };
  }
  function replayOf(entryId: string, value: string, position: number): JsonRpcNotification {
    return {
      jsonrpc: "2.0", method: "session/update",
      params: { sessionId: "s1", update: {
        sessionUpdate: "agent_message_chunk", content: { type: "text", text: value }, messageId: MESSAGE_ID,
        _meta: { periReplay: true, periLoadGeneration: "gen-1", periEntryId: entryId, periPosition: position },
      } },
    };
  }

  test("S3-a 页先落库、回放后到：同一个块（块身份一致）", () => {
    const docs = new SessionDocs();
    docs.seedHistoryWindow({ snapshotToken: TOKEN, nextCursor: null });
    docs.mergeHistoryPage("s1", pageWith("s1:m1", 10, [textSub("Answer", 0)]));
    docs.accept(replayOf("s1:m1", "Answer", 10));

    const snapshot = view(docs);
    expect(snapshot.entries).toHaveLength(1);
    expect(snapshot.entries[0]!.blocks).toHaveLength(1);
    expect(snapshot.entries[0]!.blocks[0]!.blockId).toBe(`h:${MESSAGE_ID}:text:0`);
    docs.destroy();
  });

  test("S3-b 回放先落库、页后到：同一个块", () => {
    const docs = new SessionDocs();
    docs.seedHistoryWindow({ snapshotToken: TOKEN, nextCursor: null });
    docs.accept(replayOf("s1:m1", "Answer", 10));
    docs.mergeHistoryPage("s1", pageWith("s1:m1", 10, [textSub("Answer", 0)]));

    const snapshot = view(docs);
    expect(snapshot.entries).toHaveLength(1);
    expect(snapshot.entries[0]!.blocks).toHaveLength(1);
    expect(snapshot.entries[0]!.blocks[0]!.blockId).toBe(`h:${MESSAGE_ID}:text:0`);
    docs.destroy();
  });

  test("S3-c 同一 messageId 的两个不同正文子项仍各自成块", () => {
    const docs = new SessionDocs();
    docs.seedHistoryWindow({ snapshotToken: TOKEN, nextCursor: null });
    docs.mergeHistoryPage("s1", pageWith("s1:m1", 10, [textSub("First", 0), textSub("Second", 1)]));

    const blocks = view(docs).entries[0]!.blocks;
    expect(blocks.map((block) => (block.type === "text" ? block.text : ""))).toEqual(["First", "Second"]);
    expect(new Set(blocks.map((block) => block.blockId)).size).toBe(2);
    docs.destroy();
  });
});

// ── S2 / S4 / A22：经真实 Session 走一遍 ─────────────────────────────────────

class ProbeTransport implements Transport {
  private readonly listeners = new Set<(notification: JsonRpcNotification) => void>();
  readonly calls: Array<{ method: string; params?: unknown }> = [];
  replayEntries: Array<{ entryId: string; position?: number; text: string }> = [];
  replayGeneration: string | null = null;
  initialLoadGate: Promise<void> | null = null;
  initialLoadError: Error | null = null;
  initialSnapshotToken: string | null = TOKEN;
  initialNextCursor: string | null = "cursor-1";
  lastLoadGeneration: string | null = null;
  readonly initialLoadSeen = (() => {
    let resolve!: () => void;
    const promise = new Promise<void>((res) => { resolve = res; });
    return { promise, resolve };
  })();
  readonly pages: Array<{ params: Record<string, unknown>; settle: (value: unknown) => void; fail: (error: Error) => void }> = [];

  async request<T>(method: string, params?: unknown): Promise<T> {
    this.calls.push({ method, params });
    if (method === "initialize") return { protocolVersion: 1 } as T;
    if (method === "session/new") return { sessionId: SESSION } as T;
    if (method === "session/load") {
      const request = (params ?? {}) as Record<string, unknown>;
      if (typeof request.cursor === "string") {
        let settle!: (value: unknown) => void;
        let fail!: (error: Error) => void;
        const promise = new Promise<unknown>((res, rej) => { settle = res; fail = rej; });
        this.pages.push({ params: request, settle, fail });
        return promise as Promise<T>;
      }
      const generation = typeof request.loadGeneration === "string" ? request.loadGeneration : null;
      this.lastLoadGeneration = generation;
      const replayGeneration = this.replayGeneration ?? generation ?? "";
      for (const entry of this.replayEntries)
        this.emit(replayUpdate(entry.entryId, entry.position, entry.text, replayGeneration));
      this.initialLoadSeen.resolve();
      if (this.initialLoadGate) await this.initialLoadGate;
      if (this.initialLoadError) throw this.initialLoadError;
      return {
        sessionId: SESSION,
        ...(this.initialSnapshotToken === null ? {} : { snapshotToken: this.initialSnapshotToken }),
        nextCursor: this.initialNextCursor,
        ...(generation ? { _meta: { periLoadGeneration: generation } } : {}),
      } as T;
    }
    if (method === "session/input/snapshot") return { generation: "generation-1" } as T;
    if (method === "session/close") return {} as T;
    throw new Error(`Unexpected ACP request: ${method}`);
  }
  async sendRequest<T>(method: string, params?: unknown): Promise<{ response: Promise<T> }> {
    return { response: this.request<T>(method, params) };
  }
  async notify(): Promise<void> {}
  subscribe(listener: (notification: JsonRpcNotification) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }
  emit(notification: JsonRpcNotification): void { for (const listener of this.listeners) listener(notification); }
  async *events(): AsyncIterable<JsonRpcNotification> {}
  setRequestHandler(): void {}
  async close(): Promise<void> {}
}

const storage = {
  getSessions: async () => [],
  getSession: async (id: string) => ({
    id, cwd: "/tmp/workspace", title: null, messageCount: 0, createdAt: "now", updatedAt: "now",
  }),
};

function makeAgent(transport: ProbeTransport, id: string) {
  return new ManagedAgents({ kv: new MemoryKV() }).createAgent({
    path: "/tmp/workspace",
    id,
    sandbox: new Sandbox({ id: `${id}-workspace`, storage, transportFactory: () => transport }),
  });
}

function entryTexts(docs: SessionDocs): string[] {
  const root = docs.chat.getMap<unknown>("root");
  const order = root.get("entryOrder") as Y.Array<string>;
  const entries = root.get("entries") as Y.Map<Y.Map<unknown>>;
  return order.toArray().map((id) => {
    const entry = entries.get(id)!;
    const blockOrder = entry.get("blockOrder") as Y.Array<string>;
    const blocks = entry.get("blocks") as Y.Map<Y.Map<unknown>>;
    return blockOrder.toArray().map((blockId) => String(blocks.get(blockId)?.get("text"))).join("");
  });
}

describe("S2 独立探针：原始事件流与文档投影", () => {
  test("S2-a 首屏回放在 stream() 逐条到达，文档仍暂存到响应后一次安装", async () => {
    const transport = new ProbeTransport();
    transport.replayEntries = [
      { entryId: "s1:first", position: 10, text: "older first" },
      { entryId: "s1:second", position: 20, text: "compacted summary" },
    ];
    let release!: () => void;
    transport.initialLoadGate = new Promise<void>((res) => { release = res; });
    const agent = makeAgent(transport, "probe-stream-agent");
    const starting = agent.session.start(SESSION);
    await transport.initialLoadSeen.promise;

    // 响应未回：文档不得半截呈现；此时 session 未 active，stream() 第一次 next() 就拒绝。
    expect(entryTexts(agent.docs)).toEqual([]);
    const early = agent.session.stream()[Symbol.asyncIterator]();
    await expect(early.next()).rejects.toThrow("Session is not active");

    release();
    await starting;
    // 响应后：文档一次安装，顺序与到达顺序一致。
    expect(entryTexts(agent.docs)).toEqual(["older first", "compacted summary"]);

    const seen: string[] = [];
    const stream = agent.session.stream()[Symbol.asyncIterator]();
    for (let i = 0; i < 2; i++) {
      const next = await stream.next();
      const params = next.value.params as { update?: { content?: { text?: string } } };
      seen.push(params.update?.content?.text ?? "");
    }
    // 有界队列保留了暂存窗口内到达的首屏回放（108/4 MiB 预算内不丢条）。
    expect(seen).toEqual(["older first", "compacted summary"]);

    // 安装后同代次重复投递：文档不重复，原始流照收（「连接收到什么就有什么」）。
    transport.emit(replayUpdate("s1:first", 10, "older first", transport.lastLoadGeneration!));
    expect(entryTexts(agent.docs)).toEqual(["older first", "compacted summary"]);
    const extra = await stream.next();
    const extraText = (extra.value.params as { update?: { content?: { text?: string } } }).update?.content?.text;
    expect(extraText).toBe("older first");
    await stream.return?.();
    await agent.close();
  });

  test("S2-b 失败代次：文档不留痕；stream() 在失败前不可消费，队列随失败丢弃", async () => {
    const transport = new ProbeTransport();
    transport.replayEntries = [{ entryId: "s1:summary", position: 20, text: "doomed replay" }];
    transport.initialLoadError = new Error("session/load failed");
    const agent = makeAgent(transport, "probe-failed-agent");

    await expect(agent.session.start(SESSION)).rejects.toThrow("session/load failed");
    expect(entryTexts(agent.docs)).toEqual([]);
    // start 失败后 Session 回到 declared：stream() 第一次 next() 拒绝，没有半份首屏被外部读到。
    const afterFailure = agent.session.stream()[Symbol.asyncIterator]();
    await expect(afterFailure.next()).rejects.toThrow("Session is not active");
    await agent.session.cleanupStartup();
  });
});

describe("S4 独立探针：可区分的原因", () => {
  test("S4-a 窗口存在但 nextCursor=null → exhausted（不是未协商）", async () => {
    const transport = new ProbeTransport();
    transport.initialNextCursor = null;
    const agent = makeAgent(transport, "probe-exhausted-agent");
    await agent.session.start(SESSION);

    const error = await agent.session.loadEarlierHistory().catch((err: unknown) => err);
    expect(error).toBeInstanceOf(HistoryPageUnavailableError);
    expect((error as HistoryPageUnavailableError).reason).toBe("exhausted");
    expect((error as Error).message).toContain("no earlier history");
    await agent.close();
  });

  test("S4-b 响应没有窗口 → not_negotiated", async () => {
    const transport = new ProbeTransport();
    transport.initialSnapshotToken = null;
    const agent = makeAgent(transport, "probe-unnegotiated-agent");
    await agent.session.start(SESSION);

    const error = await agent.session.loadEarlierHistory().catch((err: unknown) => err);
    expect((error as HistoryPageUnavailableError).reason).toBe("not_negotiated");
    await agent.close();
  });

  test("S4-c rewind 之后 → invalidated（与 exhausted 区分）", async () => {
    const transport = new ProbeTransport();
    const agent = makeAgent(transport, "probe-invalidated-agent");
    await agent.session.start(SESSION);
    transport.emit(legacyEvent(SESSION, "rewind_completed", { messages_json: "[]" }));

    const error = await agent.session.loadEarlierHistory().catch((err: unknown) => err);
    expect((error as HistoryPageUnavailableError).reason).toBe("invalidated");
    await agent.close();
  });
});

describe("A22 独立探针：代次暂存与重试", () => {
  test("A22-a 失败丢弃 + 同会话重试恰好一份 + 失败代次迟到不污染", async () => {
    const transport = new ProbeTransport();
    transport.replayEntries = [{ entryId: "s1:summary", position: 20, text: "compacted summary" }];
    transport.initialLoadError = new Error("session/load failed");
    const agent = makeAgent(transport, "probe-retry-agent");

    await expect(agent.session.start(SESSION)).rejects.toThrow("session/load failed");
    const failedGeneration = transport.lastLoadGeneration!;
    expect(entryTexts(agent.docs)).toEqual([]);

    transport.initialLoadError = null;
    await agent.session.start(SESSION);
    expect(transport.lastLoadGeneration).not.toBe(failedGeneration);
    expect(entryTexts(agent.docs)).toEqual(["compacted summary"]);

    transport.emit(replayUpdate("s1:summary", 20, "stale copy", failedGeneration));
    expect(entryTexts(agent.docs)).toEqual(["compacted summary"]);
    await agent.close();
  });

  test("A22-b 订阅先于 load：响应前到达的回放一条不漏（不漏消息）", async () => {
    const transport = new ProbeTransport();
    transport.replayEntries = [{ entryId: "s1:summary", position: 20, text: "first screen" }];
    const agent = makeAgent(transport, "probe-subscribe-first-agent");
    await agent.session.start(SESSION);

    const load = transport.calls.find((call) => call.method === "session/load");
    expect((load?.params as { loadGeneration?: string }).loadGeneration).toBe(transport.lastLoadGeneration);
    expect(entryTexts(agent.docs)).toEqual(["first screen"]);
    await agent.close();
  });
});

describe("S5 独立探针：公开出口", () => {
  test("S5-a 导出名可消费，取值与文档一致", () => {
    expect(COMPACTION_STATUS).toEqual({ running: "running", completed: "completed", failed: "failed", cancelled: "cancelled" });
    expect(AWAITING_RESULT_STATUS).toBe("awaiting_result");
    expect(ExportedSession).toBeDefined();
    expect(new ExportedHistoryPageUnavailableError("exhausted", "x").reason).toBe("exhausted");
  });

  test("S5-b 历史页 oversize 标记可从 view 读", () => {
    const docs = new SessionDocs();
    docs.seedHistoryWindow({ snapshotToken: TOKEN, nextCursor: null });
    docs.mergeHistoryPage("s1", { sessionId: "s1", snapshotToken: TOKEN, nextCursor: null, items: [
      { entryId: "s1:big", position: 10, oversize: true, subItems: [{ position: 0, update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text: "big" } } }] },
    ] });
    expect(view(docs).entries[0]!.historyOversize).toBe(true);
    docs.destroy();
  });
});
