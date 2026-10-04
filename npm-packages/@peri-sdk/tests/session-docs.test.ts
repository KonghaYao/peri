import { describe, expect, test } from "bun:test";
import * as Y from "yjs";
import { SessionDocs } from "../src/state/session-docs";
import { MemoryKV } from "../src/kv/memory-kv";
import { ManagedAgents } from "../src/managed/managed-agents";
import { Sandbox } from "../src/sandbox/sandbox";
import type { JsonRpcNotification } from "../src/transport/types";
import type { Transport } from "../src/transport/types";

function update(
  sessionId: string,
  sessionUpdate: string,
  fields: Record<string, unknown>,
  eventId?: string,
): JsonRpcNotification {
  return {
    jsonrpc: "2.0",
    method: "session/update",
    params: {
      sessionId,
      update: { sessionUpdate, ...fields },
      ...(eventId ? { _meta: { peri: { eventId } } } : {}),
    },
  };
}

function textChunk(sessionId: string, kind: string, messageId: string, text: string, eventId?: string) {
  return update(sessionId, kind, { messageId, content: { type: "text", text } }, eventId);
}

function entries(docs: SessionDocs): Array<Y.Map<unknown>> {
  const root = docs.chat.getMap<unknown>("root");
  const order = root.get("entryOrder") as Y.Array<string>;
  const byId = root.get("entries") as Y.Map<Y.Map<unknown>>;
  return order.toArray().map((id) => byId.get(id)!);
}

function blocks(entry: Y.Map<unknown>): Array<Y.Map<unknown>> {
  const order = entry.get("blockOrder") as Y.Array<string>;
  const byId = entry.get("blocks") as Y.Map<Y.Map<unknown>>;
  return order.toArray().map((id) => byId.get(id)!);
}

function textOf(entry: Y.Map<unknown>, type: string): string {
  return blocks(entry)
    .filter((block) => block.get("type") === type)
    .map((block) => (block.get("text") as Y.Text).toString())
    .join("");
}

const USER_ONE = "00000000-0000-4000-8000-000000000001";
const ASSISTANT_ONE = "00000000-0000-4000-8000-000000000002";
const USER_TWO = "00000000-0000-4000-8000-000000000003";
const ASSISTANT_TWO = "00000000-0000-4000-8000-000000000004";

function legacyEvent(sessionId: string, type: string, value: Record<string, unknown>): JsonRpcNotification {
  return {
    jsonrpc: "2.0",
    method: "peri/agent_event",
    params: { sessionId, event_json: JSON.stringify({ type, value }) },
  };
}

describe("ACP to Yjs session documents", () => {
  test("streams user, thought, and answer chunks into ordered Yjs entries", () => {
    const docs = new SessionDocs();
    docs.accept(textChunk("s1", "user_message_chunk", "user-1", "Build it"));
    docs.accept(textChunk("s1", "agent_thought_chunk", "assistant-1", "First "));
    docs.accept(textChunk("s1", "agent_thought_chunk", "assistant-1", "inspect"));
    docs.accept(textChunk("s1", "agent_message_chunk", "assistant-1", "Done"));

    expect(docs.chat).toBeInstanceOf(Y.Doc);
    expect(docs.session).toBeInstanceOf(Y.Doc);
    const visible = entries(docs);
    expect(visible.map((entry) => entry.get("role"))).toEqual(["user", "assistant"]);
    expect(textOf(visible[0]!, "text")).toBe("Build it");
    expect(textOf(visible[1]!, "reasoning")).toBe("First inspect");
    expect(textOf(visible[1]!, "text")).toBe("Done");
    expect(blocks(visible[1]!).find((block) => block.get("type") === "text")?.get("text"))
      .toBeInstanceOf(Y.Text);
  });

  test("tool calls keep their identity and a terminal status cannot regress", () => {
    const docs = new SessionDocs();
    docs.accept(textChunk("s1", "user_message_chunk", "user-1", "Read README.md"));
    docs.accept(update("s1", "tool_call", {
      toolCallId: "call-1", title: "Read", status: "in_progress",
      rawInput: { path: "README.md" },
    }));
    docs.accept(update("s1", "tool_call_update", {
      toolCallId: "call-1", status: "completed", title: "Read",
      content: [{ type: "text", text: "contents" }],
    }));
    docs.accept(update("s1", "tool_call_update", {
      toolCallId: "call-1", status: "in_progress", title: "Read",
    }));

    const root = docs.chat.getMap<unknown>("root");
    const toolCalls = root.get("toolCalls") as Y.Map<Y.Map<unknown>>;
    expect(toolCalls.size).toBe(1);
    const call = toolCalls.get("call-1")!;
    expect(call.get("toolCallId")).toBe("call-1");
    expect(call.get("name")).toBe("Read");
    expect(call.get("status")).toBe("completed");
  });

  test("a canonical snapshot does not claim an unpaired tool call succeeded", () => {
    const docs = new SessionDocs();
    docs.accept(legacyEvent("s1", "compact_completed", {
      messages_json: JSON.stringify([
        { role: "user", id: USER_ONE, content: "Run a tool" },
        {
          role: "assistant", id: ASSISTANT_ONE,
          content: [{ type: "tool_use", id: "call-without-result", name: "Read" }],
        },
      ]),
    }));

    const tools = docs.chat.getMap<unknown>("root").get("toolCalls") as Y.Map<Y.Map<unknown>>;
    expect(tools.get("call-without-result")?.get("status")).not.toBe("completed");
  });

  test("a new user turn cancels the unfinished assistant and its tool", () => {
    const docs = new SessionDocs();
    docs.accept(textChunk("s1", "user_message_chunk", USER_ONE, "First request"));
    docs.accept(textChunk("s1", "agent_message_chunk", ASSISTANT_ONE, "Working"));
    docs.accept(update("s1", "tool_call", {
      toolCallId: "call-in-progress", title: "Read", status: "in_progress",
    }));
    docs.accept(textChunk("s1", "user_message_chunk", USER_TWO, "Second request"));

    const visible = entries(docs);
    const tools = docs.chat.getMap<unknown>("root").get("toolCalls") as Y.Map<Y.Map<unknown>>;
    expect(visible[1]?.get("status")).toBe("cancelled");
    expect(tools.get("call-in-progress")?.get("status")).toBe("cancelled");
    expect(visible[3]?.get("status")).toBe("pending");
  });

  test("a late chunk from a terminal turn does not create a phantom turn", () => {
    const docs = new SessionDocs();
    docs.accept(textChunk("s1", "user_message_chunk", USER_ONE, "Question"));
    docs.accept(textChunk("s1", "agent_message_chunk", ASSISTANT_ONE, "Answer"));
    docs.accept({
      jsonrpc: "2.0", method: "peri/agent_event_done",
      params: { sessionId: "s1", stopReason: "end_turn", requestId: "request-1" },
    });
    docs.accept(textChunk("s1", "agent_message_chunk", ASSISTANT_TWO, "Late old output"));

    expect(entries(docs)).toHaveLength(2);
    expect(textOf(entries(docs)[1]!, "text")).toBe("Answer");
    const info = docs.session.getMap<unknown>("root").get("session") as Y.Map<unknown>;
    expect(info.get("activeTurnStatus")).toBe("completed");
  });

  test("an assistant chunk after rewind clears the active turn cannot create a ghost turn", () => {
    const docs = new SessionDocs();
    docs.accept(textChunk("s1", "user_message_chunk", USER_ONE, "Before rewind"));
    docs.accept(legacyEvent("s1", "rewind_completed", { messages_json: "[]" }));
    docs.accept(textChunk("s1", "agent_message_chunk", ASSISTANT_ONE, "stale output"));
    expect(entries(docs)).toHaveLength(0);
  });

  test("replayed notifications with an explicit event id do not append text twice", () => {
    const docs = new SessionDocs();
    const first = textChunk("s1", "user_message_chunk", "user-1", "Hello", "event-1");
    const second = textChunk("s1", "agent_message_chunk", "assistant-1", "World", "event-2");
    docs.accept(first);
    docs.accept(second);
    const before = docs.chat.getMap("root").toJSON();

    docs.accept(first);
    docs.accept(second);

    expect(docs.chat.getMap("root").toJSON()).toEqual(before);
    expect(entries(docs)).toHaveLength(2);
  });

  test("ACP replay chunks carrying the same messageId replace prior live text", () => {
    const docs = new SessionDocs();
    docs.accept(textChunk("s1", "user_message_chunk", "user-1", "Hello"));
    docs.accept(textChunk("s1", "agent_message_chunk", "assistant-1", "World"));

    // session/load replays complete message content with the canonical messageId.
    // When the replay capability is negotiated, ACP marks the chunk itself.
    docs.accept(update("s1", "user_message_chunk", {
      messageId: "user-1", content: { type: "text", text: "Hello" },
      _meta: { periReplay: true },
    }));
    docs.accept(update("s1", "agent_message_chunk", {
      messageId: "assistant-1", content: { type: "text", text: "World" },
      _meta: { periReplay: true },
    }));

    const visible = entries(docs);
    expect(visible).toHaveLength(2);
    expect(textOf(visible[0]!, "text")).toBe("Hello");
    expect(textOf(visible[1]!, "text")).toBe("World");
  });

  test("multiple replay chunks of one user message preserve every text block", () => {
    const docs = new SessionDocs();
    docs.accept(textChunk("s1", "user_message_chunk", USER_ONE, "live draft"));
    docs.accept(update("s1", "user_message_chunk", {
      messageId: USER_ONE, content: { type: "text", text: "first " }, _meta: { periReplay: true },
    }));
    docs.accept(update("s1", "user_message_chunk", {
      messageId: USER_ONE, content: { type: "text", text: "second" }, _meta: { periReplay: true },
    }));
    expect(textOf(entries(docs)[0]!, "text")).toBe("first second");
  });

  test("a later replayed user turn completes the prior historical assistant", () => {
    const docs = new SessionDocs();
    const replay = (kind: string, messageId: string, text: string) => update("s1", kind, {
      messageId, content: { type: "text", text }, _meta: { periReplay: true },
    });
    docs.accept(replay("user_message_chunk", USER_ONE, "First"));
    docs.accept(replay("agent_message_chunk", ASSISTANT_ONE, "First reply"));
    docs.accept(replay("user_message_chunk", USER_TWO, "Second"));
    expect(entries(docs)[1]!.get("status")).toBe("completed");
  });

  test("different assistant messageIds in one turn remain separate entries", () => {
    const docs = new SessionDocs();
    docs.accept(textChunk("s1", "user_message_chunk", USER_ONE, "Run tool, then answer"));
    docs.accept(textChunk("s1", "agent_message_chunk", ASSISTANT_ONE, "I will inspect."));
    docs.accept(textChunk("s1", "agent_message_chunk", ASSISTANT_TWO, "Inspection complete."));

    const visible = entries(docs);
    expect(visible.map((entry) => entry.get("role"))).toEqual(["user", "assistant", "assistant"]);
    expect(textOf(visible[1]!, "text")).toBe("I will inspect.");
    expect(textOf(visible[2]!, "text")).toBe("Inspection complete.");
  });

  test("adjacent identical text blocks in one replayed assistant message both survive", () => {
    const docs = new SessionDocs();
    docs.accept(update("s1", "user_message_chunk", {
      messageId: USER_ONE, content: { type: "text", text: "Echo" },
      _meta: { periReplay: true },
    }));
    for (const part of ["ha", "ha"]) {
      docs.accept(update("s1", "agent_message_chunk", {
        messageId: ASSISTANT_ONE, content: { type: "text", text: part },
        _meta: { periReplay: true },
      }));
    }

    expect(textOf(entries(docs)[1]!, "text")).toBe("haha");
  });

  test("ACP turn completion finalizes the assistant entry without adding a message", () => {
    const docs = new SessionDocs();
    docs.accept(textChunk("s1", "user_message_chunk", "user-1", "Question"));
    docs.accept(textChunk("s1", "agent_message_chunk", "assistant-1", "Answer"));
    docs.accept({
      jsonrpc: "2.0",
      method: "peri/agent_event_done",
      params: { sessionId: "s1", stopReason: "end_turn", requestId: "request-1" },
    });

    const visible = entries(docs);
    expect(visible).toHaveLength(2);
    expect(visible[1]!.get("status")).toBe("completed");
    expect(textOf(visible[1]!, "text")).toBe("Answer");
  });

  test("max_tokens turn done keeps an incomplete terminal state", () => {
    const docs = new SessionDocs();
    docs.accept(textChunk("s1", "user_message_chunk", USER_ONE, "Long answer"));
    docs.accept(textChunk("s1", "agent_message_chunk", ASSISTANT_ONE, "Partial answer"));
    docs.accept({
      jsonrpc: "2.0", method: "peri/agent_event_done",
      params: { sessionId: "s1", stopReason: "max_tokens", requestId: "request-1" },
    });

    const info = docs.session.getMap<unknown>("root").get("session") as Y.Map<unknown>;
    expect(info.get("activeTurnStatus")).not.toBe("completed");
    expect(entries(docs)[1]!.get("status")).not.toBe("completed");
    expect(textOf(entries(docs)[1]!, "text")).toBe("Partial answer");
  });

  test("late done from an older input request cannot finish the next turn", () => {
    const docs = new SessionDocs();
    docs.accept(legacyEvent("s1", "user_input_run_started", {
      generation: "generation-1", request_id: "old-request",
    }));
    docs.accept(textChunk("s1", "user_message_chunk", USER_ONE, "First"));
    docs.accept(textChunk("s1", "agent_message_chunk", ASSISTANT_ONE, "First reply"));
    docs.accept(legacyEvent("s1", "user_input_run_started", {
      generation: "generation-1", request_id: "new-request",
    }));
    docs.accept(textChunk("s1", "user_message_chunk", USER_TWO, "Second"));
    docs.accept(textChunk("s1", "agent_message_chunk", ASSISTANT_TWO, "Second reply"));

    docs.accept({
      jsonrpc: "2.0", method: "peri/agent_event_done",
      params: { sessionId: "s1", stopReason: "end_turn", requestId: "old-request" },
    });
    const info = docs.session.getMap<unknown>("root").get("session") as Y.Map<unknown>;
    expect(info.get("activeTurnStatus")).toBe("running");
    expect(entries(docs).at(-1)?.get("status")).toBe("streaming");

    docs.accept({
      jsonrpc: "2.0", method: "peri/agent_event_done",
      params: { sessionId: "s1", stopReason: "end_turn", requestId: "new-request" },
    });
    expect(info.get("activeTurnStatus")).toBe("completed");
    expect(entries(docs).at(-1)?.get("status")).toBe("completed");
  });

  test("a late identified done cannot finish a later turn without a run-started event", () => {
    const docs = new SessionDocs();
    docs.accept(textChunk("s1", "user_message_chunk", USER_ONE, "First"));
    docs.accept(textChunk("s1", "agent_message_chunk", ASSISTANT_ONE, "First reply"));
    docs.accept(textChunk("s1", "user_message_chunk", USER_TWO, "Second"));
    docs.accept(textChunk("s1", "agent_message_chunk", ASSISTANT_TWO, "Second reply"));

    docs.accept({
      jsonrpc: "2.0", method: "peri/agent_event_done",
      params: { sessionId: "s1", stopReason: "end_turn", requestId: "old-request" },
    });

    const info = docs.session.getMap<unknown>("root").get("session") as Y.Map<unknown>;
    expect(info.get("activeTurnStatus")).toBe("running");
    expect(entries(docs).at(-1)?.get("status")).toBe("streaming");
  });

  test("cancel requested stops new deltas and done closes the same turn", () => {
    const docs = new SessionDocs();
    docs.accept(textChunk("s1", "user_message_chunk", USER_ONE, "Stop"));
    docs.accept(textChunk("s1", "agent_message_chunk", ASSISTANT_ONE, "Before"));
    expect(docs.requestCancel()).not.toBeNull();
    docs.accept(textChunk("s1", "agent_message_chunk", ASSISTANT_ONE, " after"));
    docs.accept({ jsonrpc: "2.0", method: "peri/agent_event_done", params: { sessionId: "s1", stopReason: "cancelled" } });
    expect(textOf(entries(docs)[1]!, "text")).toBe("Before");
    expect(entries(docs)[1]!.get("status")).toBe("cancelled");
  });

  test("separate Agent session document pairs do not share state", () => {
    const first = new SessionDocs();
    const second = new SessionDocs();
    first.accept(textChunk("s1", "user_message_chunk", "user-1", "Only first"));
    second.accept(textChunk("s2", "user_message_chunk", "user-2", "Only second"));

    expect(textOf(entries(first)[0]!, "text")).toBe("Only first");
    expect(textOf(entries(second)[0]!, "text")).toBe("Only second");
    expect(first.chat).not.toBe(second.chat);
    expect(first.session).not.toBe(second.session);
  });

  test("subagent lifecycle is mirrored as safe task state", () => {
    const docs = new SessionDocs();
    docs.accept(legacyEvent("s1", "subagent_started", {
      instance_id: "child-1", agent_name: "reviewer", is_background: true,
    }));
    docs.accept(legacyEvent("s1", "subagent_stopped", {
      instance_id: "child-1", agent_name: "reviewer", is_error: true,
      result: "private tool output", subagent_failure: { diagnostic: "private path" },
    }));
    docs.accept(legacyEvent("s1", "subagent_started", {
      instance_id: "child-1", agent_name: "reviewer", is_background: true,
    }));

    const root = docs.session.getMap<unknown>("root");
    const tasks = root.get("tasks") as Y.Map<Y.Map<unknown>>;
    expect(tasks.size).toBe(1);
    expect(tasks.get("child-1")?.get("status")).toBe("failed");
    expect(JSON.stringify(root.toJSON())).not.toContain("private tool output");
    expect(JSON.stringify(root.toJSON())).not.toContain("private path");
  });

  test("background task snapshot recovers a missed start and update", () => {
    const docs = new SessionDocs();
    docs.accept({
      jsonrpc: "2.0", method: "peri/unstable_event",
      params: {
        sessionId: "s1", event: "bg-task-snapshot",
        data: {
          revision: 4,
          tasks: [{
            task_id: "bg-1", kind: "workflow", summary: "Build project",
            started_at: "2026-10-04T00:00:00Z", status: "running",
            duration_ms: 0, output_preview: null,
          }],
        },
      },
    });
    docs.accept({
      jsonrpc: "2.0", method: "peri/unstable_event",
      params: {
        sessionId: "s1", event: "bg-task-updated",
        data: { task_id: "bg-1", status: "awaiting_input", revision: 5 },
      },
    });

    const root = docs.session.getMap<unknown>("root");
    const tasks = root.get("tasks") as Y.Map<Y.Map<unknown>>;
    expect(tasks.get("bg-1")?.get("kind")).toBe("background");
    expect(tasks.get("bg-1")?.get("status")).toBe("awaiting_input");
    expect((root.get("taskOrder") as Y.Array<string>).toArray()).toEqual(["bg-1"]);
  });

  test("rewind replaces streamed chat with exactly the retained canonical messages", () => {
    const docs = new SessionDocs();
    docs.accept(textChunk("s1", "user_message_chunk", USER_ONE, "Keep question"));
    docs.accept(textChunk("s1", "agent_message_chunk", ASSISTANT_ONE, "Keep answer"));
    docs.accept(textChunk("s1", "user_message_chunk", USER_TWO, "Remove question"));
    docs.accept(textChunk("s1", "agent_message_chunk", ASSISTANT_TWO, "Remove answer"));
    expect(entries(docs)).toHaveLength(4);
    const peer = new Y.Doc();
    Y.applyUpdate(peer, Y.encodeStateAsUpdate(docs.chat));

    const retained = [
      { role: "system", id: "00000000-0000-4000-8000-000000000099", content: "Private system instructions" },
      { role: "user", id: USER_ONE, content: "Keep question" },
      { role: "assistant", id: ASSISTANT_ONE, content: "Keep answer" },
    ];
    docs.accept(legacyEvent("s1", "rewind_completed", {
      summary: "Rewound two messages", messages_json: JSON.stringify(retained),
    }));

    const visible = entries(docs);
    expect(visible).toHaveLength(2);
    expect(visible.map((entry) => entry.get("role"))).toEqual(["user", "assistant"]);
    expect(textOf(visible[0]!, "text")).toBe("Keep question");
    expect(textOf(visible[1]!, "text")).toBe("Keep answer");
    expect(JSON.stringify(docs.chat.getMap("root").toJSON())).not.toContain("Remove question");
    expect(JSON.stringify(docs.chat.getMap("root").toJSON())).not.toContain("Remove answer");
    expect(JSON.stringify(docs.chat.getMap("root").toJSON())).not.toContain("Private system instructions");
    Y.applyUpdate(peer, Y.encodeStateAsUpdate(docs.chat, Y.encodeStateVector(peer)));
    expect(peer.getMap("root").toJSON()).toEqual(docs.chat.getMap("root").toJSON());
  });

  test("rewind keeps root collection handles for Yjs observers", () => {
    const docs = new SessionDocs();
    const root = docs.chat.getMap<unknown>("root");
    const order = root.get("entryOrder");
    const entriesMap = root.get("entries");
    const toolsMap = root.get("toolCalls");
    docs.accept(textChunk("s1", "user_message_chunk", USER_ONE, "Before"));
    docs.accept(legacyEvent("s1", "rewind_completed", { messages_json: "[]" }));
    expect(root.get("entryOrder")).toBe(order);
    expect(root.get("entries")).toBe(entriesMap);
    expect(root.get("toolCalls")).toBe(toolsMap);
  });

  test("compact replaces old chat content with the committed snapshot", () => {
    const docs = new SessionDocs();
    docs.accept(textChunk("s1", "user_message_chunk", USER_ONE, "Old question"));
    docs.accept(textChunk("s1", "agent_message_chunk", ASSISTANT_ONE, "Old answer"));

    const committed = [
      { role: "user", id: USER_TWO, content: "Current question" },
      { role: "assistant", id: ASSISTANT_TWO, content: [{ type: "text", text: "Current answer" }] },
    ];
    docs.accept(legacyEvent("s1", "compact_completed", {
      summary: "Compacted context", messages_json: JSON.stringify(committed),
      trigger: "auto", strategy: "full", affected_count: 2,
      estimated_tokens_saved: 100, files: [], skills: [],
    }));

    const visible = entries(docs);
    expect(visible).toHaveLength(2);
    expect(textOf(visible[0]!, "text")).toBe("Current question");
    expect(textOf(visible[1]!, "text")).toBe("Current answer");
    expect(JSON.stringify(docs.chat.getMap("root").toJSON())).not.toContain("Old question");
    expect(JSON.stringify(docs.chat.getMap("root").toJSON())).not.toContain("Old answer");
  });

  test("unknown and malformed notifications leave both documents unchanged", () => {
    const docs = new SessionDocs();
    docs.accept(textChunk("s1", "user_message_chunk", "user-1", "Retain me"));
    const chatBefore = docs.chat.getMap("root").toJSON();
    const sessionBefore = docs.session.getMap("root").toJSON();

    docs.accept(update("s1", "future_update", { arbitrary: "value" }));
    docs.accept({ jsonrpc: "2.0", method: "session/update", params: { sessionId: "s1", update: null } });
    docs.accept({ jsonrpc: "2.0", method: "peri/agent_event", params: { sessionId: "s1", event_json: "{" } });

    expect(docs.chat.getMap("root").toJSON()).toEqual(chatBefore);
    expect(docs.session.getMap("root").toJSON()).toEqual(sessionBefore);
  });
});

class ReplayTransport implements Transport {
  private readonly listeners = new Set<(notification: JsonRpcNotification) => void>();
  readonly calls: Array<{ method: string; params?: unknown }> = [];
  failLoad = false;

  async request<T>(method: string, params?: unknown): Promise<T> {
    this.calls.push({ method, params });
    if (method === "initialize") return { protocolVersion: 1 } as T;
    if (method === "session/new") return { sessionId: "s1" } as T;
    if (method === "session/input/enqueue") {
      const inputId = (params as { inputId: string }).inputId;
      this.emit(legacyEvent("s1", "user_input_delivered", { input_id: inputId, generation: "generation-1" }));
      return { results: [{ inputId, state: "delivered" }] } as T;
    }
    if (method === "session/load") {
      // ACP sends replay notifications before its session/load response.
      this.emit(update("s1", "user_message_chunk", {
        messageId: "user-1", content: { type: "text", text: "Saved prompt" },
        _meta: { periReplay: true },
      }));
      this.emit(update("s1", "agent_message_chunk", {
        messageId: "assistant-1", content: { type: "text", text: "Saved answer" },
        _meta: { periReplay: true },
      }));
      if (this.failLoad) throw new Error("session/load failed");
      return {} as T;
    }
    if (method === "session/input/snapshot") return { generation: "generation-1" } as T;
    throw new Error(`Unexpected ACP request: ${method}`);
  }

  async sendRequest<T>(method: string, params?: unknown): Promise<{ response: Promise<T> }> {
    return { response: this.request<T>(method, params) };
  }
  async notify(): Promise<void> {}
  subscribe(listener: (notification: JsonRpcNotification) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }
  emit(notification: JsonRpcNotification): void {
    for (const listener of this.listeners) listener(notification);
  }
  async *events(): AsyncIterable<JsonRpcNotification> {}
  setRequestHandler(): void {}
  async close(): Promise<void> {}
}

const replayStorage = {
  deployment: () => ({ args: [], env: {} }),
  getSessions: async () => [],
  getSession: async (id: string) => ({ id, cwd: "/tmp/workspace", title: null, messageCount: 0, createdAt: "now", updatedAt: "now" }),
};

test("Session projects load replay and later live ACP events without a stream consumer", async () => {
  const transport = new ReplayTransport();
  const sandbox = new Sandbox({
    id: "workspace-1",
    storage: replayStorage,
    transportFactory: () => transport,
  });
  const agent = new ManagedAgents({ kv: new MemoryKV() }).createAgent({
    path: "/tmp/workspace",
    id: "agent-1",
    sandbox,
  });
  const docs = agent.docs;
  await agent.session.start("s1");

  expect(agent.docs).toBe(docs);
  expect(agent.session.docs).toBe(docs);
  expect(entries(docs).map((entry) => entry.get("role"))).toEqual(["user", "assistant"]);
  expect(textOf(entries(docs)[0]!, "text")).toBe("Saved prompt");
  expect(textOf(entries(docs)[1]!, "text")).toBe("Saved answer");
  expect(entries(docs)[1]!.get("status")).toBe("completed");
  const info = docs.session.getMap<unknown>("root").get("session") as Y.Map<unknown>;
  expect(info.get("activeTurnStatus")).toBe("completed");

  transport.emit(textChunk("s1", "user_message_chunk", "user-2", "Follow up"));
  transport.emit(textChunk("s1", "agent_message_chunk", "assistant-2", "New answer"));
  transport.emit(textChunk("unrelated", "agent_message_chunk", "other", "ignore"));
  expect(textOf(entries(docs)[1]!, "text")).toBe("Saved answer");
  expect(textOf(entries(docs)[3]!, "text")).toBe("New answer");
  expect(entries(docs)).toHaveLength(4);

  const initialize = transport.calls.find((call) => call.method === "initialize")?.params as {
    clientCapabilities?: { _meta?: Record<string, unknown> };
  };
  expect(initialize.clientCapabilities?._meta?.["peri.replay"]).toBe(true);
  await agent.close();
  expect(textOf(entries(agent.docs)[3]!, "text")).toBe("New answer");
  expect(entries(agent.docs)[3]!.get("status")).toBe("cancelled");
  expect(info.get("activeTurnStatus")).toBe("cancelled");
});

test("failed session/load discards replay from the rejected attempt", async () => {
  const transport = new ReplayTransport();
  transport.failLoad = true;
  const agent = new ManagedAgents({ kv: new MemoryKV() }).createAgent({
    path: "/tmp/workspace",
    id: "failed-agent",
    sandbox: new Sandbox({
      id: "failed-workspace", storage: replayStorage, transportFactory: () => transport,
    }),
  });
  const attemptedDocs = agent.docs;

  await expect(agent.session.start("s1")).rejects.toThrow("session/load failed");
  expect(agent.docs).not.toBe(attemptedDocs);
  expect(entries(agent.docs)).toHaveLength(0);
  await agent.close();
});

test("real mailbox delivery creates the user entry before assistant output", async () => {
  const transport = new ReplayTransport();
  const agent = new ManagedAgents({ kv: new MemoryKV() }).createAgent({
    path: "/tmp/workspace",
    id: "delivered-agent",
    sandbox: new Sandbox({ id: "delivered-workspace", transportFactory: () => transport }),
  });
  await agent.session.start(null);
  await agent.session.send("Implement the feature");
  transport.emit(textChunk("s1", "agent_message_chunk", "assistant-1", "Done"));
  expect(entries(agent.docs).map((entry) => entry.get("role"))).toEqual(["user", "assistant"]);
  expect(textOf(entries(agent.docs)[0]!, "text")).toBe("Implement the feature");
  expect(textOf(entries(agent.docs)[1]!, "text")).toBe("Done");
  await agent.close();
});
