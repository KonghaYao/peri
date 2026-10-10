import { afterEach, expect, test } from "bun:test";
import * as Y from "yjs";
import { SessionDocs, type SessionDocsOptions } from "../src/state/session-docs";
import {
    SESSION_EXTERNAL_PAYLOAD_BUDGET_BYTES, SESSION_INLINE_PAYLOAD_BUDGET_BYTES,
    TOOL_PAYLOAD_INLINE_BYTES, TOOL_PAYLOAD_MAX_BYTES, TOOL_PAYLOAD_PREVIEW_CHARS,
} from "../src/state/tool-payloads";
import type { JsonRpcNotification } from "../src/transport/types";

// ACP fixture 留在本文件，禁止把真实会话或凭据录入测试。
const CREATED = "2026-10-10T01:00:00.000Z";
const COMPLETED = "2026-10-10T01:00:01.000Z";
let now = CREATED;
const instances: SessionDocs[] = [];
function create(options: SessionDocsOptions = {}) {
    const docs = new SessionDocs({ now: () => now, ...options });
    instances.push(docs);
    return docs;
}
afterEach(() => { instances.splice(0).forEach((docs) => docs.destroy()); now = CREATED; });
function update(sessionUpdate: string, fields: Record<string, unknown>): JsonRpcNotification {
    return { jsonrpc: "2.0", method: "session/update", params: {
        sessionId: "s1", update: { sessionUpdate, ...fields },
    } };
}
function text(text: string, messageId?: string, thought = false) {
    return update(thought ? "agent_thought_chunk" : "agent_message_chunk", {
        content: { type: "text", text }, ...(messageId ? { messageId } : {}),
    });
}
function history(messages: unknown[]): JsonRpcNotification {
    return { jsonrpc: "2.0", method: "peri/agent_event", params: {
        sessionId: "s1", event_json: JSON.stringify({ type: "compact_completed", value: {
            messages_json: JSON.stringify(messages),
        } }),
    } };
}
function entries(docs: SessionDocs): Y.Map<unknown>[] {
    const root = docs.chat.getMap("root");
    const map = root.get("entries") as Y.Map<Y.Map<unknown>>;
    return (root.get("entryOrder") as Y.Array<string>).toArray().map((id) => map.get(id)!);
}
function tool(docs: SessionDocs, id = "t1") {
    return (docs.chat.getMap("root").get("toolCalls") as Y.Map<Y.Map<unknown>>).get(id)!;
}
function blocks(entry: Y.Map<unknown>) {
    const map = entry.get("blocks") as Y.Map<Y.Map<unknown>>;
    return (entry.get("blockOrder") as Y.Array<string>).toArray().map((id) => {
        const block = map.get(id)!;
        return block.get("type") === "tool_call" ? ["tool_call", block.get("toolCallId")]
            : [block.get("type"), String(block.get("text"))];
    });
}

test("交错 ACP 与 canonical 历史产生相同有序块，工具结果晚到不重排", () => {
    const live = create(); live.acceptDeliveredUserInput("u1", "read");
    live.acceptBatch([text("A", "a1"), text("R", "a1", true),
        update("tool_call", { toolCallId: "t1", status: "in_progress" }),
        update("tool_call", { toolCallId: "t2", status: "in_progress" }), text("B", "a1"),
        update("tool_call_update", { toolCallId: "t2", status: "completed" }),
        update("tool_call_update", { toolCallId: "t1", status: "completed" })]);
    const replay = create();
    replay.accept(history([{ id: "u1", role: "user", content: "read" }, {
        id: "a1", role: "assistant", content: [
            { type: "text", text: "A" }, { type: "reasoning", text: "R" },
            { type: "tool_use", id: "t1" }, { type: "tool_use", id: "t2" }, { type: "text", text: "B" },
        ],
    }]));
    const expected = [["text", "A"], ["reasoning", "R"], ["tool_call", "t1"], ["tool_call", "t2"], ["text", "B"]];
    expect(blocks(entries(live)[1]!)).toEqual(expected);
    expect(blocks(entries(replay)[1]!)).toEqual(expected);
});

test("晚到 messageId 不改变 Entry 展示身份或创建时间，不依赖应用 metadata", () => {
    const docs = create(); docs.acceptDeliveredUserInput("u1", "read");
    docs.accept(text("A")); const entry = entries(docs)[1]!;
    const id = entry.get("entryId");
    now = COMPLETED; docs.accept(text("B", "a1"));
    expect(entry.get("id")).toBe(id);
    expect(entry.get("kind")).toBe("message");
    expect(entry.get("createdAt")).toBe(CREATED);
    expect(entry.get("createdAtSource")).toBe("observed");
    expect(entry.get("messageId")).toBe("a1");
});

test("同 turn 多 Entry 取消不污染已完成 Entry，重复终态不刷新时间", () => {
    const docs = create(); docs.acceptDeliveredUserInput("u1", "read");
    docs.accept(text("A", "a1")); now = COMPLETED;
    docs.accept(text("B", "a2"));
    const first = entries(docs)[1]!;
    expect(first.get("completedAt")).toBe(COMPLETED);
    now = "2026-10-10T01:00:02.000Z";
    docs.completeTurn("cancelled"); const second = entries(docs)[2]!;
    const ended = second.toJSON();
    now = "2026-10-10T01:00:03.000Z";
    expect(docs.completeTurn("error")).toBe(false);
    expect(first.get("status")).toBe("completed");
    expect(first.get("completedAt")).toBe(COMPLETED);
    expect(first.get("error")).toBeNull();
    expect(second.get("status")).toBe("cancelled");
    expect(second.toJSON()).toEqual(ended);
});

test("同 turn 前序已完成消息的迟到 chunk 不切换当前 Entry", () => {
    const docs = create(); docs.acceptDeliveredUserInput("u", "read");
    docs.accept(text("A", "a1")); docs.accept(text("B", "a2"));
    const before = docs.chat.toJSON();
    docs.accept(text("late", "a1"));
    expect(docs.chat.toJSON()).toEqual(before);
    expect(docs.session.getMap("root").get("session").get("activeAssistantEntryId")).toBe("turn:u:assistant:a2");
});

test("缺失工具 status 的补丁不推进 pending，终态补元数据不改变完成时间", () => {
    const docs = create(); docs.acceptDeliveredUserInput("u1", "read");
    docs.accept(update("tool_call", { toolCallId: "t1", status: "pending" }));
    docs.accept(update("tool_call_update", { toolCallId: "t1", title: "Read" }));
    expect(tool(docs).get("status")).toBe("pending");
    now = COMPLETED;
    docs.accept(update("tool_call_update", { toolCallId: "t1", status: "completed" }));
    now = "2026-10-10T01:00:02.000Z";
    docs.accept(update("tool_call_update", { toolCallId: "t1", title: "Read complete", rawOutput: null }));
    expect(tool(docs).get("name")).toBe("Read complete");
    expect(tool(docs).get("result")).toEqual({ rawOutput: null });
    expect(tool(docs).get("completedAt")).toBe(COMPLETED);
});

test("旧 turn 的工具 ID 不得被新 turn 重绑或更新", () => {
    const docs = create(); docs.acceptDeliveredUserInput("u1", "read");
    docs.accept(update("tool_call", { toolCallId: "t1", status: "completed", rawOutput: "first" }));
    const before = tool(docs).toJSON(); docs.completeTurn();
    docs.acceptDeliveredUserInput("u2", "next");
    docs.accept(update("tool_call_update", { toolCallId: "t1", status: "completed", rawOutput: "late" }));
    expect(tool(docs).toJSON()).toEqual(before);
    expect(blocks(entries(docs)[3]!)).toEqual([]);
    expect(() => docs.accept(update("tool_call", { toolCallId: "t1", status: "pending" })))
        .toThrow("tool_id_reused");
});

test("历史未知时间不得使用当前时钟，hidden reasoning 与 system prompt 不写 Yjs", () => {
    const docs = create();
    docs.accept(history([{ id: "s", role: "system", content: "PRIVATE_SYSTEM_MARKER" },
        { id: "u", role: "user", content: "read" }, { id: "a", role: "assistant", content: [
            { type: "reasoning", visibility: "hidden", text: "PRIVATE_REASONING_MARKER" },
            { type: "provider_private", text: "PRIVATE_PROVIDER_MARKER" }, { type: "text", text: "public" },
        ] }]));
    expect(entries(docs).map((entry) => [entry.get("createdAt"), entry.get("createdAtSource"), entry.get("completedAt")]))
        .toEqual([[null, "unknown", null], [null, "unknown", null]]);
    expect(JSON.stringify(docs.chat.toJSON())).not.toContain("PRIVATE_");
});

test("live hidden reasoning 不写入正文，退出后晚到块不创建幽灵对象", () => {
    const docs = create(); docs.acceptDeliveredUserInput("u", "read");
    docs.accept(update("agent_thought_chunk", { content: { type: "text", visibility: "hidden", text: "PRIVATE_REASONING_MARKER" } }));
    expect(blocks(entries(docs)[1]!)).toEqual([]);
    docs.completeTurn(); const before = docs.chat.toJSON();
    docs.accept(text("late")); docs.accept(update("tool_call", { toolCallId: "late" }));
    expect(docs.chat.toJSON()).toEqual(before);
});

test("原始失败诊断不进入公开 error，失败只归未完成 Entry", () => {
    const docs = create(); docs.acceptDeliveredUserInput("u", "read");
    docs.accept(text("A", "a1")); docs.accept(text("B", "a2")); now = COMPLETED;
    docs.accept({ jsonrpc: "2.0", method: "peri/agent_event", params: { sessionId: "s1",
        event_json: JSON.stringify({ type: "agent_execution_failed", value: { error: "PRIVATE_DIAGNOSTIC_MARKER" } }),
    } });
    expect(entries(docs)[1]!.get("error")).toBeNull();
    expect(entries(docs)[2]!.get("error")).toEqual({ code: "execution_failed", message: "执行失败" });
    expect(JSON.stringify(docs.chat.toJSON())).not.toContain("PRIVATE_DIAGNOSTIC_MARKER");
});

// 旧 DO 文本回放不能保留展示 id/time 的缺口已由 checkpoint import 承接；
// 正例与 converged 边界见 tests/checkpoint.test.ts（含只存 Yjs 丢 payload 全文的替代验证）。

test("turn 记录随生命周推进，终态保留 stopReason 且不重复收敛", () => {
    const docs = create(); docs.acceptDeliveredUserInput("u", "read");
    const turn = (): Y.Map<unknown> => (docs.session.getMap("root").get("turns") as Y.Map<Y.Map<unknown>>).get("turn:u")!;
    expect(turn().get("status")).toBe("accepting");
    expect(turn().get("startedAt")).toBe(CREATED);
    docs.accept(text("A")); now = COMPLETED;
    expect(turn().get("status")).toBe("running");
    docs.accept({ jsonrpc: "2.0", method: "peri/agent_event_done", params: { sessionId: "s1", stopReason: "end_turn" } });
    expect(turn().toJSON()).toMatchObject({ status: "completed", completedAt: COMPLETED, stopReason: "end_turn", error: null });
    expect(turn().get("interactionStatus")).toBe("none");
    expect(docs.completeTurn()).toBe(false);
    expect(turn().get("completedAt")).toBe(COMPLETED);
});

test("取消与失败分别写 cancelled 与 execution_failed，重放 turn 无伪造开始时间", () => {
    const docs = create(); docs.acceptDeliveredUserInput("u", "read");
    const turns = docs.session.getMap("root").get("turns") as Y.Map<Y.Map<unknown>>;
    const token = docs.requestCancel()!;
    expect(turns.get("turn:u")!.get("status")).toBe("cancelling");
    docs.restoreCancel(token);
    expect(turns.get("turn:u")!.get("status")).toBe("accepting");
    now = COMPLETED;
    docs.accept({ jsonrpc: "2.0", method: "peri/agent_event", params: { sessionId: "s1",
        event_json: JSON.stringify({ type: "agent_execution_failed", value: {} }),
    } });
    expect(turns.get("turn:u")!.toJSON()).toMatchObject({ status: "error", error: { code: "execution_failed" }, completedAt: COMPLETED });
    expect(turns.get("turn:u")!.get("startedAt")).toBe(CREATED);
    const replay = create();
    replay.accept(update("user_message_chunk", { messageId: "u2", content: { type: "text", text: "old" }, _meta: { periReplay: true } }));
    const replayed = (replay.session.getMap("root").get("turns") as Y.Map<Y.Map<unknown>>).get("turn:u2")!;
    expect(replayed.get("startedAt")).toBeNull();
    expect(replayed.get("status")).toBe("accepting");
});

test("载荷预算常量按设计评审起点冻结为默认待确认值", () => {
    expect(TOOL_PAYLOAD_INLINE_BYTES).toBe(4096);
    expect(TOOL_PAYLOAD_PREVIEW_CHARS).toBe(512);
    expect(SESSION_INLINE_PAYLOAD_BUDGET_BYTES).toBe(1_048_576);
    expect(TOOL_PAYLOAD_MAX_BYTES).toBe(8_388_608);
    expect(SESSION_EXTERNAL_PAYLOAD_BUDGET_BYTES).toBe(33_554_432);
});

test("inline 累计预算内联；超限时降级为外置引用且全文仍可读", () => {
    const docs = create({ payloadBudgets: { inlineBytes: 200 } });
    docs.acceptDeliveredUserInput("u", "read");
    docs.accept(update("tool_call", { toolCallId: "t1", status: "in_progress", rawInput: { note: "x".repeat(120) } }));
    expect(tool(docs).get("arguments")).toEqual({ note: "x".repeat(120) });
    docs.accept(update("tool_call", { toolCallId: "t2", status: "in_progress", rawInput: { note: "y".repeat(120) } }));
    const ref = tool(docs, "t2").get("argumentsRef") as { id: string; preview: string; bytes: number };
    expect(tool(docs, "t2").get("arguments")).toBeUndefined();
    expect(ref.preview.length).toBeLessThanOrEqual(512);
    expect(JSON.parse(docs.readPayload(ref.id)!)).toEqual({ note: "y".repeat(120) });
});

test("超出单项或累计外置预算的载荷被拒绝且保留前一个版本", () => {
    const docs = create({ payloadBudgets: { maxPayloadBytes: 6000, externalBytes: 6000 } });
    docs.acceptDeliveredUserInput("u", "read");
    docs.accept(update("tool_call", { toolCallId: "t1", status: "in_progress", rawInput: { keep: true } }));
    docs.accept(update("tool_call", { toolCallId: "t1", status: "completed", rawOutput: "z".repeat(5000) }));
    const ref = (tool(docs).get("resultRef") as { id: string }).id;
    expect(docs.readPayload(ref)).toBeDefined();
    const accepted = tool(docs).toJSON();
    // 单项上限：更大的结果被拒绝，前一个可读版本保持。
    docs.accept(update("tool_call", { toolCallId: "t1", status: "completed", rawOutput: "q".repeat(7000) }));
    expect(tool(docs).get("resultRef")).toEqual((accepted as Record<string, unknown>).resultRef);
    expect(tool(docs).get("error")).toEqual({ code: "payload_too_large", message: "工具载荷超出会话接纳预算" });
    expect(tool(docs).get("status")).toBe("completed");
    expect(docs.readPayload(ref)).toBeDefined();
    // 累计上限：外置总量超预算时同样拒绝，且不产生新引用。
    docs.accept(update("tool_call", { toolCallId: "t2", status: "in_progress", rawInput: { big: "w".repeat(4200) } }));
    expect(tool(docs, "t2").get("argumentsRef")).toBeUndefined();
    expect(tool(docs, "t2").get("error")).toEqual({ code: "payload_too_large", message: "工具载荷超出会话接纳预算" });
    expect(docs.exportCheckpoint().payloadValues).toHaveLength(1);
});
