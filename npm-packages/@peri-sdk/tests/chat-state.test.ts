import { describe, expect, test } from "bun:test";
import type { ChatBindingTarget, ChatFact, ChatFactEnvelope } from "../src/contracts/chat";
import type { JsonRpcNotification } from "../src/transport/types";
import {
    ChatState,
    ChatStateInputError,
    type ChatStateSnapshot,
} from "../src/entries/view";

const TARGET: ChatBindingTarget = { chatId: "chat-1", sessionId: "s1", bindingRevision: 1 };
const NEXT: ChatBindingTarget = { chatId: "chat-1", sessionId: "s2", bindingRevision: 2 };

function update(kind: string, fields: Record<string, unknown>): JsonRpcNotification {
    return {
        jsonrpc: "2.0",
        method: "session/update",
        params: { sessionId: "s1", update: { sessionUpdate: kind, ...fields } },
    };
}

function user(text: string, inputId = `u-${text}`): ChatFact {
    return { kind: "input-delivered", inputId, text };
}

function assistant(messageId: string, text: string): ChatFact {
    return {
        kind: "notice",
        notification: update("agent_message_chunk", { messageId, content: { type: "text", text } }),
    };
}

function runState(running: boolean): ChatFact {
    return { kind: "run-state", running, executionBlocked: false };
}

function turnComplete(status: "completed" | "cancelled" | "error" = "completed", stopReason?: string): ChatFact {
    return { kind: "turn-complete", status, ...(stopReason === undefined ? {} : { stopReason }) };
}

function envelope(sequence: number, fact: ChatFact, binding = TARGET): ChatFactEnvelope {
    return {
        chatId: binding.chatId,
        sessionId: binding.sessionId,
        bindingRevision: binding.bindingRevision,
        sequence,
        fact,
    };
}

function snapshot(
    envelopes: ChatFactEnvelope[] = [],
    binding: ChatBindingTarget | null = TARGET,
    overflow = false,
) {
    return { binding, envelopes, overflow };
}

function texts(snapshot: ChatStateSnapshot): string[] {
    return snapshot.messages.map((message) => message.text);
}

async function flush(): Promise<void> {
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
}

describe("ChatState", () => {
    test("empty() starts unbound and accepts an empty facts snapshot", async () => {
        const state = ChatState.empty();
        expect(state.snapshot.readiness).toBe("not-ready");
        expect(state.snapshot.running).toBe(false);
        expect(state.snapshot.messages).toEqual([]);

        state.apply(snapshot([], null));
        await flush();

        expect(state.snapshot.readiness).toBe("not-ready");
        expect(state.snapshot.messages).toEqual([]);
    });

    test("applies a facts snapshot and live increments into messages", async () => {
        const state = ChatState.empty();
        state.apply(snapshot([envelope(1, user("你好")), envelope(2, assistant("a1", "世界"))]));
        await flush();

        expect(state.snapshot.readiness).toBe("ready");
        expect(state.snapshot.messages).toEqual([
            { entryId: "turn:u-你好:user", role: "user", text: "你好" },
            { entryId: "turn:u-你好:assistant", role: "assistant", text: "世界" },
        ]);

        state.apply(envelope(3, assistant("a1", "！")));
        await flush();
        expect(texts(state.snapshot)).toEqual(["你好", "世界！"]);
    });

    test("dedupes replayed increments and keeps content notifications coalesced", async () => {
        const state = ChatState.empty();
        const seen: string[][] = [];
        state.subscribe((current) => seen.push(texts(current)));
        state.apply(snapshot());
        await flush();
        seen.length = 0;

        state.apply(envelope(1, user("a")));
        state.apply(envelope(2, assistant("a1", "b")));
        state.apply(envelope(3, assistant("a1", "c")));
        expect(seen).toEqual([]);
        await flush();
        expect(seen).toEqual([["a", "bc"]]);

        state.apply(envelope(2, assistant("a1", "重复")));
        await flush();
        expect(seen).toEqual([["a", "bc"]]);
    });

    test("late facts and other generations cannot rewrite the active generation", async () => {
        const state = ChatState.empty();
        state.apply(snapshot([envelope(1, user("old")), envelope(2, assistant("a1", "answer"))]));
        await flush();

        state.apply(snapshot([
            envelope(1, user("stale"), TARGET),
            envelope(1, user("fresh"), NEXT),
        ], NEXT));
        await flush();
        expect(state.snapshot.bindingRevision).toBe(2);
        expect(texts(state.snapshot)).toEqual(["fresh"]);

        state.apply(envelope(5, assistant("a2", "late old"), TARGET));
        await flush();
        expect(texts(state.snapshot)).toEqual(["fresh"]);
    });

    test("snapshots are frozen and cannot mutate the state", async () => {
        const state = ChatState.empty();
        state.apply(snapshot([envelope(1, user("hello"))]));
        await flush();
        const current = state.snapshot;

        expect(() => {
            (current.messages as unknown as Array<unknown>).push({ entryId: "x", role: "user", text: "x" });
        }).toThrow(TypeError);
        expect(() => {
            (current as { running: boolean }).running = true;
        }).toThrow(TypeError);
        expect(() => {
            (current.entries as unknown as Array<unknown>).pop();
        }).toThrow(TypeError);

        expect(state.snapshot.running).toBe(false);
        expect(texts(state.snapshot)).toEqual(["hello"]);
        expect(state.snapshot.messages).toHaveLength(1);
    });

    test("a fast turn still exposes running true before its terminal state", async () => {
        const state = ChatState.empty();
        const running: boolean[] = [];
        let turnStarted = false;
        let finishTurn: (() => void) | undefined;
        const done = new Promise<void>((resolve) => {
            finishTurn = resolve;
        });
        state.subscribe((current) => {
            running.push(current.running);
            if (current.running) {
                turnStarted = true;
            } else if (turnStarted) {
                turnStarted = false;
                finishTurn?.();
                finishTurn = undefined;
            }
        });

        state.apply(snapshot());
        await flush();
        running.length = 0;

        // 真实事件形状；一轮在同一个 tick 内开始并结束，microtask 合并仍须保留 true。
        state.apply(envelope(1, user("ping")));
        state.apply(envelope(2, runState(true)));
        state.apply(envelope(3, assistant("a1", "pong")));
        state.apply(envelope(4, turnComplete("completed", "end_turn")));
        state.apply(envelope(5, runState(false)));

        await Promise.race([
            done,
            new Promise<never>((_, reject) => setTimeout(() => reject(new Error("turn wait hung")), 1_000)),
        ]);

        expect(running).toEqual([true, false]);
        expect(state.snapshot.running).toBe(false);
        expect(state.snapshot.activeTurnStatus).toBe("completed");
        expect(texts(state.snapshot)).toEqual(["ping", "pong"]);
    });

    test("a failed turn settles with an error status and running false", async () => {
        const state = ChatState.empty();
        state.apply(snapshot());
        await flush();

        state.apply(envelope(1, user("boom")));
        state.apply(envelope(2, runState(true)));
        state.apply(envelope(3, turnComplete("error", "error")));
        state.apply(envelope(4, runState(false)));
        await flush();

        expect(state.snapshot.running).toBe(false);
        expect(state.snapshot.turns.at(-1)).toMatchObject({ status: "error", stopReason: "error" });
    });

    test("cancelled turns settle and closed is terminal for late facts", async () => {        const state = ChatState.empty();
        state.apply(snapshot());
        await flush();

        state.apply(envelope(1, user("stop")));
        state.apply(envelope(2, runState(true)));
        state.apply(envelope(3, { kind: "turn-cancel", restore: false }));
        state.apply(envelope(4, turnComplete("cancelled", "cancelled")));
        state.apply(envelope(5, runState(false)));
        await flush();

        expect(state.snapshot.running).toBe(false);
        expect(state.snapshot.turns.at(-1)).toMatchObject({ status: "cancelled", stopReason: "cancelled" });

        state.apply(envelope(6, {
            kind: "closed",
            reason: { code: "execution_interrupted", message: "执行已中断" },
        }));
        await flush();
        expect(state.snapshot.closed).toBe(true);

        state.apply(envelope(7, assistant("a2", "too late")));
        await flush();
        expect(texts(state.snapshot)).toEqual(["stop"]);
    });

    test("unsubscribe stops notifications but keeps the latest snapshot readable", async () => {
        const state = ChatState.empty();
        let calls = 0;
        const unsubscribe = state.subscribe(() => {
            calls += 1;
        });

        state.apply(snapshot([envelope(1, user("one"))]));
        await flush();
        expect(calls).toBe(1);

        unsubscribe();
        state.apply(envelope(2, assistant("a1", "two")));
        await flush();
        expect(calls).toBe(1);
        expect(texts(state.snapshot)).toEqual(["one", "two"]);
    });

    test("facts before the first snapshot are dropped without inventing a binding", async () => {
        const state = ChatState.empty();
        let calls = 0;
        state.subscribe(() => {
            calls += 1;
        });

        state.apply(envelope(1, user("early")));
        await flush();

        expect(calls).toBe(0);
        expect(state.snapshot.readiness).toBe("not-ready");
        expect(state.snapshot.messages).toEqual([]);
    });

    test("public snapshots are deeply frozen, including nested entries and turns", async () => {
        const state = ChatState.empty();
        state.apply(
            snapshot([
                envelope(1, user("hi")),
                envelope(2, assistant("a1", "hello")),
                envelope(3, turnComplete("completed")),
            ]),
        );
        await flush();

        const current = state.snapshot;
        expect(current.entries.length).toBeGreaterThan(0);
        expect(Object.isFrozen(current)).toBe(true);
        expect(Object.isFrozen(current.entries)).toBe(true);
        expect(Object.isFrozen(current.entries[0])).toBe(true);
        expect(Object.isFrozen(current.entries[0]?.blocks)).toBe(true);
        expect(Object.isFrozen(current.turns)).toBe(true);
        expect(Object.isFrozen(current.turns[0])).toBe(true);
        expect(Object.isFrozen(current.messages)).toBe(true);
        expect(Object.isFrozen(current.messages[0])).toBe(true);

        const entries = current.entries as unknown as Array<{ role: string }>;
        expect(() => entries.push(entries[0]!)).toThrow(TypeError);
        expect(() => {
            entries[0]!.role = "user";
        }).toThrow(TypeError);
        const turns = current.turns as unknown as unknown[];
        expect(() => turns.splice(0, 1)).toThrow(TypeError);
        const messages = current.messages as unknown as Array<{ text: string }>;
        expect(() => {
            messages[0]!.text = "changed";
        }).toThrow(TypeError);
    });

    test("subscribers receive the same deeply frozen snapshot", async () => {
        const state = ChatState.empty();
        const received: ChatStateSnapshot[] = [];
        state.subscribe((value) => received.push(value));
        state.apply(snapshot([envelope(1, user("one"))]));
        await flush();

        expect(received).toHaveLength(1);
        expect(received[0]).toBe(state.snapshot);
        expect(Object.isFrozen(received[0]?.entries[0]?.blocks)).toBe(true);
    });

    test("rejects malformed snapshots and envelopes at the boundary", () => {
        const state = ChatState.empty();
        const cases: Array<[unknown, string]> = [
            [{ binding: TARGET, envelopes: [], overflow: "no" }, "invalid_snapshot"],
            [{ binding: TARGET, envelopes: "nope", overflow: false }, "invalid_snapshot"],
            [{ binding: { ...TARGET, bindingRevision: -1 }, envelopes: [], overflow: false }, "invalid_binding"],
            [{ binding: null, envelopes: [envelope(1, user("x"))], overflow: false }, "invalid_snapshot"],
            [{ chatId: "chat-1", sessionId: "s1", bindingRevision: 1, sequence: 1, fact: { kind: "unknown" } }, "invalid_envelope"],
            [{ chatId: "chat-1", sessionId: "s1", bindingRevision: 1, sequence: 1.5, fact: runState(true) }, "invalid_envelope"],
        ];

        for (const [input, reason] of cases) {
            expect(() => state.apply(input as never)).toThrow(ChatStateInputError);
            try {
                state.apply(input as never);
            } catch (error) {
                expect((error as ChatStateInputError).reason).toBe(reason);
            }
        }
        expect(state.snapshot.readiness).toBe("not-ready");
    });
});
