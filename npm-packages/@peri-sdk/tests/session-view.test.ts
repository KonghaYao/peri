import { describe, expect, test } from "bun:test";
import * as Y from "yjs";
import { ChatDocument } from "../src/view/chat-document";
import { readSessionView, selectEntry, selectPendingInteractions, selectPlanForTurn, selectToolCall, selectTurn } from "../src/view/session-view";

function update(kind: string, fields: Record<string, unknown>) {
    return { jsonrpc: "2.0" as const, method: "session/update", params: {
        sessionId: "s1", update: { sessionUpdate: kind, ...fields },
    } };
}

describe("transport independent session view", () => {
    test("reads the same chat, tools, plan and task state from mirrored Y.Doc updates", () => {
        const source = new ChatDocument();
        source.accept(update("user_message_chunk", { content: { type: "text", text: "Build" } }));
        source.accept(update("agent_message_chunk", { content: { type: "text", text: "Done" } }));
        source.accept(update("tool_call", {
            toolCallId: "tool-1", title: "Read", status: "completed",
            arguments: { path: "README.md" }, result: { contentText: "File" },
        }));
        source.accept(update("plan", { entries: [{ content: "Inspect", status: "in_progress", _meta: { activeForm: "Inspecting" } }] }));
        source.accept({ jsonrpc: "2.0", method: "peri/unstable_event", params: { sessionId: "s1", event: "bg-task-snapshot", data: {
            revision: 1, tasks: [{ task_id: "task-1", status: "running", kind: "shell", summary: "Checking" }],
        } } });
        source.applyFact({ kind: "run-state", running: true, executionBlocked: false });
        const mirrorChat = new Y.Doc();
        const mirrorSession = new Y.Doc();
        Y.applyUpdate(mirrorChat, Y.encodeStateAsUpdate(source.chat));
        Y.applyUpdate(mirrorSession, Y.encodeStateAsUpdate(source.session));
        const view = readSessionView(mirrorChat, mirrorSession);
        expect(view.readiness).toBe("ready");
        expect(view.schemaVersions).toEqual({ chat: 3, session: 2 });
        expect(view.running).toBe(true);
        expect(view.executionBlocked).toBe(false);
        expect(view.closed).toBe(false);
        expect(view.closedReason).toBeNull();
        expect(view.entries.map((entry) => entry.role)).toEqual(["user", "assistant"]);
        expect(view.entries[0]?.blocks[0]).toMatchObject({ type: "text", text: "Build" });
        expect(view.entries[1]?.blocks.map((block) => block.type)).toEqual(["text", "tool_call"]);
        expect(view.entries[1]?.blocks[1]).toMatchObject({ toolCallId: "tool-1", tool: { name: "Read", status: "completed", arguments: { path: "README.md" }, result: { contentText: "File" } } });
        expect(view.plansByTurn[view.activeTurn.turnId!]?.[0]).toMatchObject({ activeForm: "Inspecting" });
        expect(view.tasks[0]).toMatchObject({ taskId: "task-1", title: "Checking", status: "unobserved:running" });
    });

    test("an empty browser replica is not-ready and creates no root keys", () => {
        const chat = new Y.Doc();
        const session = new Y.Doc();
        const view = readSessionView(chat, session);
        expect(view.readiness).toBe("not-ready");
        expect(view.schemaVersions).toEqual({ chat: null, session: null });
        expect(view.entries).toEqual([]);
        expect(view.tasks).toEqual([]);
        expect(view.pendingInteractions).toEqual([]);
        expect(view.activeTurn).toEqual({ turnId: null, status: null });
        expect(view.sessionId).toBeNull();
        expect(view.bindingRevision).toBeNull();
        expect(chat.share.size).toBe(0);
        expect(session.share.size).toBe(0);
    });

    test("a complete but unsupported pair reports unsupported and never pretends empty history", () => {
        const source = new ChatDocument();
        source.accept(update("user_message_chunk", { content: { type: "text", text: "Old" } }));
        const chat = new Y.Doc();
        const session = new Y.Doc();
        Y.applyUpdate(chat, Y.encodeStateAsUpdate(source.chat));
        Y.applyUpdate(session, Y.encodeStateAsUpdate(source.session));
        session.getMap("root").set("schemaVersion", 1);
        const view = readSessionView(chat, session);
        expect(view.readiness).toBe("unsupported");
        expect(view.schemaVersions).toEqual({ chat: 3, session: 1 });
        expect(view.entries).toEqual([]);
        // 只应用了一半的文档对属未就绪，不冒充不受支持。
        const partialChat = new Y.Doc();
        const partialSession = new Y.Doc();
        Y.applyUpdate(partialChat, Y.encodeStateAsUpdate(source.chat));
        expect(readSessionView(partialChat, partialSession).readiness).toBe("not-ready");
        source.destroy();
    });

    test("projects queue and interaction display fields without private answers", () => {
        const docs = new ChatDocument();
        docs.applyFact({ kind: "input-queue", snapshot: { generation: "g1", revision: 2, activeRequestId: "r1", items: [
            { inputId: "i1", state: "queued", content: "private draft" },
        ] } });
        docs.applyFact({ kind: "interaction-began", record: {
            id: "interaction:1", kind: "permission", sessionId: "s1", toolCallId: "t1", title: "Write",
            options: [{ optionId: "allow_once", name: "Allow" }, { optionId: "reject_once", name: "Reject" }],
        } });
        const view = readSessionView(docs.chat, docs.session);
        expect(view.inputQueue).toEqual({ generation: "g1", revision: 2, activeRequestId: "r1", items: [{ inputId: "i1", state: "queued" }] });
        expect(view.pendingInteractions[0]).toMatchObject({ kind: "permission", sessionId: "s1", toolCallId: "t1", options: [{ optionId: "allow_once" }, { optionId: "reject_once" }] });
        expect(JSON.stringify(view)).not.toContain("private input");
        expect(JSON.stringify(view)).not.toContain("private draft");
        docs.applyFact({ kind: "interaction-settled", id: "interaction:1", status: "resolved", outcome: "allow_once" });
        expect(readSessionView(docs.chat, docs.session).pendingInteractions).toEqual([]);
        expect(selectPendingInteractions(readSessionView(docs.chat, docs.session))).toEqual([]);
    });

    test("selectors distinguish missing identities from empty collections", () => {
        const docs = new ChatDocument();
        docs.accept(update("user_message_chunk", { content: { type: "text", text: "Ask" } }));
        docs.accept(update("agent_message_chunk", { content: { type: "text", text: "Answer" } }));
        docs.accept(update("tool_call", { toolCallId: "t1", title: "Read", status: "pending" }));
        docs.accept(update("plan", { entries: [{ content: "Inspect", status: "in_progress" }] }));
        const view = readSessionView(docs.chat, docs.session);
        const assistant = view.entries[1]!;
        expect(selectEntry(view, assistant.entryId)?.entryId).toBe(assistant.entryId);
        expect(selectEntry(view, "missing")).toBeNull();
        expect(selectToolCall(view, "t1")?.name).toBe("Read");
        expect(selectToolCall(view, "missing")).toBeNull();
        expect(selectTurn(view, view.activeTurn.turnId!)?.status).toBe("running");
        expect(selectTurn(view, "missing")).toBeNull();
        expect(selectPlanForTurn(view, view.activeTurn.turnId!)?.[0]).toMatchObject({ content: "Inspect" });
        expect(selectPlanForTurn(view, "missing")).toBeNull();
        docs.destroy();
    });
});
