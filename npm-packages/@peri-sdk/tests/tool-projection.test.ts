import { expect, test } from "bun:test";
import * as Y from "yjs";
import { ChatDocument } from "../src/view/chat-document";

function update(kind: string, fields: Record<string, unknown>) {
    return { jsonrpc: "2.0" as const, method: "session/update", params: {
        sessionId: "s1", update: { sessionUpdate: kind, ...fields },
    } };
}

function tools(docs: ChatDocument): Y.Map<Y.Map<unknown>> {
    return docs.chat.getMap<unknown>("root").get("toolCalls") as Y.Map<Y.Map<unknown>>;
}

test("live ACP tool cards keep public kind, arguments, and standard result content", () => {
    const docs = new ChatDocument();
    docs.accept(update("user_message_chunk", { messageId: "user-1", content: { type: "text", text: "read" } }));
    docs.accept(update("tool_call", {
        toolCallId: "call-1", title: "Read", kind: "read", status: "in_progress",
        arguments: { path: "README.md" },
    }));
    docs.accept(update("tool_call_update", {
        toolCallId: "call-1", status: "completed", result: { contentText: "first\nsecond", rawOutput: { lines: 2 } },
    }));
    const tool = tools(docs).get("call-1")!;
    expect(tool.get("kind")).toBe("read");
    expect(tool.get("arguments")).toEqual({ path: "README.md" });
    expect(tool.get("result")).toEqual({ contentText: "first\nsecond", rawOutput: { lines: 2 } });
});

test("terminal tool cards retain result and cannot regress on late updates", () => {
    const docs = new ChatDocument();
    docs.accept(update("user_message_chunk", { messageId: "user-1", content: { type: "text", text: "run" } }));
    docs.accept(update("tool_call", { toolCallId: "call-1", title: "Bash", status: "in_progress" }));
    docs.accept(update("tool_call_update", {
        toolCallId: "call-1", status: "failed", result: { contentText: "Tool execution failed" },
    }));
    docs.accept(update("tool_call_update", {
        toolCallId: "call-1", status: "in_progress", result: { contentText: "late" },
    }));
    const tool = tools(docs).get("call-1")!;
    expect(tool.get("status")).toBe("error");
    expect(tool.get("result")).toEqual({ contentText: "Tool execution failed" });
});

test("permission-pending tool resumes and ACP replay uses the same public card fields", () => {
    const docs = new ChatDocument();
    docs.accept(update("user_message_chunk", { messageId: "user-1", content: { type: "text", text: "read" } }));
    docs.accept(update("tool_call", {
        toolCallId: "call-1", title: "Read", kind: "read", status: "pending",
        arguments: { path: "README.md" }, _meta: { periReplay: true },
    }));
    docs.accept(update("tool_call_update", {
        toolCallId: "call-1", status: "in_progress", _meta: { periReplay: true },
    }));
    expect(tools(docs).get("call-1")?.get("status")).toBe("running");
    docs.accept(update("tool_call_update", {
        toolCallId: "call-1", status: "completed", _meta: { periReplay: true },
        result: { contentText: "file contents" },
    }));
    const tool = tools(docs).get("call-1")!;
    expect(tool.get("status")).toBe("completed");
    expect(tool.get("kind")).toBe("read");
    expect(tool.get("arguments")).toEqual({ path: "README.md" });
    expect(tool.get("result")).toEqual({ contentText: "file contents" });
});

// Compact 不再整表替换（REQ-C02）：规范历史重建仍由 rewind 承担（与 tool-payloads 同一取向）。
test("canonical rewind history reconstructs tool arguments and result content", () => {
    const docs = new ChatDocument();
    docs.accept({ jsonrpc: "2.0", method: "peri/agent_event", params: {
        sessionId: "s1", event_json: JSON.stringify({ type: "rewind_completed", value: {
            messages_json: JSON.stringify([
                { role: "user", id: "u1", content: "read" },
                { role: "assistant", id: "a1", content: [{ type: "tool_use", id: "call-1", name: "Read", input: { path: "README.md" } }] },
                { role: "tool", id: "t1", tool_call_id: "call-1", content: "file contents", is_error: false },
            ]),
        } }),
    } });
    const tool = tools(docs).get("call-1")!;
    expect(tool.get("arguments")).toEqual({ path: "README.md" });
    expect(tool.get("result")).toEqual({ contentText: "file contents" });
    expect(tool.get("status")).toBe("completed");
});

test("completion remains attached to the assistant message that started the tool", () => {
    const docs = new ChatDocument();
    docs.accept(update("user_message_chunk", { messageId: "user-1", content: { type: "text", text: "read" } }));
    docs.accept(update("agent_message_chunk", { messageId: "assistant-1", content: { type: "text", text: "reading" } }));
    docs.accept(update("tool_call", { toolCallId: "call-1", title: "Read", status: "in_progress" }));
    docs.accept(update("agent_message_chunk", { messageId: "assistant-2", content: { type: "text", text: "afterwards" } }));
    docs.accept(update("tool_call_update", { toolCallId: "call-1", status: "completed",
        result: { contentText: "done" } }));

    const root = docs.chat.getMap<unknown>("root");
    const entries = root.get("entries") as Y.Map<Y.Map<unknown>>;
    const first = entries.get("turn:user-1:assistant")!;
    const second = entries.get("turn:user-1:assistant:assistant-2")!;
    expect((first.get("blocks") as Y.Map<unknown>).has("tool:call-1")).toBe(true);
    expect((second.get("blocks") as Y.Map<unknown>).has("tool:call-1")).toBe(false);
});
