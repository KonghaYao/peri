import { describe, expect, test } from "bun:test";
import {
  HistoryPageUnavailableError,
  StaleHistoryPageError,
} from "../src/state/history-browser";
import {
  PeriInstance,
  type PeriInstanceCloseOptions,
} from "../src/agent/peri-instance";
import { Session } from "../src/agent/session";
import { ChatDocument } from "../src/view/chat-document";
import { readSessionView } from "../src/view/session-view";
import type { ChatFact } from "../src/chat/facts";
import type { JsonRpcNotification, Transport } from "../src/transport/types";

// 契约出处：peri-acp/src/host/history_page.rs 模块文档（初始 load 响应带
// `snapshotToken` / `nextCursor`；历史页响应 {sessionId, snapshotToken, nextCursor,
// items}；回放通知的 `_meta.periLoadGeneration` 按当前连接回带）。字段名照 wire 原样。

const SESSION = "s1";
const TOKEN = "1:524288:range-token";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

/** 回放通知：带代次与条目身份戳（`dispatch/session_replay.rs` 的 `replay_meta`）。 */
function replayUpdate(
  entryId: string,
  position: number | undefined,
  text: string,
  generation: string,
): JsonRpcNotification {
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

function pageItem(entryId: string, position: number, text: string) {
  return {
    entryId, position, oversize: false,
    subItems: [{ position: 0, update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text } } }],
  };
}

function pageResponse(
  items: ReturnType<typeof pageItem>[],
  nextCursor: string | null,
  snapshotToken = TOKEN,
) {
  return { sessionId: SESSION, snapshotToken, nextCursor, items };
}

function entryTexts(document: ChatDocument): string[] {
  return readSessionView(document.chat, document.session).entries.map((entry) =>
    entry.blocks.map((block) => block.text ?? "").join(""));
}

class PagedTransport implements Transport {
  private readonly listeners = new Set<(notification: JsonRpcNotification) => void>();
  readonly calls: Array<{ method: string; params?: unknown }> = [];
  /** 初始 load 回放先发哪些条目（代次默认回带请求里的 `loadGeneration`）。 */
  replayEntries: Array<{ entryId: string; position?: number; text: string }> = [];
  /** 强制替换回放通知的代次，用来构造「迟到／被取代」的历史。 */
  replayGeneration: string | null = null;
  /** 初始 load 响应的放行闸门：测试先断言「暂存中」，再放行。 */
  initialLoadGate: Promise<void> | null = null;
  initialLoadError: Error | null = null;
  /** 初始 load 响应的窗口身份与首游标：`null` 分别构造「未协商分页」与「没有更早内容」。 */
  initialSnapshotToken: string | null = TOKEN;
  initialNextCursor: string | null = "cursor-1";
  lastLoadGeneration: string | null = null;
  readonly initialLoadSeen = deferred<void>();
  readonly pages: Array<{
    params: Record<string, unknown>;
    settle: (value: unknown) => void;
    fail: (error: Error) => void;
  }> = [];

  async request<T>(method: string, params?: unknown): Promise<T> {
    this.calls.push({ method, params });
    if (method === "initialize") return { protocolVersion: 1 } as T;
    if (method === "session/new") return { sessionId: SESSION } as T;
    if (method === "session/load") {
      const request = (params ?? {}) as Record<string, unknown>;
      if (typeof request.cursor === "string") {
        const pending = deferred<unknown>();
        this.pages.push({ params: request, settle: pending.resolve, fail: pending.reject });
        return pending.promise as Promise<T>;
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
  emit(notification: JsonRpcNotification): void {
    for (const listener of this.listeners) listener(notification);
  }
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

const closeOptions: PeriInstanceCloseOptions = {
  drainTimeoutMs: 1000,
  bindingSettleTimeoutMs: 1000,
  activationCloseTimeoutMs: 1000,
};

function makeInstance(
  transport: PagedTransport,
  paths: string[] = [],
): PeriInstance {
  return new PeriInstance({
    acquireHost: async (path) => {
      paths.push(path);
      return { transport, close: async () => ({ confirmed: true }) };
    },
    resolveSessionPath: async (sessionId) => (await storage.getSession(sessionId)).cwd,
    defaultMcpServers: {},
    close: closeOptions,
  });
}

/** 事实 → 展示文档；PeriInstance 只发事实，测试不直接摸运行侧历史缓冲。 */
function attachDisplay(instance: PeriInstance): ChatDocument {
  const document = new ChatDocument();
  instance.attach((fact: ChatFact) => {
    document.applyFact(fact);
  });
  return document;
}

describe("history load staging and pagination", () => {
  test("declares peri.historyPagination and pages with only sessionId, cursor, and limit", async () => {
    const transport = new PagedTransport();
    const paths: string[] = [];
    const instance = makeInstance(transport, paths);
    const document = attachDisplay(instance);
    await instance.bindExisting(SESSION);

    // session/load 的 cwd 来自 Store 中的持久路径，而不是调用方句柄 path。
    expect(paths).toEqual(["/tmp/workspace"]);
    const initialize = transport.calls.find((call) => call.method === "initialize")?.params as {
      clientCapabilities?: { _meta?: Record<string, unknown> };
    };
    expect(initialize.clientCapabilities?._meta?.["peri.historyPagination"]).toBe(true);
    const load = transport.calls.find((call) => call.method === "session/load")?.params as Record<string, unknown>;
    expect(typeof load.loadGeneration).toBe("string");
    expect(load.cursor).toBeUndefined();

    const paging = instance.loadEarlierHistory(2);
    expect(transport.pages).toHaveLength(1);
    // 历史页不得携带装配参数（REQ §5.2）。
    expect(transport.pages[0]!.params).toEqual({ sessionId: SESSION, cursor: "cursor-1", limit: 2 });

    transport.pages[0]!.settle(pageResponse([pageItem("s1:older", 10, "older answer")], null));
    await expect(paging).resolves.toEqual({ applied: 1, nextCursor: null });
    expect(entryTexts(document)).toEqual(["older answer"]);
    // nextCursor = null 表示更早方向确实没有内容，入口随即关闭。
    await expect(instance.loadEarlierHistory()).rejects.toBeInstanceOf(HistoryPageUnavailableError);
    await expect(instance.loadEarlierHistory()).rejects.toMatchObject({ reason: "exhausted" });
    await instance.close();
  });

  test("a first screen with no earlier content reports an exhausted window, not a missing capability (A15)", async () => {
    const transport = new PagedTransport();
    transport.initialNextCursor = null;
    const instance = makeInstance(transport);
    const document = attachDisplay(instance);
    await instance.bindExisting(SESSION);

    await expect(instance.loadEarlierHistory()).rejects.toBeInstanceOf(HistoryPageUnavailableError);
    await expect(instance.loadEarlierHistory()).rejects.toMatchObject({
      reason: "exhausted",
      message: expect.stringContaining("no earlier history"),
    });
    expect(entryTexts(document)).toEqual([]);
    await instance.close();
  });

  test("a load response without a browsing window reports a missing capability", async () => {
    const transport = new PagedTransport();
    transport.initialSnapshotToken = null;
    const instance = makeInstance(transport);
    await instance.bindExisting(SESSION);

    await expect(instance.loadEarlierHistory()).rejects.toBeInstanceOf(HistoryPageUnavailableError);
    await expect(instance.loadEarlierHistory()).rejects.toMatchObject({
      reason: "not_negotiated",
      message: expect.stringContaining("no history pagination window"),
    });
    await instance.close();
  });

  test("the staged first-screen replay still reaches the raw event stream (REQ-H04)", async () => {
    // 原始诊断流是 Session 层契约；PeriInstance 只向 View 发归一化事实。
    const transport = new PagedTransport();
    transport.replayEntries = [{ entryId: "s1:summary", position: 20, text: "compacted summary" }];
    const session = new Session(transport, { requestHandler: () => undefined });
    await session.start({ path: "/tmp/workspace", sessionId: SESSION });

    const stream = session.stream()[Symbol.asyncIterator]();
    const first = await stream.next();
    expect(first.done).toBe(false);
    expect(first.value.method).toBe("session/update");
    const params = first.value.params as { update?: { content?: { text?: string }; _meta?: Record<string, unknown> } };
    expect(params.update?.content?.text).toBe("compacted summary");
    expect(params.update?._meta?.periLoadGeneration).toBe(transport.lastLoadGeneration);
    await stream.return?.();
    await session.close();
  });

  test("replay is staged until the load response installs it in one pass (A22)", async () => {
    const transport = new PagedTransport();
    transport.replayEntries = [{ entryId: "s1:summary", position: 20, text: "compacted summary" }];
    const gate = deferred<void>();
    transport.initialLoadGate = gate.promise;
    const instance = makeInstance(transport);
    const document = attachDisplay(instance);

    const starting = instance.bindExisting(SESSION);
    await transport.initialLoadSeen.promise;
    // 回放已到达但响应未回：窗口不得半截呈现。
    expect(entryTexts(document)).toEqual([]);
    gate.resolve();
    await starting;

    expect(entryTexts(document)).toEqual(["compacted summary"]);

    // 安装之后同一代次再来的回放是重复投递：整条丢弃，窗口不变。
    transport.emit(replayUpdate("s1:summary", 20, "compacted summary", transport.lastLoadGeneration!));
    expect(entryTexts(document)).toEqual(["compacted summary"]);
    await instance.close();
  });

  test("replay from a superseded generation never reaches the window", async () => {
    const transport = new PagedTransport();
    transport.replayEntries = [{ entryId: "s1:stale", position: 10, text: "stale replay" }];
    transport.replayGeneration = "superseded-generation";
    const instance = makeInstance(transport);
    const document = attachDisplay(instance);
    await instance.bindExisting(SESSION);

    expect(entryTexts(document)).toEqual([]);
    await instance.close();
  });

  test("a failed load leaves no staged replay behind", async () => {
    const transport = new PagedTransport();
    transport.replayEntries = [{ entryId: "s1:summary", position: 20, text: "compacted summary" }];
    transport.initialLoadError = new Error("session/load failed");
    const instance = makeInstance(transport);
    const document = attachDisplay(instance);

    await expect(instance.bindExisting(SESSION)).rejects.toThrow("session/load failed");
    expect(entryTexts(document)).toEqual([]);
    await instance.close();
  });

  test("a failed load discards its staged replay, and a retry installs exactly one copy (A22)", async () => {
    const transport = new PagedTransport();
    transport.replayEntries = [{ entryId: "s1:summary", position: 20, text: "compacted summary" }];
    transport.initialLoadError = new Error("session/load failed");
    const instance = makeInstance(transport);
    const document = attachDisplay(instance);

    await expect(instance.bindExisting(SESSION)).rejects.toThrow("session/load failed");
    const failedGeneration = transport.lastLoadGeneration!;
    expect(failedGeneration.length).toBeGreaterThan(0);
    expect(entryTexts(document)).toEqual([]);

    // 同一 Session 重试：失败代次的暂存不得被沿用，成功代次恰好安装一份。
    transport.initialLoadError = null;
    await instance.bindExisting(SESSION);
    expect(transport.lastLoadGeneration).not.toBe(failedGeneration);
    expect(entryTexts(document)).toEqual(["compacted summary"]);

    // 失败代次的迟到回放不得污染新快照。
    transport.emit(replayUpdate("s1:summary", 20, "stale copy", failedGeneration));
    expect(entryTexts(document)).toEqual(["compacted summary"]);
    await instance.close();
  });

  test("concurrent calls with the same cursor and limit share one request", async () => {
    const transport = new PagedTransport();
    const instance = makeInstance(transport);
    const document = attachDisplay(instance);
    await instance.bindExisting(SESSION);

    const first = instance.loadEarlierHistory(5);
    const second = instance.loadEarlierHistory(5);
    expect(transport.pages).toHaveLength(1);

    transport.pages[0]!.settle(pageResponse([pageItem("s1:older", 10, "older answer")], "cursor-2"));
    const [a, b] = await Promise.all([first, second]);
    expect(a).toEqual({ applied: 1, nextCursor: "cursor-2" });
    expect(b).toEqual(a);
    expect(entryTexts(document)).toEqual(["older answer"]);
    await instance.close();
  });

  test("a failed page keeps the window and the same cursor can be retried", async () => {
    const transport = new PagedTransport();
    const instance = makeInstance(transport);
    const document = attachDisplay(instance);
    await instance.bindExisting(SESSION);

    const failing = instance.loadEarlierHistory();
    transport.pages[0]!.fail(new Error("history page unavailable"));
    await expect(failing).rejects.toThrow("history page unavailable");
    expect(entryTexts(document)).toEqual([]);

    const retrying = instance.loadEarlierHistory();
    expect(transport.pages[1]!.params.cursor).toBe("cursor-1");
    transport.pages[1]!.settle(pageResponse([pageItem("s1:older", 10, "older answer")], null));
    await expect(retrying).resolves.toEqual({ applied: 1, nextCursor: null });
    expect(entryTexts(document)).toEqual(["older answer"]);
    await instance.close();
  });

  test("a page that arrives after rewind is rejected and the window is cleared", async () => {
    const transport = new PagedTransport();
    const instance = makeInstance(transport);
    const document = attachDisplay(instance);
    await instance.bindExisting(SESSION);

    const paging = instance.loadEarlierHistory();
    transport.emit({
      jsonrpc: "2.0",
      method: "peri/agent_event",
      params: {
        sessionId: SESSION,
        event_json: JSON.stringify({ type: "rewind_completed", value: { messages_json: "[]" } }),
      },
    });
    await expect(instance.loadEarlierHistory()).rejects.toMatchObject({
      name: "HistoryPageUnavailableError", reason: "invalidated",
    });

    transport.pages[0]!.settle(pageResponse([pageItem("s1:older", 10, "older answer")], null));
    await expect(paging).rejects.toMatchObject({ name: "StaleHistoryPageError", reason: "window" });
    expect(entryTexts(document)).toEqual([]);
    await instance.close();
  });

  test("a page from a different snapshot is rejected whole (A11)", async () => {
    const transport = new PagedTransport();
    const instance = makeInstance(transport);
    const document = attachDisplay(instance);
    await instance.bindExisting(SESSION);

    const paging = instance.loadEarlierHistory();
    transport.pages[0]!.settle(pageResponse([pageItem("s1:older", 10, "older answer")], null, "9:1:other-token"));
    await expect(paging).rejects.toBeInstanceOf(StaleHistoryPageError);
    expect(entryTexts(document)).toEqual([]);
    await instance.close();
  });

  test("a page that arrives after close is dropped", async () => {
    const transport = new PagedTransport();
    const instance = makeInstance(transport);
    const document = attachDisplay(instance);
    await instance.bindExisting(SESSION);

    const paging = instance.loadEarlierHistory();
    await instance.close();
    transport.pages[0]!.settle(pageResponse([pageItem("s1:older", 10, "older answer")], null));
    await expect(paging).rejects.toBeInstanceOf(StaleHistoryPageError);
    expect(entryTexts(document)).toEqual([]);
  });
});
