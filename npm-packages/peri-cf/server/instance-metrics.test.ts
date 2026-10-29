import { describe, expect, test } from "bun:test";
import type {
  ChatBindingTarget,
  ChatFact,
  ChatFactEnvelope,
  ChatFactObserver,
  ChatFactSource,
  ChatServiceContract,
} from "@peri-code/sdk";
import { ChatInstanceMetricsCollector } from "./instance-metrics";

const TARGET: ChatBindingTarget = {
  chatId: "chat-1",
  sessionId: "session-1",
  bindingRevision: 1,
};

class FakeFactSource implements ChatFactSource {
  binding: ChatBindingTarget | null = TARGET;
  private readonly observers = new Set<ChatFactObserver>();
  private sequence = 0;

  subscribeFacts(observer: ChatFactObserver): () => void {
    this.observers.add(observer);
    if (this.binding) observer.onBinding(this.binding, []);
    return () => this.observers.delete(observer);
  }

  bind(target: ChatBindingTarget, replay: ChatFact[] = []): void {
    this.binding = target;
    const envelopes: ChatFactEnvelope[] = replay.map((fact) => ({
      ...target,
      sequence: ++this.sequence,
      fact,
    }));
    for (const observer of [...this.observers]) observer.onBinding(target, envelopes);
  }

  emit(fact: ChatFact): void {
    if (!this.binding) return;
    const envelope: ChatFactEnvelope = {
      ...this.binding,
      sequence: ++this.sequence,
      fact,
    };
    for (const observer of [...this.observers]) observer.onFact(envelope);
  }
}

function fakeChat(id: string, source: FakeFactSource): ChatServiceContract {
  return {
    id,
    sessionId: TARGET.sessionId,
    bindingRevision: TARGET.bindingRevision,
    binding: TARGET,
    execute(): never { throw new Error("unused"); },
    readPayload(): Promise<never> { throw new Error("unused"); },
    async close(): Promise<void> {},
    subscribeFacts: (observer) => source.subscribeFacts(observer),
  };
}

function usage(update: Record<string, unknown>, meta: Record<string, unknown> = {}): ChatFact {
  return {
    kind: "notice",
    notification: {
      jsonrpc: "2.0",
      method: "session/update",
      params: { update: { sessionUpdate: "usage_update", ...update }, _meta: meta },
    },
  };
}

function toolCall(id: string, status: string, extra: Record<string, unknown> = {}): ChatFact {
  return {
    kind: "notice",
    notification: {
      jsonrpc: "2.0",
      method: "session/update",
      params: { update: { sessionUpdate: "tool_call", toolCallId: id, status, ...extra } },
    },
  };
}

function toolUpdate(id: string, status: string, extra: Record<string, unknown> = {}): ChatFact {
  return {
    kind: "notice",
    notification: {
      jsonrpc: "2.0",
      method: "session/update",
      params: { update: { sessionUpdate: "tool_call_update", toolCallId: id, status, ...extra } },
    },
  };
}

function setup() {
  let now = 1_000;
  const collector = new ChatInstanceMetricsCollector(() => now);
  const source = new FakeFactSource();
  const chat = fakeChat(TARGET.chatId, source);
  collector.track(chat);
  return {
    collector,
    source,
    chat,
    setNow: (value: number) => { now = value; },
    snapshot: (at = now) => collector.snapshot(TARGET.chatId, at),
  };
}

describe("ChatInstanceMetricsCollector", () => {
  test("回合开始/完成与墙钟，只统计已结算回合", () => {
    const { source, setNow, snapshot } = setup();
    setNow(1_000);
    source.emit({ kind: "run-state", running: true, executionBlocked: false });
    setNow(4_000);
    source.emit({ kind: "turn-complete", status: "completed", stopReason: "end_turn" });

    setNow(5_000);
    source.emit({ kind: "run-state", running: true, executionBlocked: false });
    const active = snapshot(6_500);
    expect(active?.running).toBe(true);
    expect(active?.turns).toMatchObject({
      started: 2,
      completed: 1,
      cancelled: 0,
      error: 0,
      totalWallMs: 3_000,
      lastWallMs: 3_000,
      activeWallMs: 1_500,
    });
    expect(active?.lastStopReason).toBe("end_turn");

    setNow(6_500);
    source.emit({ kind: "turn-complete", status: "error", stopReason: "SECRET_PROMPT" });
    const settled = snapshot(6_500);
    expect(settled?.turns.error).toBe(1);
    expect(settled?.turns.lastWallMs).toBe(1_500);
    expect(settled?.lastStopReason).toBeNull();
  });

  test("usage_update 累计 token、区分主/子代理并保留 context", () => {
    const { source, snapshot } = setup();
    source.emit(usage({ used: 100, size: 1_000, _meta: { inputTokens: 10, outputTokens: 5 } }));
    source.emit(usage(
      { used: 999, size: 9_999, _meta: { inputTokens: 20, outputTokens: 7 } },
      { peri: { sourceAgentId: "sub-1" } },
    ));
    source.emit(usage({ used: 120, size: 1_000 }));

    const business = snapshot();
    expect(business?.tokens).toEqual({
      llmCalls: 3,
      inputTokens: 30,
      outputTokens: 12,
      auxiliaryCalls: 1,
      partial: true,
    });
    expect(business?.context).toEqual({ used: 120, size: 1_000 });
  });

  test("tool_call 终态去重，重复终态不重复计数", () => {
    const { source, snapshot } = setup();
    source.emit(toolCall("tool-1", "pending"));
    source.emit(toolCall("tool-2", "pending"));
    source.emit(toolUpdate("tool-1", "completed"));
    source.emit(toolUpdate("tool-1", "completed"));
    source.emit(toolUpdate("tool-2", "failed"));

    expect(snapshot()?.tools).toEqual({
      started: 2,
      completed: 1,
      failed: 1,
      cancelled: 0,
    });
  });

  test("队列深度与待交互计数", () => {
    const { source, snapshot } = setup();
    source.emit({
      kind: "input-queue",
      snapshot: {
        generation: "g1",
        items: [
          { inputId: "a", state: "queued" },
          { inputId: "b", state: "delivered" },
          { inputId: "c", state: "running" },
        ],
      },
    });
    source.emit({
      kind: "interaction-began",
      record: { id: "i-1", kind: "permission", sessionId: TARGET.sessionId },
    });
    let business = snapshot();
    expect(business?.queueDepth).toBe(2);
    expect(business?.pendingInteractions).toBe(1);

    source.emit({ kind: "interaction-settled", id: "i-1", status: "resolved" });
    business = snapshot();
    expect(business?.pendingInteractions).toBe(0);
  });

  test("onBinding 重置计数，replay 只恢复运行态与队列/交互", () => {
    const { source, snapshot } = setup();
    source.emit({ kind: "run-state", running: true, executionBlocked: false });
    source.emit({ kind: "turn-complete", status: "completed" });
    source.emit(usage({ used: 1, size: 10, _meta: { inputTokens: 1, outputTokens: 1 } }));

    source.bind(TARGET, [
      { kind: "turn-complete", status: "completed", stopReason: "end_turn" },
      usage({ used: 5, size: 100, _meta: { inputTokens: 5, outputTokens: 5 } }),
      { kind: "run-state", running: true, executionBlocked: false },
      {
        kind: "input-queue",
        snapshot: { generation: "g2", items: [{ inputId: "x", state: "queued" }] },
      },
      {
        kind: "interaction-began",
        record: { id: "i-2", kind: "permission", sessionId: TARGET.sessionId },
      },
    ]);

    const business = snapshot(2_000);
    expect(business).toMatchObject({
      running: true,
      queueDepth: 1,
      pendingInteractions: 1,
      turns: { started: 0, completed: 0, lastWallMs: null, activeWallMs: null },
      tokens: null,
      context: null,
      lastStopReason: null,
    });
  });

  test("快照不包含 prompt、消息、工具标题/参数/结果或错误原文", () => {
    const { source, snapshot } = setup();
    source.emit(usage(
      { used: 10, size: 100, _meta: { inputTokens: 1, outputTokens: 1 } },
      { peri: { sourceAgentId: "SECRET_AGENT" } },
    ));
    source.emit(toolCall("tool-secret", "pending", {
      title: "SECRET_TOOL_TITLE",
      rawInput: { token: "SECRET_TOOL_ARGUMENT" },
    }));
    source.emit(toolUpdate("tool-secret", "failed", {
      content: { text: "SECRET_TOOL_RESULT" },
      error: { message: "SECRET_ERROR_TEXT" },
    }));
    source.emit({
      kind: "notice",
      notification: {
        jsonrpc: "2.0",
        method: "session/update",
        params: {
          update: {
            sessionUpdate: "agent_message_chunk",
            content: { text: "SECRET_PROMPT" },
          },
        },
      },
    });

    const serialized = JSON.stringify(snapshot());
    for (const secret of ["SECRET_PROMPT", "SECRET_TOOL_TITLE", "SECRET_TOOL_ARGUMENT",
      "SECRET_TOOL_RESULT", "SECRET_ERROR_TEXT", "SECRET_AGENT"]) {
      expect(serialized).not.toContain(secret);
    }
  });

  test("退订后不再计数，未 track 的 Chat 返回 null", () => {
    const { collector, source, chat, snapshot } = setup();
    source.emit({ kind: "run-state", running: true, executionBlocked: false });
    collector.forget(chat.id);
    expect(snapshot()).toBeNull();
    source.emit({ kind: "turn-complete", status: "completed" });
    expect(collector.snapshot(chat.id, 2_000)).toBeNull();
  });
});
