import { describe, expect, test } from "bun:test";
import type { SyncState } from "../../core/shared";
import type { AcquireHost, AgentEnvironment, ChatSessionStore } from "../../core/agent";
import { PeriInstance } from "./peri-instance";

const state: SyncState = {
  chat: { id: "11111111-1111-4111-8111-111111111111", title: "测试会话", updatedAt: "2026-10-20T00:00:00Z" },
  running: false, executionBlocked: false, readiness: "ready",
  revision: 0,
  entries: [], toolCalls: [], turns: [], activeTurn: { turnId: null, status: null },
  session: {}, tasks: [], plans: { current: [], byTurn: {} },
  interactions: [], pendingInteractions: [], inputQueue: null,
};

const unusedHost: AcquireHost = async () => { throw new Error("unused host"); };
const store: Pick<ChatSessionStore, "get"> = { get: async () => null };
const environment = {} as unknown as AgentEnvironment;

/** 只验证实例观测面：用固定读面绕过宿主复原。 */
class TestInstance extends PeriInstance {
  constructor(private readonly readState: () => Promise<SyncState>) {
    super(state.chat.id, store, environment, unusedHost);
  }

  override read(): Promise<SyncState> { return this.readState(); }
}

function deferred<T>(): { promise: Promise<T>; resolve(value: T): void; reject(error: unknown): void } {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

describe("PeriInstance watcher 生命周期", () => {
  test("初始可回收", () => {
    expect(new TestInstance(async () => state).recyclable).toBe(true);
  });

  test("summary 只读内存态，不触发复原", async () => {
    const runtime = new TestInstance(async () => state);
    expect(runtime.summary()).toEqual({
      running: false, pendingInteractionCount: 0, lastTurnStatus: null, lastCompletedAt: null,
    });
  });

  test("watch 等待 read 期间不可回收", async () => {
    const gate = deferred<SyncState>();
    const runtime = new TestInstance(() => gate.promise);

    const watching = runtime.watch(() => {});
    expect(runtime.recyclable).toBe(false);

    gate.resolve(state);
    const subscription = await watching;
    expect(runtime.recyclable).toBe(false);

    subscription.close();
    expect(runtime.recyclable).toBe(true);
  });

  test("重复 close 不重复扣减", async () => {
    const runtime = new TestInstance(async () => state);
    const subscription = await runtime.watch(() => {});

    subscription.close();
    subscription.close();
    expect(runtime.recyclable).toBe(true);
  });

  test("read 失败后恢复可回收", async () => {
    const runtime = new TestInstance(async () => { throw new Error("read failed"); });

    await expect(runtime.watch(() => {})).rejects.toThrow("read failed");
    expect(runtime.recyclable).toBe(true);
  });
});
