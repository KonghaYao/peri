import { afterEach, expect, test } from "bun:test";
import { SessionDocs } from "../src/state/session-docs";
import { CHECKPOINT_VERSION, CheckpointImportError, type DisplayCheckpoint } from "../src/state/checkpoint";
import { readSessionView } from "../src/view/session-view";
import type { JsonRpcNotification } from "../src/transport/types";

const NOW = "2026-10-10T02:00:00.000Z";
let now = NOW;
const instances: SessionDocs[] = [];
afterEach(() => { instances.splice(0).forEach((docs) => docs.destroy()); now = NOW; });

function create(): SessionDocs {
    const docs = new SessionDocs({ now: () => now });
    instances.push(docs);
    return docs;
}
function update(sessionUpdate: string, fields: Record<string, unknown>): JsonRpcNotification {
    return { jsonrpc: "2.0", method: "session/update", params: { sessionId: "s1", update: { sessionUpdate, ...fields } } };
}

/** 一段完整会话：user → 文本 → 工具（大载荷外置）→ 终态 → 计划/任务/队列/已解决交互。 */
async function completedConversation(): Promise<SessionDocs> {
    const docs = create();
    docs.acceptDeliveredUserInput("u1", "read README");
    docs.accept(update("agent_message_chunk", { content: { type: "text", text: "Working" } }));
    docs.accept(update("tool_call", { toolCallId: "t1", title: "Read", status: "in_progress", rawInput: { path: "README.md" } }));
    docs.accept(update("tool_call_update", { toolCallId: "t1", status: "completed", rawOutput: "x".repeat(9000),
        content: [{ type: "text", text: "file body" }] }));
    docs.accept(update("plan", { entries: [{ content: "Inspect", status: "in_progress", _meta: { activeForm: "Inspecting" } }] }));
    docs.seedInputQueue({ generation: "g1", revision: 3, items: [{ inputId: "i1", state: "delivered" }] });
    docs.accept({ jsonrpc: "2.0", method: "peri/unstable_event", params: { sessionId: "s1", event: "bg-task-snapshot", data: {
        revision: 1, tasks: [{ task_id: "task-1", status: "completed", kind: "shell", summary: "Checked" }],
    } } });
    await docs.handleRequest("session/request_permission", {
        sessionId: "s1", toolCall: { toolCallId: "t1", title: "Read", rawInput: { path: "README.md" } },
        options: [{ optionId: "allow_once", name: "Allow" }, { optionId: "reject_once", name: "Reject" }],
    }, "s1", { id: "a", sandbox: {} as never, onPermissionRequest: () => "allow_once" });
    docs.completeTurn("completed", "end_turn");
    return docs;
}

test("checkpoint 往返保留 Entry/Tool/Turn/Interaction 材料与外置载荷全文", async () => {
    const docs = await completedConversation();
    const checkpoint = docs.exportCheckpoint();
    expect(checkpoint.checkpointVersion).toBe(CHECKPOINT_VERSION);
    expect(checkpoint.schemaVersions).toEqual({ chat: 3, session: 2 });

    const restored = SessionDocs.fromCheckpoint(checkpoint, { now: () => now });
    instances.push(restored);

    expect(restored.exportCheckpoint()).toEqual(checkpoint);
    const ref = checkpoint.toolCalls[0]!.resultRef!;
    expect(ref.bytes).toBeGreaterThan(4096);
    expect(restored.readPayload(ref.id)).toBe(docs.readPayload(ref.id));
    const view = readSessionView(restored.chat, restored.session);
    expect(view.entries.map((entry) => entry.entryId)).toEqual(["turn:u1:user", "turn:u1:assistant"]);
    expect(view.entries[1]!.blocks.map((block) => block.type)).toEqual(["text", "tool_call"]);
    expect(view.tasks[0]).toMatchObject({ taskId: "task-1", status: "completed" });
    expect(view.plansByTurn["turn:u1"]).toMatchObject([{ content: "Inspect" }]);
    expect(view.pendingInteractions).toEqual([]);
});

test("导入把活动 turn/interactions 收敛为中断与失效，且不伪造时间或状态", async () => {
    const docs = create();
    docs.acceptDeliveredUserInput("u1", "read");
    docs.accept(update("agent_message_chunk", { content: { type: "text", text: "partial" } }));
    docs.accept(update("tool_call", { toolCallId: "t1", title: "Read", status: "in_progress", rawInput: { path: "README.md" } }));
    void docs.handleRequest("session/request_permission", {
        sessionId: "s1", toolCall: { toolCallId: "t1", title: "Read", rawInput: { path: "README.md" } },
        options: [{ optionId: "allow_once", name: "Allow" }],
    }, "s1", { id: "a", sandbox: {} as never, onPermissionRequest: () => new Promise(() => {}) });
    await docs.handleRequest("elicitation/create", {
        sessionId: "s1", mode: "form", message: "Answer", requestedSchema: {
            properties: { secret: { title: "Secret", type: "string" } },
        },
    }, "s1", { id: "a", sandbox: {} as never, onElicitation: () => ({ action: "accept", content: { secret: "PRIVATE_ANSWER_MARKER" } }) });
    docs.accept({ jsonrpc: "2.0", method: "peri/unstable_event", params: { sessionId: "s1", event: "bg-task-snapshot", data: {
        revision: 1, tasks: [{ task_id: "task-live", status: "running", kind: "shell", summary: "Checking" }],
    } } });

    const restored = SessionDocs.fromCheckpoint(docs.exportCheckpoint(), { now: () => now });
    instances.push(restored);
    const checkpoint = restored.exportCheckpoint();
    expect(checkpoint.turns[0]).toMatchObject({ status: "error", error: { code: "execution_interrupted" }, completedAt: null });
    expect(checkpoint.entries[1]).toMatchObject({ status: "error", error: { code: "execution_interrupted" } });
    expect(checkpoint.toolCalls[0]).toMatchObject({ status: "cancelled", completedAt: null });
    const byKind = Object.fromEntries(checkpoint.interactions.map((item) => [item.kind, item]));
    expect(byKind.permission).toMatchObject({ status: "expired", resolvedAt: null });
    expect(byKind.elicitation).toMatchObject({ status: "resolved", outcome: "accept" });
    expect(checkpoint.tasks[0]).toMatchObject({ status: "unobserved:running" });
    expect(checkpoint.session.activeTurnId).toBeUndefined();
    const view = readSessionView(restored.chat, restored.session);
    expect(view.activeTurnId ?? null).toBeNull();
    expect(view.pendingInteractions).toEqual([]);
    expect(JSON.stringify({ chat: restored.chat.toJSON(), session: restored.session.toJSON() })).not.toContain("PRIVATE_ANSWER_MARKER");
});

test("导入拒绝未知版本、未知 schema pair、缺失与错配的载荷引用", async () => {
    const docs = await completedConversation();
    const checkpoint = docs.exportCheckpoint();
    const failing = (mutate: (draft: DisplayCheckpoint) => void): CheckpointImportError => {
        const draft = structuredClone(checkpoint);
        mutate(draft);
        try { SessionDocs.fromCheckpoint(draft); }
        catch (error) { return error as CheckpointImportError; }
        throw new Error("import should have failed");
    };
    expect(failing((draft) => { draft.checkpointVersion = 2; }).code).toBe("unsupported_version");
    expect(failing((draft) => { draft.schemaVersions = { chat: 2, session: 1 }; }).code).toBe("unsupported_schema");
    expect(failing((draft) => { draft.payloadValues = []; }).code).toBe("missing_payload");
    expect(failing((draft) => { draft.payloadValues[0]!.json += " "; }).code).toBe("broken_reference");
});

test("旧记录由应用转换后导入：保留旧 id 与时间并标记 legacy", () => {
    const legacy: DisplayCheckpoint = {
        checkpointVersion: CHECKPOINT_VERSION,
        schemaVersions: { chat: 3, session: 2 },
        legacyIncomplete: true,
        session: { title: "Imported" },
        entries: [{
            entryId: "legacy-1", id: "legacy-1", kind: "message", turnId: "legacy-turn", role: "user",
            status: "completed", createdAt: "2024-01-01T00:00:00.000Z", createdAtSource: "observed",
            completedAt: null, error: null, blocks: [{ blockId: "text:0", type: "text", text: "old question" }],
        }, {
            entryId: "legacy-2", id: "legacy-2", kind: "message", turnId: "legacy-turn", role: "assistant",
            status: "completed", createdAt: null, createdAtSource: "unknown",
            completedAt: null, error: null, blocks: [{ blockId: "text:0", type: "text", text: "old answer" }],
        }],
        toolCalls: [], turns: [], tasks: [], plansByTurn: {}, interactions: [], payloadValues: [],
    };
    const restored = SessionDocs.fromCheckpoint(legacy, { now: () => now });
    instances.push(restored);
    const view = readSessionView(restored.chat, restored.session);
    expect(view.entries[0]).toMatchObject({ entryId: "legacy-1", status: "completed" });
    expect(view.entries[0]!.blocks[0]).toMatchObject({ text: "old question" });
    const roundtrip = restored.exportCheckpoint();
    expect(roundtrip.entries[0]).toMatchObject({
        id: "legacy-1", createdAt: "2024-01-01T00:00:00.000Z", createdAtSource: "observed",
    });
    expect(roundtrip.entries[1]).toMatchObject({ id: "legacy-2", createdAt: null, createdAtSource: "unknown" });
    expect(roundtrip.legacyIncomplete).toBe(true);
    expect(restored.session.getMap("root").get("session").get("legacyIncomplete")).toBe(true);
});

test("导入产生独立文档，活跃文档不被改写", async () => {
    const docs = await completedConversation();
    const restored = SessionDocs.fromCheckpoint(docs.exportCheckpoint());
    instances.push(restored);
    expect(restored.chat).not.toBe(docs.chat);
    expect(restored.session).not.toBe(docs.session);
    restored.acceptDeliveredUserInput("u2", "another");
    expect(docs.exportCheckpoint().entries).toHaveLength(2);
    expect(restored.exportCheckpoint().entries).toHaveLength(4);
    expect(docs.chat.getMap("root").get("toolCalls").has("t1")).toBe(true);
});
