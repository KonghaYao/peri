import { describe, expect, test } from "bun:test";
import * as Y from "yjs";
import { SessionDocs } from "../src/state/session-docs";
import type { JsonRpcNotification } from "../src/transport/types";

// These envelopes follow the serialization in peri-acp/src/session/event_sink.rs
// and peri-acp/src/host/requests.rs. Keep field names as they appear on the wire.
function unstable(event: string, data: Record<string, unknown>): JsonRpcNotification {
  return { jsonrpc: "2.0", method: "peri/unstable_event", params: { sessionId: "s1", event, data } };
}

function agentEvent(type: string, value: Record<string, unknown>): JsonRpcNotification {
  return {
    jsonrpc: "2.0", method: "peri/agent_event",
    params: { sessionId: "s1", event_json: JSON.stringify({ type, value }) },
  };
}

function update(sessionUpdate: string, fields: Record<string, unknown>): JsonRpcNotification {
  return {
    jsonrpc: "2.0", method: "session/update",
    params: { sessionId: "s1", update: { sessionUpdate, ...fields } },
  };
}

function session(docs: SessionDocs): Y.Map<unknown> {
  return docs.session.getMap<unknown>("root").get("session") as Y.Map<unknown>;
}

function task(docs: SessionDocs, id: string): Y.Map<unknown> | undefined {
  const tasks = docs.session.getMap<unknown>("root").get("tasks") as Y.Map<Y.Map<unknown>>;
  return tasks.get(id);
}

function assistant(docs: SessionDocs): Y.Map<unknown> {
  const root = docs.chat.getMap<unknown>("root");
  const order = root.get("entryOrder") as Y.Array<string>;
  const entries = root.get("entries") as Y.Map<Y.Map<unknown>>;
  return entries.get(order.get(order.length - 1))!;
}

describe("Peri ACP wire aggregation", () => {
  test("background snapshot restores tasks and a later snapshot repairs a revision gap", () => {
    const docs = new SessionDocs();
    docs.accept(unstable("bg-task-snapshot", {
      revision: 10,
      tasks: [
        { task_id: "shell-1", kind: "shell", summary: "Build workspace", started_at: "2026-10-04T00:00:00Z", status: "running", duration_ms: 0, output_preview: null },
        { task_id: "agent-2", kind: "agent", summary: "Review patch", started_at: "2026-10-04T00:00:01Z", status: "completed", duration_ms: 12, output_preview: "private output" },
      ],
    }));
    expect(task(docs, "shell-1")?.get("status")).toBe("running");
    expect(task(docs, "shell-1")?.get("title")).toBe("Build workspace");
    expect(task(docs, "agent-2")?.get("status")).toBe("completed");
    expect(JSON.stringify(docs.session.getMap("root").toJSON())).not.toContain("private output");

    docs.accept(unstable("bg-task-updated", { task_id: "shell-1", status: "waiting", revision: 11 }));
    expect(task(docs, "shell-1")?.get("status")).toBe("waiting");
    // The server sends a full snapshot after a skipped revision or lagged receiver.
    docs.accept(unstable("bg-task-snapshot", {
      revision: 13,
      tasks: [{ task_id: "shell-1", kind: "shell", summary: "Build workspace", started_at: "2026-10-04T00:00:00Z", status: "completed", duration_ms: 30, output_preview: "private output" }],
    }));
    docs.accept(unstable("bg-task-updated", { task_id: "shell-1", status: "running", revision: 12 }));
    expect(task(docs, "shell-1")?.get("status")).toBe("completed");
    expect(task(docs, "agent-2")).toBeUndefined();
    expect(JSON.stringify(docs.session.getMap("root").toJSON())).not.toContain("private output");
  });

  test("background start uses the wire summary, and completion keeps its identity", () => {
    const docs = new SessionDocs();
    docs.accept(unstable("bg-task-started", {
      task_id: "workflow-1", kind: "workflow", summary: "Publish docs", started_at: "2026-10-04T00:00:00Z", revision: 1,
    }));
    expect(task(docs, "workflow-1")?.get("title")).toBe("Publish docs");
    docs.accept(unstable("bg-task-completed", {
      task_id: "workflow-1", kind: "workflow", success: false, output_preview: "private output", duration_ms: 7, revision: 2,
    }));
    expect(task(docs, "workflow-1")?.get("status")).toBe("failed");
    expect(JSON.stringify(docs.session.getMap("root").toJSON())).not.toContain("private output");
  });

  test("queue snapshots and delivery follow the current generation", () => {
    const docs = new SessionDocs();
    docs.accept(agentEvent("user_input_queue_changed", { snapshot: {
      sessionId: "s1", generation: "generation-1", revision: 3, activeRequestId: "request-1",
      items: [{ inputId: "input-1", content: "Run tests", originalDraft: "Run tests", state: "claimed" }],
    } }));
    const queue = session(docs).get("inputQueue") as Record<string, unknown>;
    expect(queue.generation).toBe("generation-1");
    expect(queue.revision).toBe(3);
    expect(queue.items).toEqual([{ inputId: "input-1", state: "claimed" }]);
    docs.accept(agentEvent("user_input_delivered", {
      input_id: "input-1", generation: "generation-1", content: "Run tests",
    }));
    expect((session(docs).get("inputQueue") as Record<string, unknown>).items)
      .toEqual([{ inputId: "input-1", state: "delivered" }]);
    docs.accept(agentEvent("user_input_delivered", {
      input_id: "input-1", generation: "old-generation", content: "Run tests",
    }));
    expect((session(docs).get("inputQueue") as Record<string, unknown>).items)
      .toEqual([{ inputId: "input-1", state: "delivered" }]);
  });

  test("agent execution failure marks the active turn before done arrives", () => {
    const docs = new SessionDocs();
    docs.accept(update("user_message_chunk", { content: { type: "text", text: "Run tests" } }));
    docs.accept(agentEvent("agent_execution_failed", { message: "private provider diagnostic" }));
    expect(session(docs).get("activeTurnStatus")).toBe("error");
    expect(assistant(docs).get("status")).toBe("error");
    expect(JSON.stringify(docs.session.getMap("root").toJSON())).not.toContain("private provider diagnostic");
  });

  test("plan preserves ACP activeForm and usage preserves token metadata", () => {
    const docs = new SessionDocs();
    docs.accept(update("user_message_chunk", { content: { type: "text", text: "Implement it" } }));
    docs.accept(update("plan", { entries: [
      { content: "Run checks", priority: "medium", status: "in_progress", _meta: { activeForm: "Running checks" } },
    ] }));
    expect(session(docs).get("plan")).toEqual([
      { content: "Run checks", status: "in_progress", activeForm: "Running checks" },
    ]);
    docs.accept(update("usage_update", {
      used: 42, size: 100,
      _meta: {
        inputTokens: 42, outputTokens: 9, cacheCreationTokens: 5, cacheReadTokens: 11,
        requestId: "request-1", model: "test-model", stopReason: "end_turn",
      },
    }));
    expect(assistant(docs).get("tokenUsage")).toEqual({
      totalTokens: 42, contextWindow: 100, inputTokens: 42, outputTokens: 9,
      cacheCreationTokens: 5, cacheReadTokens: 11,
      requestId: "request-1", model: "test-model", stopReason: "end_turn",
    });
  });

  test("configuration state follows load response and live config option updates", () => {
    const docs = new SessionDocs();
    docs.seedConfig({
      modes: { currentModeId: "default", availableModes: [{ id: "default", name: "Default" }] },
      configOptions: [{ id: "model", name: "Model", type: "select", category: "model", currentValue: "fast", options: [{ value: "fast", name: "Fast" }] }],
    });
    expect((session(docs).get("configOptions") as Array<Record<string, unknown>>)[0]?.currentValue).toBe("fast");
    expect((session(docs).get("modeState") as Record<string, unknown>).currentModeId).toBe("default");
    docs.accept(update("config_option_update", { configOptions: [
      { id: "model", name: "Model", type: "select", currentValue: "quality", options: [{ value: "quality", name: "Quality" }] },
    ] }));
    expect((session(docs).get("configOptions") as Array<Record<string, unknown>>)[0]?.currentValue).toBe("quality");
  });

  test("plans retain the turn that produced them", () => {
    const docs = new SessionDocs();
    docs.accept(update("user_message_chunk", { content: { type: "text", text: "First" } }));
    docs.accept(update("plan", { entries: [{ content: "Build", status: "in_progress" }] }));
    const firstTurn = session(docs).get("activeTurnId") as string;
    docs.accept(update("user_message_chunk", { content: { type: "text", text: "Second" } }));
    docs.accept(update("plan", { entries: [{ content: "Test", status: "in_progress" }] }));
    const plans = docs.session.getMap("root").get("plansByTurn") as Y.Map<unknown>;
    expect(plans.get(firstTurn)).toEqual([{ content: "Build", status: "in_progress" }]);
    expect(plans.get(session(docs).get("activeTurnId") as string)).toEqual([{ content: "Test", status: "in_progress" }]);
  });

  test("available command input hint follows the ACP object shape", () => {
    const docs = new SessionDocs();
    docs.accept(update("available_commands_update", { availableCommands: [
      { name: "review", description: "Review code", input: { hint: "path" } },
    ] }));
    expect(session(docs).get("availableCommands")).toEqual([
      { name: "review", description: "Review code", input: { hint: "path" } },
    ]);
  });
});
