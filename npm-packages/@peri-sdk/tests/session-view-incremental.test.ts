import { expect, test } from "bun:test";
import * as Y from "yjs";
import { SessionDocs } from "../src/state/session-docs";
import { SessionViewStore, readSessionView } from "../src/view/session-view";

const update = (kind: string, fields: Record<string, unknown>) => ({ jsonrpc: "2.0" as const, method: "session/update",
    params: { sessionId: "s1", update: { sessionUpdate: kind, ...fields } } });

test("streaming reuses unchanged history and tools while previous snapshots stay immutable", async () => {
    const docs = new SessionDocs(); docs.acceptDeliveredUserInput("one", "read");
    for (let i = 0; i < 100; i++) docs.accept(update("tool_call", { toolCallId: `tool-${i}`, status: "completed", rawInput: { path: `${i}.ts` } }));
    const store = new SessionViewStore(docs.chat, docs.session);
    const previous = store.getSnapshot();
    docs.accept(update("agent_message_chunk", { messageId: "message", content: { text: "first" } }));
    await Promise.resolve();
    const current = store.getSnapshot();
    expect(current.entries[0]).toBe(previous.entries[0]);
    for (let i = 0; i < 100; i++) expect(current.entries[1]!.blocks[i]).toBe(previous.entries[1]!.blocks[i]);
    expect(previous.entries[1]!.blocks).toHaveLength(100);
    expect(Object.isFrozen(previous.entries[1]!.blocks[0])).toBe(true);
    docs.seedInputQueue({ generation: "g", items: [] }); await Promise.resolve();
    expect(store.getSnapshot().entries).toBe(current.entries);
    store.destroy(); docs.destroy();
});

test("tool reference edges invalidate only affected blocks across repeated remote updates", async () => {
    const docs = new SessionDocs(); docs.acceptDeliveredUserInput("one", "read");
    docs.accept(update("tool_call", { toolCallId: "first", status: "pending" }));
    docs.accept(update("tool_call", { toolCallId: "second", status: "pending" }));
    const chat = new Y.Doc(); const session = new Y.Doc();
    Y.applyUpdateV2(chat, Y.encodeStateAsUpdateV2(docs.chat)); Y.applyUpdateV2(session, Y.encodeStateAsUpdateV2(docs.session));
    docs.chat.on("updateV2", (bytes) => Y.applyUpdateV2(chat, bytes));
    const store = new SessionViewStore(chat, session);
    const before = store.getSnapshot();
    for (const status of ["in_progress", "completed"]) {
        docs.accept(update("tool_call_update", { toolCallId: "first", status, rawOutput: "result" }));
        await Promise.resolve();
        const current = store.getSnapshot();
        expect(current.entries[1]!.blocks[1]).toBe(before.entries[1]!.blocks[1]);
        expect(current).toEqual(readSessionView(chat, session));
        expect(current.entries[1]!.blocks[0]).toMatchObject({ tool: { status: status === "in_progress" ? "running" : "completed" } });
    }
    expect(before.entries[1]!.blocks[0]).toMatchObject({ tool: { status: "pending" } });
    store.destroy(); docs.destroy(); chat.destroy(); session.destroy();
});

test("tool replacement, deletion, and canonical rewind remove stale cached references", async () => {
    const docs = new SessionDocs(); docs.acceptDeliveredUserInput("one", "read");
    docs.accept(update("tool_call", { toolCallId: "tool", status: "pending" }));
    const store = new SessionViewStore(docs.chat, docs.session);
    const tools = docs.chat.getMap("root").get("toolCalls") as Y.Map<Y.Map<unknown>>;
    const replacement = new Y.Map<unknown>(); replacement.set("toolCallId", "tool"); replacement.set("name", "Changed");
    tools.set("tool", replacement); await Promise.resolve();
    expect(store.getSnapshot().entries[1]!.blocks[0]).toMatchObject({ tool: { name: "Changed" } });
    tools.delete("tool"); await Promise.resolve();
    expect(store.getSnapshot().entries[1]!.blocks[0]).toMatchObject({ tool: null });
    docs.accept({ jsonrpc: "2.0", method: "peri/agent_event", params: { event_json: JSON.stringify({
        type: "rewind_completed", value: { messages_json: JSON.stringify([{ role: "user", id: "retained", content: "new history" }]) },
    }) } });
    await Promise.resolve();
    expect(store.getSnapshot()).toEqual(readSessionView(docs.chat, docs.session));
    expect(store.getSnapshot().entries).toHaveLength(1);
    store.destroy(); docs.destroy();
});

test("task order changes and session updates invalidate the correct read-model collections", async () => {
    const docs = new SessionDocs();
    docs.accept({ jsonrpc: "2.0", method: "peri/unstable_event", params: { event: "bg-task-snapshot", data: {
        revision: 1, tasks: [{ task_id: "a", status: "running" }, { task_id: "b", status: "running" }],
    } } });
    const store = new SessionViewStore(docs.chat, docs.session);
    const order = docs.session.getMap("root").get("taskOrder") as Y.Array<string>;
    docs.session.transact(() => { order.delete(0, order.length); order.push(["b", "a"]); });
    await Promise.resolve();
    expect(store.getSnapshot().tasks.map((task) => task.taskId)).toEqual(["b", "a"]);
    expect(store.getSnapshot()).toEqual(readSessionView(docs.chat, docs.session));
    store.destroy(); docs.destroy();
});

test("destroy rejects every later projection entry point without resurrecting a turn", async () => {
    const docs = new SessionDocs();
    const before = readSessionView(docs.chat, docs.session);
    docs.destroy(); docs.destroy();
    expect(docs.acceptDeliveredUserInput("late", "ghost")).toBe(false);
    docs.accept(update("user_message_chunk", { content: { text: "ghost" } }));
    docs.acceptBatch([update("user_message_chunk", { content: { text: "ghost" } })]);
    docs.seedInputQueue({ generation: "late", items: [] });
    docs.seedConfig({ modes: { currentModeId: "late" } });
    docs.restoreCancel({ turnId: "late", previous: "running" });
    expect(docs.completeTurn()).toBe(false); expect(docs.requestCancel()).toBeNull();
    await expect(docs.handleRequest("session/request_permission", {}, undefined, {} as never)).rejects.toThrow("destroyed");
    expect(readSessionView(docs.chat, docs.session)).toEqual(before);
});

test("replacing the tools collection cannot leave cached blocks pointing at its previous values", async () => {
    const docs = new SessionDocs(); docs.acceptDeliveredUserInput("one", "read");
    docs.accept(update("tool_call", { toolCallId: "tool", title: "Before", status: "completed" }));
    const store = new SessionViewStore(docs.chat, docs.session);
    const tools = new Y.Map<Y.Map<unknown>>(); const tool = new Y.Map<unknown>();
    tool.set("toolCallId", "tool"); tool.set("name", "After"); tools.set("tool", tool);
    docs.chat.getMap("root").set("toolCalls", tools); await Promise.resolve();
    expect(store.getSnapshot().entries[1]!.blocks[0]).toMatchObject({ tool: { name: "After" } });
    store.destroy(); docs.destroy();
});
