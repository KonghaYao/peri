import { afterEach, expect, test } from "bun:test";
import * as Y from "yjs";
import { DocModel } from "../src/state/doc-model";
import { ChatDocument } from "../src/view/chat-document";
import { evaluateElicitationForm } from "../src/state/interaction-projection";
import {
    readSessionView, selectEntry, selectPendingInteractions, selectPlanForTurn, selectToolCall, selectTurn,
} from "../src/view/session-view";
import type { JsonRpcNotification } from "../src/transport/types";

const NOW = "2026-10-10T03:00:00.000Z";
let now = NOW;
const instances: ChatDocument[] = [];
afterEach(() => { instances.splice(0).forEach((docs) => docs.destroy()); now = NOW; });
function create() {
    const docs = new ChatDocument({ now: () => now });
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

test("视图暴露完整 Entry/Tool/Turn/Interaction 与 schemaVersions", () => {
    const docs = create();
    docs.acceptDeliveredUserInput("u", "read README");
    mark(docs, "a1", "Working");
    docs.accept(update("tool_call", { toolCallId: "t1", title: "Read", status: "in_progress", arguments: { path: "README.md" } }));
    docs.applyFact({ kind: "interaction-began", record: {
        id: "interaction:1", kind: "permission", sessionId: "s1", toolCallId: "t1", title: "Read",
        options: [{ optionId: "allow_once", name: "Allow" }],
    } });
    docs.applyFact({ kind: "interaction-settled", id: "interaction:1", status: "resolved", outcome: "allow_once" });
    docs.accept(update("tool_call_update", { toolCallId: "t1", status: "completed", result: { contentText: "lines: 2" } }));
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
        createdAt: NOW, completedAt: NOW, error: null, arguments: { path: "README.md" },
        result: { contentText: "lines: 2" },
    });
    expect(tool.permissionId).toBe("interaction:1");

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
    model.destroy();
});

test("ask_user 事实保留问题结构，普通表单与权限不互相冒充", () => {
    const docs = create();
    docs.applyFact({ kind: "interaction-began", record: {
        id: "interaction:ask", kind: "elicitation", sessionId: "s1", variant: "ask_user",
        questions: [
            {
                id: "ask_user_question_0", header: "Language", question: "Which language?", multiSelect: false,
                options: [{ value: "ts", label: "TypeScript", description: "typed" }, { value: "rs", label: "Rust" }],
            },
            {
                id: "ask_user_question_1", header: "Features", question: "Pick features", multiSelect: true,
                options: [{ value: "a", label: "A" }, { value: "b", label: "B" }],
            },
        ],
    } });
    docs.applyFact({ kind: "interaction-began", record: {
        id: "interaction:form", kind: "elicitation", sessionId: "s1", message: "Answer",
        fields: [{ key: "answer", title: "Answer", description: "Free text", type: "string", required: true }],
    } });
    docs.applyFact({ kind: "interaction-began", record: {
        id: "interaction:permission", kind: "permission", sessionId: "s1", title: "Run",
        options: [{ optionId: "allow_once", name: "Allow" }],
    } });

    const view = readSessionView(docs.chat, docs.session);
    const pending = selectPendingInteractions(view);
    const ask = pending.find((interaction) => interaction.id === "interaction:ask")!;
    expect(ask.variant).toBe("ask_user");
    expect(ask.fields).toEqual([]);
    expect(ask.options).toBeUndefined();
    expect(ask.questions).toHaveLength(2);
    expect(ask.questions![0]).toEqual({
        id: "ask_user_question_0", header: "Language", question: "Which language?", multiSelect: false,
        options: [{ value: "ts", label: "TypeScript", description: "typed" }, { value: "rs", label: "Rust" }],
    });
    expect(ask.questions![1]!.multiSelect).toBe(true);
    expect(ask.questions![1]!.options.map((option) => option.value)).toEqual(["a", "b"]);

    const form = pending.find((interaction) => interaction.id === "interaction:form")!;
    expect(form.questions).toBeUndefined();
    expect(form.options).toBeUndefined();
    expect(form.fields).toEqual([{ key: "answer", title: "Answer", description: "Free text", type: "string", required: true }]);

    const permission = pending.find((interaction) => interaction.id === "interaction:permission")!;
    expect(permission.questions).toBeUndefined();
    expect(permission.fields).toBeUndefined();
    expect(permission.options).toEqual([{ optionId: "allow_once", name: "Allow" }]);
});

test("不支持的表单保留原因，交互展示只随事实结算", () => {
    const docs = create();
    docs.acceptDeliveredUserInput("u", "read");
    mark(docs, "a1", "Working");
    // 表单窗口是纯校验函数；不支持的表单不产生可回答的界面。
    const form = evaluateElicitationForm({ properties: { secret: { type: "object" } } });
    expect(form).toMatchObject({ supported: false, reason: "unsupported_property_type" });
    docs.applyFact({ kind: "interaction-began", record: {
        id: "interaction:1", kind: "elicitation", sessionId: "s1", message: "Answer",
        fields: form.fields, formSupport: form.supported ? undefined : "unsupported", formReason: form.reason,
    } });
    docs.applyFact({ kind: "interaction-settled", id: "interaction:1", status: "resolved", outcome: "decline" });
    const settled = readSessionView(docs.chat, docs.session);
    expect(settled.pendingInteractions).toEqual([]);
    expect(settled.interactions.at(-1)).toMatchObject({
        status: "resolved", outcome: "decline", formUnsupported: "unsupported_property_type",
    });

    // 支持的表单：pending → 运行侧结算 → 派生列表同步。
    const supported = evaluateElicitationForm({ properties: { answer: { type: "string", title: "Answer" } } });
    expect(supported.supported).toBe(true);
    docs.applyFact({ kind: "interaction-began", record: {
        id: "interaction:2", kind: "elicitation", sessionId: "s1", message: "Answer", fields: supported.fields,
    } });
    const pending = readSessionView(docs.chat, docs.session);
    expect(pending.pendingInteractions[0]).toMatchObject({ status: "pending", kind: "elicitation" });
    expect(selectPendingInteractions(pending)).toHaveLength(1);
    docs.applyFact({ kind: "interaction-settled", id: "interaction:2", status: "resolved", outcome: "accept" });
    const after = readSessionView(docs.chat, docs.session);
    expect(after.pendingInteractions).toEqual([]);
    expect(after.interactions.at(-1)).toMatchObject({ status: "resolved", outcome: "accept" });
});
