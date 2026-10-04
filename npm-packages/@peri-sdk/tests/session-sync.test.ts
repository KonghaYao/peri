import { expect, test } from "bun:test";
import * as Y from "yjs";
import { SessionDocs } from "../src/state/session-docs";
import { readSessionView, SessionViewStore } from "../src/view/session-view";
import { SessionDocSync, SessionDocReplica, SyncBackpressureError, type DocUpdate } from "../src/sync/index";

const update = (kind: string, fields: Record<string, unknown>) => ({ jsonrpc: "2.0" as const, method: "session/update",
    params: { sessionId: "s1", update: { sessionUpdate: kind, ...fields } } });

test("bounded binary batches preserve Unicode streaming, tool payload refs, and the terminal tail", async () => {
    const docs = new SessionDocs();
    const sync = new SessionDocSync(docs.chat, docs.session, { maxBatchBytes: 8192, maxBatchUpdates: 32 });
    const replica = new SessionDocReplica();
    const frames: DocUpdate[] = [];
    const connection = sync.subscribe((frame) => { frames.push(frame); replica.applyUpdate(frame); });
    replica.applySnapshot(connection.snapshot);
    docs.accept(update("user_message_chunk", { content: { text: "go" } }));
    const pieces = Array.from({ length: 1000 }, (_, index) => `片段 ${index} 🌙\n`);
    for (const text of pieces) docs.accept(update("agent_message_chunk", { content: { text } }));
    docs.accept(update("tool_call", { toolCallId: "large", status: "completed", rawOutput: "x".repeat(100_000) }));
    docs.completeTurn();
    await sync.close();
    const view = readSessionView(replica.chat, replica.session);
    expect(view.activeTurnStatus).toBe("completed");
    expect(view.entries[1]?.blocks[0]).toMatchObject({ text: pieces.join("") });
    const card = view.entries[1]?.blocks[1];
    expect(card?.type).toBe("tool_call");
    if (card?.type !== "tool_call") throw new Error("Missing tool card");
    expect(JSON.parse(docs.readPayload(card.tool!.resultRef!.id)!)).toEqual({ rawOutput: "x".repeat(100_000) });
    expect(frames.length).toBeLessThan(50);
    expect(frames.every((frame) => (frame.chat?.byteLength ?? 0) + (frame.session?.byteLength ?? 0) < 8192)).toBe(true);
    expect(() => sync.subscribe(() => {})).toThrow("closed");
    replica.destroy(); docs.destroy();
});

test("resume includes offline deletions even when the Yjs state vector did not advance", async () => {
    const chat = new Y.Doc(); const session = new Y.Doc();
    chat.getMap("root").set("obsolete", "private old text");
    const sync = new SessionDocSync(chat, session);
    const replica = new SessionDocReplica();
    const connection = sync.subscribe((frame) => replica.applyUpdate(frame));
    replica.applySnapshot(connection.snapshot);
    connection.unsubscribe();
    const vector = replica.stateVector()!;
    chat.getMap("root").delete("obsolete");
    expect(Y.encodeStateVector(chat)).toEqual(vector.chat);
    const resumed = sync.subscribe((frame) => replica.applyUpdate(frame), { resume: vector });
    expect(resumed.snapshot.mode).toBe("delta");
    const originalChat = replica.chat;
    replica.applySnapshot(resumed.snapshot);
    expect(replica.chat).toBe(originalChat);
    expect(replica.chat.getMap("root").has("obsolete")).toBe(false);
    resumed.unsubscribe(); await sync.close(); replica.destroy(); chat.destroy(); session.destroy();
});

test("a missing batch is detected and repaired from state vectors; duplicate frames are harmless", async () => {
    const chat = new Y.Doc(); const session = new Y.Doc();
    const sync = new SessionDocSync(chat, session);
    const replica = new SessionDocReplica();
    const frames: DocUpdate[] = [];
    const connection = sync.subscribe((frame) => { frames.push(frame); });
    replica.applySnapshot(connection.snapshot);
    chat.getText("text").insert(0, "one"); sync.flush();
    chat.getText("text").insert(3, "two"); sync.flush();
    expect(() => replica.applyUpdate(frames[1]!)).toThrow("sequence gap");
    replica.applyUpdate(frames[0]!); replica.applyUpdate(frames[0]!);
    connection.unsubscribe();
    const resumed = sync.subscribe((frame) => replica.applyUpdate(frame), { resume: replica.stateVector() });
    replica.applySnapshot(resumed.snapshot);
    expect(replica.chat.getText("text").toString()).toBe("onetwo");
    expect(() => replica.applyUpdate({ ...frames[1]!, generation: "other" })).toThrow("generation mismatch");
    resumed.unsubscribe(); await sync.close(); replica.destroy(); chat.destroy(); session.destroy();
});

test("Yjs V2 updates converge when a binary transport reorders and duplicates batches", async () => {
    const chat = new Y.Doc(); const session = new Y.Doc();
    const sync = new SessionDocSync(chat, session);
    const frames: DocUpdate[] = [];
    const { snapshot, unsubscribe } = sync.subscribe((frame) => { frames.push(frame); });
    const copy = new Y.Doc();
    Y.applyUpdateV2(copy, snapshot.chat);
    for (const text of ["甲", "乙", "🌙"]) { chat.getText("text").insert(chat.getText("text").length, text); sync.flush(); }
    for (const frame of [...frames].reverse()) { Y.applyUpdateV2(copy, frame.chat!); Y.applyUpdateV2(copy, frame.chat!); }
    expect(copy.getText("text").toString()).toBe("甲乙🌙");
    unsubscribe(); await sync.close(); copy.destroy(); chat.destroy(); session.destroy();
});

test("replacing a producer generation requires a fresh pair and detaches an existing view store", async () => {
    const first = new SessionDocs(); first.acceptDeliveredUserInput("one", "first");
    const firstSync = new SessionDocSync(first.chat, first.session);
    const replica = new SessionDocReplica();
    replica.applySnapshot(firstSync.subscribe(() => {}).snapshot);
    const store = new SessionViewStore(replica.chat, replica.session);
    const oldChat = replica.chat;
    const next = new SessionDocs(); next.acceptDeliveredUserInput("two", "second");
    const nextSync = new SessionDocSync(next.chat, next.session);
    const connection = nextSync.subscribe((frame) => replica.applyUpdate(frame), { resume: replica.stateVector() });
    expect(connection.snapshot.mode).toBe("snapshot");
    replica.applySnapshot(connection.snapshot);
    store.replaceDocs(replica.chat, replica.session);
    expect(replica.chat).not.toBe(oldChat);
    expect(store.getSnapshot().entries[0]?.blocks[0]).toMatchObject({ text: "second" });
    store.destroy(); connection.unsubscribe(); await firstSync.close(); await nextSync.close();
    replica.destroy(); first.destroy(); next.destroy();
});

test("slow subscribers exceed a bounded queue while fast subscribers remain current and resume works", async () => {
    const chat = new Y.Doc(); const session = new Y.Doc();
    const sync = new SessionDocSync(chat, session, { maxSubscriberBytes: 512 });
    let release!: () => void;
    let error: Error | undefined;
    sync.subscribe(() => new Promise<void>((resolve) => { release = resolve; }), { onError: (value) => { error = value; } });
    const replica = new SessionDocReplica();
    const fast = sync.subscribe((frame) => replica.applyUpdate(frame)); replica.applySnapshot(fast.snapshot);
    for (let i = 0; i < 20; i++) { chat.getText("text").insert(chat.getText("text").length, "x".repeat(100)); sync.flush(); }
    expect(error).toBeInstanceOf(SyncBackpressureError);
    expect(replica.chat.getText("text").length).toBe(2000);
    release();
    fast.unsubscribe(); await sync.close(); replica.destroy(); chat.destroy(); session.destroy();
});

test("close waits for an asynchronous transport to send queued tail updates", async () => {
    const chat = new Y.Doc(); const session = new Y.Doc();
    const sync = new SessionDocSync(chat, session);
    const replica = new SessionDocReplica();
    let release!: () => void;
    let first = true;
    const connection = sync.subscribe(async (frame) => {
        if (first) { first = false; await new Promise<void>((resolve) => { release = resolve; }); }
        replica.applyUpdate(frame);
    });
    replica.applySnapshot(connection.snapshot);
    chat.getText("text").insert(0, "head"); sync.flush();
    chat.getText("text").insert(4, "tail");
    let closed = false;
    const closing = sync.close().then(() => { closed = true; });
    await Promise.resolve(); expect(closed).toBe(false);
    release(); await closing;
    expect(replica.chat.getText("text").toString()).toBe("headtail");
    replica.destroy(); chat.destroy(); session.destroy();
});

test("malformed snapshots and invalid budgets fail without replacing a usable replica", async () => {
    const chat = new Y.Doc(); const session = new Y.Doc();
    expect(() => new SessionDocSync(chat, session, { maxBatchBytes: 0 })).toThrow("positive");
    const sync = new SessionDocSync(chat, session);
    const replica = new SessionDocReplica();
    const initial = sync.subscribe(() => {}).snapshot;
    replica.applySnapshot(initial);
    const previous = replica.chat;
    expect(() => replica.applySnapshot({ ...initial, session: new Uint8Array([255]) })).toThrow();
    expect(replica.chat).toBe(previous);
    expect(() => replica.applySnapshot({ ...initial, protocol: 1 } as never)).toThrow("Invalid Yjs sync frame");
    expect(() => sync.subscribe(() => {}, { resume: { ...replica.stateVector()!, chat: new Uint8Array(70_000) } })).toThrow("byte budget");
    await sync.close(); replica.destroy(); chat.destroy(); session.destroy();
});

test("a reentrant subscriber cannot send a later batch ahead of the current broadcast", async () => {
    const chat = new Y.Doc(); const session = new Y.Doc();
    const sync = new SessionDocSync(chat, session);
    const replica = new SessionDocReplica();
    sync.subscribe((frame) => {
        if (frame.sequence === 1) { chat.getText("text").insert(1, "B"); sync.flush(); }
    });
    const seen: number[] = [];
    const connection = sync.subscribe((frame) => { seen.push(frame.sequence); replica.applyUpdate(frame); });
    replica.applySnapshot(connection.snapshot);
    chat.getText("text").insert(0, "A"); sync.flush();
    expect(seen).toEqual([1, 2]); expect(replica.chat.getText("text").toString()).toBe("AB");
    await sync.close(); replica.destroy(); chat.destroy(); session.destroy();
});

test("closing from a send callback still delivers the current batch to every admitted peer", async () => {
    const chat = new Y.Doc(); const session = new Y.Doc();
    const sync = new SessionDocSync(chat, session);
    const replica = new SessionDocReplica();
    sync.subscribe(() => { void sync.close(); });
    const connection = sync.subscribe((frame) => replica.applyUpdate(frame)); replica.applySnapshot(connection.snapshot);
    chat.getText("text").insert(0, "tail"); sync.flush(); await sync.close();
    expect(replica.chat.getText("text").toString()).toBe("tail");
    replica.destroy(); chat.destroy(); session.destroy();
});

test("each subscriber can transfer its binary buffers to a Worker without corrupting another peer", async () => {
    const chat = new Y.Doc(); const session = new Y.Doc();
    const sync = new SessionDocSync(chat, session);
    const first = new SessionDocReplica(); const second = new SessionDocReplica();
    first.applySnapshot(sync.subscribe((frame) => {
        const transfer = [frame.chat?.buffer, frame.session?.buffer].filter(Boolean) as ArrayBuffer[];
        first.applyUpdate(structuredClone(frame, { transfer }));
    }).snapshot);
    second.applySnapshot(sync.subscribe((frame) => second.applyUpdate(frame)).snapshot);
    chat.getText("text").insert(0, "hello"); sync.flush();
    expect(first.chat.getText("text").toString()).toBe("hello");
    expect(second.chat.getText("text").toString()).toBe("hello");
    await sync.close(); first.destroy(); second.destroy(); chat.destroy(); session.destroy();
});

test("subscription admission fails if flushing an existing peer closes the producer", async () => {
    const chat = new Y.Doc(); const session = new Y.Doc();
    const sync = new SessionDocSync(chat, session);
    sync.subscribe(() => { void sync.close(); });
    chat.getText("text").insert(0, "pending");
    expect(() => sync.subscribe(() => {})).toThrow("Yjs sync is closed");
    await sync.close(); chat.destroy(); session.destroy();
});

test("a sender that unsubscribes before returning a rejected promise is still observed", async () => {
    const chat = new Y.Doc(); const session = new Y.Doc();
    const sync = new SessionDocSync(chat, session);
    let sent = 0;
    const connection = sync.subscribe(() => {
        sent++;
        connection.unsubscribe();
        return Promise.reject(new Error("Transport was aborted"));
    });
    chat.getText("text").insert(0, "tail"); sync.flush(); await sync.close();
    // An unhandled rejection during this tick is a test failure in Bun.
    await Bun.sleep(0);
    expect(sent).toBe(1);
    chat.destroy(); session.destroy();
});
