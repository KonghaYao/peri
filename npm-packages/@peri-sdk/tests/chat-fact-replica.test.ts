import { describe, expect, test } from "bun:test";
import type { ChatBindingTarget, ChatFact, ChatFactEnvelope } from "../src/contracts/chat";
import { ChatFactReplica, type SessionView } from "../src/entries/view";
import type { JsonRpcNotification } from "../src/transport/types";

const TARGET: ChatBindingTarget = { chatId: "chat-1", sessionId: "s1", bindingRevision: 1 };

function update(kind: string, fields: Record<string, unknown>): JsonRpcNotification {
    return {
        jsonrpc: "2.0",
        method: "session/update",
        params: { sessionId: "s1", update: { sessionUpdate: kind, ...fields } },
    };
}

function user(text: string): ChatFact {
    return { kind: "input-delivered", inputId: `u-${text}`, text };
}

function assistant(messageId: string, text: string): ChatFact {
    return {
        kind: "notice",
        notification: update("agent_message_chunk", { messageId, content: { type: "text", text } }),
    };
}

function envelope(sequence: number, fact: ChatFact, bindingRevision = 1): ChatFactEnvelope {
    return { chatId: "chat-1", sessionId: "s1", bindingRevision, sequence, fact };
}

function texts(view: SessionView): string[] {
    return view.entries.flatMap((entry) => entry.blocks.flatMap((block) => (block.type === "text" ? [block.text] : [])));
}

async function flush(): Promise<void> {
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
}

describe("ChatFactReplica", () => {
    test("applies a snapshot and exposes a SessionView", async () => {
        const replica = new ChatFactReplica();
        expect(replica.status).toBe("connecting");
        expect(replica.binding).toBeNull();
        expect(replica.bindingRevision).toBe(0);
        expect(replica.read().readiness).toBe("not-ready");

        replica.applySnapshot({
            binding: TARGET,
            overflow: false,
            envelopes: [envelope(1, user("hello")), envelope(2, assistant("a1", "world"))],
        });
        expect(replica.status).toBe("synced");
        expect(replica.binding).toEqual(TARGET);
        expect(replica.bindingRevision).toBe(1);

        await flush();
        const view = replica.read();
        expect(view.readiness).toBe("ready");
        expect(view.sessionId).toBe("s1");
        expect(view.bindingRevision).toBe(1);
        expect(view.entries.map((entry) => entry.role)).toEqual(["user", "assistant"]);
        expect(view.entries[0]?.blocks[0]).toMatchObject({ type: "text", text: "hello" });
        expect(replica.read()).toBe(view);
    });

    test("dedupes replayed increments by sequence", async () => {
        const replica = new ChatFactReplica({ binding: TARGET });
        replica.applySnapshot({
            binding: TARGET,
            overflow: false,
            envelopes: [envelope(1, user("hello"))],
        });
        await flush();

        expect(replica.applyFact(envelope(1, user("duplicate")))).toBe(false);
        expect(replica.applyFact(envelope(2, assistant("a1", "world")))).toBe(true);
        expect(replica.applyFact(envelope(2, assistant("a1", "world")))).toBe(false);

        await flush();
        expect(replica.read().entries).toHaveLength(2);
        expect(texts(replica.read())).toEqual(["hello", "world"]);
    });

    test("isolates generations when the binding revision changes", async () => {
        const replica = new ChatFactReplica();
        replica.applySnapshot({
            binding: TARGET,
            overflow: false,
            envelopes: [envelope(1, user("old")), envelope(2, assistant("a1", "old answer"))],
        });
        await flush();
        expect(texts(replica.read())).toEqual(["old", "old answer"]);

        const next = { ...TARGET, bindingRevision: 2 };
        replica.applySnapshot({
            binding: next,
            overflow: false,
            envelopes: [envelope(1, user("stale"), 1), envelope(1, user("fresh"), 2)],
        });
        await flush();

        expect(replica.bindingRevision).toBe(2);
        expect(replica.read().bindingRevision).toBe(2);
        expect(texts(replica.read())).toEqual(["fresh"]);

        expect(replica.applyFact(envelope(5, assistant("a1", "late old"), 1))).toBe(false);
        expect(replica.applyFact(envelope(2, assistant("a2", "fresh answer"), 2))).toBe(true);
        await flush();
        expect(texts(replica.read())).toEqual(["fresh", "fresh answer"]);
    });

    test("marks overflow as resynced and applies only current-generation facts", async () => {
        const replica = new ChatFactReplica();
        replica.applySnapshot({
            binding: TARGET,
            overflow: true,
            envelopes: [envelope(1, user("kept")), envelope(1, user("foreign"), 2)],
        });
        expect(replica.status).toBe("resynced");
        await flush();
        expect(texts(replica.read())).toEqual(["kept"]);

        expect(replica.applyFact(envelope(2, assistant("a1", "live")))).toBe(true);
        expect(replica.status).toBe("resynced");
        await flush();
        expect(texts(replica.read())).toEqual(["kept", "live"]);

        replica.applySnapshot({ binding: TARGET, overflow: false, envelopes: [] });
        expect(replica.status).toBe("synced");
        await flush();
        expect(texts(replica.read())).toEqual(["kept", "live"]);
    });

    test("rejects late facts after the session is closed", async () => {
        const replica = new ChatFactReplica({ binding: TARGET });
        replica.applySnapshot({
            binding: TARGET,
            overflow: false,
            envelopes: [
                envelope(1, user("hello")),
                envelope(2, { kind: "closed", reason: { code: "execution_interrupted", message: "执行已中断" } }),
            ],
        });
        await flush();
        expect(replica.read().closed).toBe(true);

        expect(replica.applyFact(envelope(3, assistant("a1", "too late")))).toBe(false);
        await flush();
        expect(texts(replica.read())).toEqual(["hello"]);
    });

    test("coalesces subscriber notifications and updates the view first", async () => {
        const replica = new ChatFactReplica();
        const seen: number[] = [];
        const unsubscribe = replica.subscribe(() => seen.push(replica.read().entries.length));

        replica.applySnapshot({
            binding: TARGET,
            overflow: false,
            envelopes: [envelope(1, user("hello"))],
        });
        replica.applyFact(envelope(2, assistant("a1", "world")));
        expect(seen).toEqual([]);

        await flush();
        expect(seen).toEqual([2]);

        unsubscribe();
        expect(replica.applyFact(envelope(3, assistant("a2", "more")))).toBe(true);
        await flush();
        expect(seen).toEqual([2]);
        expect(replica.read().entries).toHaveLength(3);
    });

    test("reports lifecycle states and stops after dispose", async () => {
        const replica = new ChatFactReplica({ binding: TARGET });
        expect(replica.binding).toEqual(TARGET);
        expect(replica.bindingRevision).toBe(1);
        expect(replica.status).toBe("connecting");
        expect(replica.read().readiness).toBe("ready");

        let calls = 0;
        replica.subscribe(() => { calls++; });

        replica.fail(new Error("stream broke"));
        expect(replica.status).toBe("failed");
        await flush();
        expect(calls).toBe(1);

        replica.reset();
        expect(replica.status).toBe("connecting");
        expect(replica.binding).toBeNull();
        expect(replica.bindingRevision).toBe(0);
        expect(replica.read().readiness).toBe("not-ready");
        await flush();
        expect(calls).toBe(2);

        replica.applySnapshot({
            binding: TARGET,
            overflow: false,
            envelopes: [envelope(1, user("hello"))],
        });
        await flush();
        expect(calls).toBe(3);

        replica.dispose();
        expect(replica.binding).toBeNull();
        expect(replica.bindingRevision).toBe(0);
        expect(replica.applyFact(envelope(2, assistant("a1", "late")))).toBe(false);
        replica.applySnapshot({
            binding: TARGET,
            overflow: false,
            envelopes: [envelope(1, user("after dispose"))],
        });
        await flush();
        expect(calls).toBe(3);
        expect(replica.read().readiness).toBe("not-ready");
    });
});
