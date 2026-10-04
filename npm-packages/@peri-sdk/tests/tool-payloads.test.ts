import { expect, test } from "bun:test";
import * as Y from "yjs";
import { SessionDocs } from "../src/state/session-docs";
import { readSessionView, type ToolView } from "../src/view/session-view";

function update(docs: SessionDocs, fields: Record<string, unknown>): void {
    docs.accept({ jsonrpc: "2.0", method: "session/update", params: { sessionId: "s1", update: fields } });
}
function card(docs: SessionDocs): ToolView {
    const block = readSessionView(docs.chat, docs.session).entries[1]!.blocks[0]!;
    if (block.type !== "tool_call" || !block.tool) throw new Error("Missing tool");
    return block.tool;
}

test("large JSON stays exact on demand and is absent from hot snapshots", () => {
    const docs = new SessionDocs(); docs.acceptDeliveredUserInput("u", "read");
    const argumentsValue = { text: "🌙中文\u0000".repeat(10000), values: [null, false, 42] };
    const result = { contentText: "完整结果".repeat(30000), rawOutput: { code: 0, nested: argumentsValue } };
    update(docs, { sessionUpdate: "tool_call", toolCallId: "call", status: "completed", rawInput: argumentsValue,
        content: [{ type: "text", text: result.contentText }], rawOutput: result.rawOutput });
    const tool = card(docs);
    expect(tool.arguments).toBeUndefined(); expect(tool.result).toBeUndefined();
    expect(JSON.parse(docs.readPayload(tool.argumentsRef!.id)!)).toEqual(argumentsValue);
    expect(JSON.parse(docs.readPayload(tool.resultRef!.id)!)).toEqual(result);
    expect(tool.resultRef!.bytes).toBe(new TextEncoder().encode(JSON.stringify(result)).length);
    expect(tool.resultRef!.preview.length).toBeLessThanOrEqual(512);
    expect(Y.encodeStateAsUpdateV2(docs.chat).byteLength).toBeLessThan(8000);
    expect(docs.readPayload("another-session:1")).toBeUndefined();
    docs.destroy(); expect(docs.readPayload(tool.resultRef!.id)).toBeUndefined();
});

test("payload versions deduplicate repeats and retire when replaced, inlined, or rewound", () => {
    const docs = new SessionDocs(); docs.acceptDeliveredUserInput("u", "read");
    const set = (value: unknown) => update(docs, { sessionUpdate: "tool_call", toolCallId: "call", status: "completed", rawInput: value });
    set("x".repeat(8000)); const first = card(docs).argumentsRef!;
    set("x".repeat(8000)); expect(card(docs).argumentsRef!.id).toBe(first.id);
    set("y".repeat(9000)); const second = card(docs).argumentsRef!;
    expect(second.id).not.toBe(first.id); expect(docs.readPayload(first.id)).toBeUndefined();
    set({ small: true }); expect(card(docs).arguments).toEqual({ small: true });
    expect(card(docs).argumentsRef).toBeUndefined(); expect(docs.readPayload(second.id)).toBeUndefined();
    set("z".repeat(10000)); const last = card(docs).argumentsRef!;
    docs.accept({ jsonrpc: "2.0", method: "peri/agent_event", params: { sessionId: "s1",
        event_json: JSON.stringify({ type: "rewind_completed", value: { messages_json: "[]" } }) } });
    expect(docs.readPayload(last.id)).toBeUndefined(); expect(readSessionView(docs.chat, docs.session).entries).toEqual([]);
    docs.destroy();
});

test("caller mutation cannot alter inline or external payloads after projection", () => {
    const docs = new SessionDocs(); docs.acceptDeliveredUserInput("u", "read");
    const input = { nested: { value: "before" } };
    const result = { value: "r".repeat(8000) };
    update(docs, { sessionUpdate: "tool_call", toolCallId: "call", status: "completed", rawInput: input, rawOutput: result });
    input.nested.value = "after"; result.value = "after";
    expect(card(docs).arguments).toEqual({ nested: { value: "before" } });
    expect(JSON.parse(docs.readPayload(card(docs).resultRef!.id)!)).toEqual({ rawOutput: { value: "r".repeat(8000) } });
    docs.destroy();
});

test("canonical history uses the same bounded payload structure and preserves full content", () => {
    const docs = new SessionDocs(); const text = "result".repeat(3000);
    docs.accept({ jsonrpc: "2.0", method: "peri/agent_event", params: { sessionId: "s1", event_json: JSON.stringify({
        type: "compact_completed", value: { messages_json: JSON.stringify([
            { id: "u", role: "user", content: "read" },
            { id: "a", role: "assistant", content: [{ type: "tool_use", id: "call", name: "Read", input: { key: "x".repeat(5000) } }] },
            { id: "t", role: "tool", tool_call_id: "call", content: text },
        ]) },
    }) } });
    expect(JSON.parse(docs.readPayload(card(docs).resultRef!.id)!)).toEqual({ contentText: text });
    expect(card(docs).status).toBe("completed"); docs.destroy();
});

test("canonical tool call arrays preserve order while deduplicating content-block identities", () => {
    const docs = new SessionDocs();
    docs.accept({ jsonrpc: "2.0", method: "peri/agent_event", params: { event_json: JSON.stringify({
        type: "compact_completed", value: { messages_json: JSON.stringify([
            { id: "u", role: "user", content: "read" },
            { id: "a", role: "assistant", content: [{ type: "tool_use", id: "first", name: "Read" }],
                tool_calls: [{ id: "first", name: "duplicate" }, { id: "second", name: "Bash" }, { id: "second", name: "duplicate" }] },
        ]) },
    }) } });
    const view = readSessionView(docs.chat, docs.session);
    expect(view.entries[1]!.blocks.map((block) => block.type === "tool_call" ? block.tool?.name : null)).toEqual(["Read", "Bash"]);
    docs.destroy();
});
