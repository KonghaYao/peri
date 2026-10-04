import { describe, expect, test } from "bun:test";
import * as Y from "yjs";
import { SessionDocs } from "../src/state/session-docs";
import { readSessionView, SessionViewStore } from "../src/view/session-view";

function update(kind: string, fields: Record<string, unknown>) {
    return { jsonrpc: "2.0" as const, method: "session/update", params: {
        sessionId: "s1", update: { sessionUpdate: kind, ...fields },
    } };
}

describe("transport independent session view", () => {
    test("reads the same chat, tools, plan and task state from mirrored Y.Doc updates", () => {
        const source = new SessionDocs();
        source.accept(update("user_message_chunk", { content: { type: "text", text: "Build" } }));
        source.accept(update("agent_message_chunk", { content: { type: "text", text: "Done" } }));
        source.accept(update("tool_call", { toolCallId: "tool-1", title: "Read", status: "completed", rawInput: { path: "README.md" }, content: [{ type: "text", text: "File" }] }));
        source.accept(update("plan", { entries: [{ content: "Inspect", status: "in_progress", _meta: { activeForm: "Inspecting" } }] }));
        source.accept({ jsonrpc: "2.0", method: "peri/unstable_event", params: { sessionId: "s1", event: "bg-task-snapshot", data: {
            revision: 1, tasks: [{ task_id: "task-1", status: "running", kind: "shell", summary: "Checking" }],
        } } });
        const mirrorChat = new Y.Doc();
        const mirrorSession = new Y.Doc();
        Y.applyUpdate(mirrorChat, Y.encodeStateAsUpdate(source.chat));
        Y.applyUpdate(mirrorSession, Y.encodeStateAsUpdate(source.session));
        const view = readSessionView(mirrorChat, mirrorSession);
        expect(view.entries.map((entry) => entry.role)).toEqual(["user", "assistant"]);
        expect(view.entries[0]?.blocks[0]).toMatchObject({ type: "text", text: "Build" });
        expect(view.entries[1]?.blocks.map((block) => block.type)).toEqual(["text", "tool_call"]);
        expect(view.entries[1]?.blocks[1]).toMatchObject({ toolCallId: "tool-1", tool: { name: "Read", status: "completed", arguments: { path: "README.md" }, result: { contentText: "File" } } });
        expect(view.plansByTurn[view.activeTurnId!]?.[0]).toMatchObject({ activeForm: "Inspecting" });
        expect(view.tasks[0]).toMatchObject({ taskId: "task-1", title: "Checking", status: "running" });
    });

    test("store batches two document notifications, replaces docs, and detaches old docs", async () => {
        const first = new SessionDocs();
        const replacement = new SessionDocs();
        const store = new SessionViewStore(first.chat, first.session);
        const seen: string[][] = [];
        const unsubscribe = store.subscribe(() => seen.push(store.getSnapshot().entries.map((entry) => entry.role)));
        first.accept(update("user_message_chunk", { content: { type: "text", text: "One" } }));
        await Promise.resolve();
        expect(seen).toEqual([["user", "assistant"]]);
        store.replaceDocs(replacement.chat, replacement.session);
        expect(store.getSnapshot().entries).toEqual([]);
        expect(seen.at(-1)).toEqual([]);
        const count = seen.length;
        first.accept(update("user_message_chunk", { content: { type: "text", text: "Old" } }));
        await Promise.resolve();
        expect(seen).toHaveLength(count);
        replacement.accept(update("user_message_chunk", { content: { type: "text", text: "New" } }));
        await Promise.resolve();
        expect(store.getSnapshot().entries[0]?.blocks[0]).toMatchObject({ text: "New" });
        unsubscribe();
        store.destroy();
    });

    test("an empty browser replica publishes one view after both remote updates", async () => {
        const source = new SessionDocs();
        source.accept(update("user_message_chunk", { content: { type: "text", text: "Remote" } }));
        const chat = new Y.Doc();
        const session = new Y.Doc();
        const store = new SessionViewStore(chat, session);
        let notifications = 0;
        store.subscribe(() => notifications++);
        Y.applyUpdate(chat, Y.encodeStateAsUpdate(source.chat));
        Y.applyUpdate(session, Y.encodeStateAsUpdate(source.session));
        await Promise.resolve();
        expect(notifications).toBe(1);
        expect(store.getSnapshot().entries[0]?.blocks[0]).toMatchObject({ text: "Remote" });
        expect(store.getSnapshot().activeTurnId).toBeTruthy();
        store.destroy();
    });

    test("projects queue and pending interaction metadata without exposing private answers", async () => {
        const docs = new SessionDocs();
        docs.seedInputQueue({ generation: "g1", revision: 2, activeRequestId: "r1", items: [
            { inputId: "i1", state: "queued", content: "private draft" },
        ] });
        let answer!: (value: "allow_once") => void;
        const pending = docs.handleRequest("session/request_permission", {
            sessionId: "s1", toolCall: { toolCallId: "t1", title: "Write", rawInput: { secret: "private input" } },
            options: [{ optionId: "allow_once", name: "Allow" }, { optionId: "reject_once", name: "Reject" }],
        }, "s1", {
            id: "a", sandbox: {} as never,
            onPermissionRequest: () => new Promise((resolve) => { answer = resolve; }),
        });
        const view = readSessionView(docs.chat, docs.session);
        expect(view.inputQueue).toEqual({ generation: "g1", revision: 2, activeRequestId: "r1", items: [{ inputId: "i1", state: "queued" }] });
        expect(view.pendingInteractions[0]).toMatchObject({ kind: "permission", sessionId: "s1", toolCallId: "t1", options: [{ optionId: "allow_once" }, { optionId: "reject_once" }] });
        expect(JSON.stringify(view)).not.toContain("private input");
        expect(JSON.stringify(view)).not.toContain("private draft");
        answer("allow_once");
        await pending;
        expect(readSessionView(docs.chat, docs.session).pendingInteractions).toEqual([]);
    });

    test("empty peer docs have an empty safe view", () => {
        const chat = new Y.Doc();
        const session = new Y.Doc();
        const view = readSessionView(chat, session);
        expect(view.entries).toEqual([]);
        expect(view.tasks).toEqual([]);
        expect(view.pendingInteractions).toEqual([]);
        expect(view.activeTurnId).toBeNull();
        expect(chat.share.size).toBe(0);
        expect(session.share.size).toBe(0);
    });
});
