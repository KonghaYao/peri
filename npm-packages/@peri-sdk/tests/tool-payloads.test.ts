import { expect, test } from "bun:test";
import * as Y from "yjs";
import { ChatDocument } from "../src/view/chat-document";
import { readSessionView, type ToolView } from "../src/view/session-view";

function update(docs: ChatDocument, fields: Record<string, unknown>): void {
    docs.accept({ jsonrpc: "2.0", method: "session/update", params: { sessionId: "s1", update: fields } });
}
function card(docs: ChatDocument): ToolView {
    const block = readSessionView(docs.chat, docs.session).entries[1]!.blocks[0]!;
    if (block.type !== "tool_call" || !block.tool) throw new Error("Missing tool");
    return block.tool;
}

test("normalized inline and external payload fields stay mutually exclusive", () => {
    const docs = new ChatDocument(); docs.acceptDeliveredUserInput("u", "read");
    const argumentsValue = { text: "🌙中文\u0000".repeat(100), values: [null, false, 42] };
    update(docs, { sessionUpdate: "tool_call", toolCallId: "call", status: "completed", arguments: argumentsValue });
    expect(card(docs).arguments).toEqual(argumentsValue);
    expect(card(docs).argumentsRef).toBeUndefined();

    const ref = { id: "payload:1", bytes: 9000, preview: "preview" };
    update(docs, { sessionUpdate: "tool_call", toolCallId: "call", status: "completed", argumentsRef: ref });
    expect(card(docs).arguments).toBeUndefined();
    expect(card(docs).argumentsRef).toEqual(ref);
    expect(Y.encodeStateAsUpdateV2(docs.chat).byteLength).toBeLessThan(3000);

    update(docs, { sessionUpdate: "tool_call", toolCallId: "call", status: "completed", arguments: { small: true } });
    expect(card(docs).arguments).toEqual({ small: true });
    expect(card(docs).argumentsRef).toBeUndefined();
    docs.destroy();
});

test("caller mutation cannot alter a normalized inline payload after projection", () => {
    const docs = new ChatDocument(); docs.acceptDeliveredUserInput("u", "read");
    const input = { nested: { value: "before" } };
    update(docs, { sessionUpdate: "tool_call", toolCallId: "call", status: "completed", arguments: input });
    input.nested.value = "after";
    expect(card(docs).arguments).toEqual({ nested: { value: "before" } });
    docs.destroy();
});

test("a payload rejection is reported as a public tool error without dropping the previous value", () => {
    const docs = new ChatDocument(); docs.acceptDeliveredUserInput("u", "read");
    update(docs, { sessionUpdate: "tool_call", toolCallId: "call", status: "in_progress", arguments: { keep: true } });
    update(docs, {
        sessionUpdate: "tool_call_update", toolCallId: "call", status: "in_progress",
        _meta: { periPayloadTooLarge: ["result"] },
    });
    const tool = readSessionView(docs.chat, docs.session).toolCalls[0]!;
    expect(tool.arguments).toEqual({ keep: true });
    expect(tool.error).toEqual({ code: "payload_too_large", message: "工具载荷超出会话接纳预算" });
    docs.destroy();
});

// Compact 不再整表替换（REQ-C02）；窗口刷新是 rewind 的语义。
test("canonical rewind keeps normalized tool arguments and result content", () => {
    const docs = new ChatDocument(); const text = "result".repeat(3000);
    docs.accept({ jsonrpc: "2.0", method: "peri/agent_event", params: { sessionId: "s1", event_json: JSON.stringify({
        type: "rewind_completed", value: { messages_json: JSON.stringify([
            { id: "u", role: "user", content: "read" },
            { id: "a", role: "assistant", content: [{ type: "tool_use", id: "call", name: "Read", input: { key: "x" } }] },
            { id: "t", role: "tool", tool_call_id: "call", content: text },
        ]) },
    }) } });
    expect(card(docs).arguments).toEqual({ key: "x" });
    expect(card(docs).result).toEqual({ contentText: text });
    expect(card(docs).status).toBe("completed");
    docs.destroy();
});

test("canonical rewind accepts a runtime-normalized external argument reference", () => {
    const docs = new ChatDocument();
    docs.accept({ jsonrpc: "2.0", method: "peri/agent_event", params: { sessionId: "s1", event_json: JSON.stringify({
        type: "rewind_completed", value: { messages_json: JSON.stringify([
            { id: "u", role: "user", content: "read" },
            { id: "a", role: "assistant", content: [{
                type: "tool_use", id: "call", name: "Read",
                argumentsRef: { id: "payload:9", bytes: 2048, preview: "preview" },
            }] },
        ]) },
    }) } });
    expect(card(docs).arguments).toBeUndefined();
    expect(card(docs).argumentsRef).toEqual({ id: "payload:9", bytes: 2048, preview: "preview" });
    docs.destroy();
});

test("canonical rewind tool call arrays preserve order while deduplicating content-block identities", () => {
    const docs = new ChatDocument();
    docs.accept({ jsonrpc: "2.0", method: "peri/agent_event", params: { event_json: JSON.stringify({
        type: "rewind_completed", value: { messages_json: JSON.stringify([
            { id: "u", role: "user", content: "read" },
            { id: "a", role: "assistant", content: [{ type: "tool_use", id: "first", name: "Read" }],
                tool_calls: [{ id: "first", name: "duplicate" }, { id: "second", name: "Bash" }, { id: "second", name: "duplicate" }] },
        ]) },
    }) } });
    const view = readSessionView(docs.chat, docs.session);
    expect(view.entries[1]!.blocks.map((block) => block.type === "tool_call" ? block.tool?.name : null)).toEqual(["Read", "Bash"]);
    docs.destroy();
});
