import { afterEach, expect, test } from "bun:test";
import * as Y from "yjs";
import { DocModel } from "../src/state/doc-model";
import { SessionDocs } from "../src/state/session-docs";
import { InteractionProjection } from "../src/state/interaction-projection";
import {
    readSessionView, selectEntry, selectPendingInteractions, selectPlanForTurn, selectToolCall, selectTurn,
} from "../src/view/session-view";
import type { JsonRpcNotification } from "../src/transport/types";

const NOW = "2026-10-10T03:00:00.000Z";
let now = NOW;
const instances: SessionDocs[] = [];
afterEach(() => { instances.splice(0).forEach((docs) => docs.destroy()); now = NOW; });
function create() {
    const docs = new SessionDocs({ now: () => now });
    instances.push(docs);
    return docs;
}
function update(sessionUpdate: string, fields: Record<string, unknown>): JsonRpcNotification {
    return { jsonrpc: "2.0", method: "session/update", params: { sessionId: "s1", update: { sessionUpdate, ...fields } } };
}
function mark<T>(docs: { accept(notification: JsonRpcNotification): void }, messageId: string, text: string, chunks = 1): void {
    for (let index = 0; index < chunks; index++) {
        docs.accept(update("agent_message_chunk", { messageId, content: { type: "text", text: index === 0 ? text : "" } }));
    }
}

test("视图暴露完整 Entry/Tool/Turn/Interaction 与 schemaVersions", async () => {
    const docs = create();
    docs.acceptDeliveredUserInput("u", "read README");
    mark(docs, "a1", "Working");
    docs.accept(update("tool_call", { toolCallId: "t1", title: "Read", status: "in_progress", rawInput: { path: "README.md" } }));
    await docs.handleRequest("session/request_permission", {
        sessionId: "s1", toolCall: { toolCallId: "t1", title: "Read", rawInput: { path: "README.md" } },
        options: [{ optionId: "allow_once", name: "Allow" }],
    }, "s1", { id: "a", sandbox: {} as never, onPermissionRequest: () => "allow_once" });
    docs.accept(update("tool_call_update", { toolCallId: "t1", status: "completed", rawOutput: { lines: 2 } }));
    docs.completeTurn("completed", "end_turn");
    const view = readSessionView(docs.chat, docs.session);
    expect(view.readiness).toBe("ready");
    expect(view.schemaVersions).toEqual({ chat: 3, session: 2 });

    const assistant = view.entries[1]!;
    expect(assistant).toMatchObject({ id: assistant.entryId, kind: "message", status: "completed", error: null });
    expect(assistant.createdAt).toBe(NOW);
    expect(assistant.createdAtSource).toBe("observed");
    expect(assistant.completedAt).toBe(NOW);
    expect(assistant.blocks.map((block) => block.type)).toEqual(["text", "tool_call"]);

    const tool = view.toolCalls[0]!;
    expect(tool).toMatchObject({
        toolCallId: "t1", entryId: assistant.entryId, status: "completed", name: "Read",
        createdAt: NOW, completedAt: NOW, error: null,
    });
    expect(typeof tool.permissionId).toBe("string");
    expect(tool.permissionId!.startsWith("interaction:")).toBe(true);

    const turn = view.turns[0]!;
    expect(turn).toMatchObject({ turnId: "turn:u", status: "completed", interactionStatus: "none", stopReason: "end_turn" });
    expect(turn.startedAt).toBe(NOW);
    expect(view.activeTurn).toEqual({ turnId: "turn:u", status: "completed" });

    expect(view.interactions).toHaveLength(1);
    expect(view.interactions[0]).toMatchObject({ status: "resolved", outcome: "allow_once", toolCallId: "t1" });
    expect(view.pendingInteractions).toEqual([]);
});

test("选择器按身份查询，且区分缺失与空集合", () => {
    const docs = create();
    docs.acceptDeliveredUserInput("u", "read");
    mark(docs, "a1", "Answer");
    docs.accept(update("plan", { entries: [{ content: "Inspect", status: "in_progress" }] }));
    const view = readSessionView(docs.chat, docs.session);
    const entry = view.entries[1]!;
    expect(selectEntry(view, entry.entryId)).toMatchObject({ entryId: entry.entryId });
    expect(selectEntry(view, entry.id)).toMatchObject({ entryId: entry.entryId });
    expect(selectEntry(view, "missing")).toBeNull();
    expect(selectTurn(view, "turn:u")?.status).toBe("running");
    expect(selectTurn(view, "turn:other")).toBeNull();
    expect(selectPlanForTurn(view, "turn:u")).toMatchObject([{ content: "Inspect" }]);
    expect(selectPlanForTurn(view, "turn:other")).toBeNull();
    expect(selectPendingInteractions(view)).toEqual([]);
    expect(selectToolCall(view, "missing")).toBeNull();
});

test("未知块与孤立工具引用给出明确诊断，不静默吞块", () => {
    const model = new DocModel(() => now);
    const entry = model.ensureEntry("turn:1:assistant", "turn:1", "assistant");
    const blocks = entry.get("blocks") as Y.Map<Y.Map<unknown>>;
    const order = entry.get("blockOrder") as Y.Array<string>;
    const resource = new Y.Map<unknown>();
    resource.set("blockId", "resource:0");
    resource.set("type", "resource");
    resource.set("resourceId", "res-1");
    blocks.set("resource:0", resource);
    order.push(["resource:0"]);
    const orphan = new Y.Map<unknown>();
    orphan.set("blockId", "tool:ghost");
    orphan.set("type", "tool_call");
    orphan.set("toolCallId", "ghost");
    blocks.set("tool:ghost", orphan);
    order.push(["tool:ghost"]);
    const view = readSessionView(model.chat, model.session);
    expect(view.readiness).toBe("ready");
    expect(view.entries[0]!.blocks).toEqual([
        { blockId: "resource:0", type: "unknown", reason: "unsupported_block:resource" },
        { blockId: "tool:ghost", type: "tool_call", toolCallId: "ghost", tool: null, unavailable: "missing_tool" },
    ]);
});

test("交互终态在视图与派生列表中同步更新，不支持的表单直接拒绝", async () => {
    const docs = create();
    docs.acceptDeliveredUserInput("u", "read");
    mark(docs, "a1", "Working");
    // 不支持的表单：不产生 pending（没有可回答的界面），立即按拒绝结算并保留原因。
    const unsupported = docs.handleRequest("elicitation/create", {
        sessionId: "s1", message: "Answer", requestedSchema: { properties: { secret: { type: "object" } } },
    }, "s1", { id: "a", sandbox: {} as never, onElicitation: () => ({ action: "accept", content: { secret: {} } }) });
    expect(await unsupported).toEqual({ action: "decline" });
    const afterUnsupported = readSessionView(docs.chat, docs.session);
    expect(afterUnsupported.pendingInteractions).toEqual([]);
    expect(afterUnsupported.interactions[0]).toMatchObject({
        status: "resolved", outcome: "decline", formUnsupported: "unsupported_property_type",
    });

    // 支持的表单：pending → 回答 → 结算，派生列表同步。
    const request = docs.handleRequest("elicitation/create", {
        sessionId: "s1", message: "Answer", requestedSchema: { properties: { answer: { type: "string", title: "Answer" } } },
    }, "s1", { id: "a", sandbox: {} as never, onElicitation: () => ({ action: "accept", content: { answer: "yes" } }) });
    const pendingView = readSessionView(docs.chat, docs.session);
    expect(pendingView.pendingInteractions[0]).toMatchObject({ status: "pending", kind: "elicitation" });
    expect(selectPendingInteractions(pendingView)).toHaveLength(1);
    expect(await request).toEqual({ action: "accept", content: { answer: "yes" } });
    const settled = readSessionView(docs.chat, docs.session);
    expect(settled.pendingInteractions).toEqual([]);
    expect(settled.interactions.at(-1)).toMatchObject({ status: "resolved", outcome: "accept" });
});
