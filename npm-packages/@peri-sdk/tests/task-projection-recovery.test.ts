import { closeCommand, controlResponse } from "../test/control-fixture";
import { expect, test } from "bun:test";
import * as Y from "yjs";
import { SessionDocs } from "../src/state/session-docs";
import { MemoryKV } from "../src/kv/memory-kv";
import { ManagedAgents } from "../src/managed/managed-agents";
import { Sandbox } from "../src/sandbox/sandbox";
import type { JsonRpcNotification, Transport } from "../src/transport/types";

function tasks(docs: SessionDocs): Y.Map<Y.Map<unknown>> {
  return docs.session.getMap<unknown>("root").get("tasks") as Y.Map<Y.Map<unknown>>;
}

function event(name: string, data: Record<string, unknown>) {
  return { jsonrpc: "2.0" as const, method: "peri/unstable_event", params: { sessionId: "s1", event: name, data } };
}

test("a missed client revision requests the Peri snapshot and converges", async () => {
  const docs = new SessionDocs();
  let requested = 0;
  docs.setTaskSnapshotRequester(async () => {
    requested++;
    return { revision: 4, tasks: [
      { task_id: "task-1", kind: "shell", summary: "Build", started_at: "2026-10-04T00:00:00Z", status: "completed", duration_ms: 12, output_preview: "private output" },
    ] };
  });
  docs.accept(event("bg-task-snapshot", { revision: 1, tasks: [
    { task_id: "task-1", kind: "shell", summary: "Build", started_at: "2026-10-04T00:00:00Z", status: "running", duration_ms: null, output_preview: null },
  ] }));
  const original = tasks(docs).get("task-1");
  docs.accept(event("bg-task-completed", { task_id: "task-1", kind: "shell", success: true, duration_ms: 12, revision: 4 }));
  await Promise.resolve();
  expect(requested).toBe(1);
  expect(tasks(docs).get("task-1")).toBe(original);
  expect(original?.get("status")).toBe("completed");
  expect(original?.get("taskSubtype")).toBe("shell");
  expect(original?.get("summary")).toBe("Build");
  expect(original?.get("startedAt")).toBe("2026-10-04T00:00:00Z");
  expect(original?.get("durationMs")).toBe(12);
  expect(JSON.stringify(docs.session.getMap("root").toJSON())).not.toContain("private output");
});

test("subagent lifecycle preserves background identity and rejects terminal rollback", () => {
  const docs = new SessionDocs();
  const send = (type: string, value: Record<string, unknown>) => docs.accept({
    jsonrpc: "2.0", method: "peri/agent_event",
    params: { sessionId: "s1", event_json: JSON.stringify({ type, value }) },
  });
  send("subagent_started", { instance_id: "sub-1", agent_name: "Research", is_background: true });
  send("subagent_stopped", { instance_id: "sub-1", agent_name: "Research", is_error: false, result: "private result" });
  send("subagent_started", { instance_id: "sub-1", agent_name: "Research", is_background: true });
  const sub = tasks(docs).get("sub-1");
  expect(sub?.get("status")).toBe("completed");
  expect(sub?.get("isBackground")).toBe(true);
  expect(JSON.stringify(docs.session.getMap("root").toJSON())).not.toContain("private result");
});

test("Session asks Peri for a task snapshot after a missed notification", async () => {
  class TaskTransport implements Transport {
    private listeners = new Set<(event: JsonRpcNotification) => void>();
    snapshotCalls = 0;
    async request<T>(method: string, _params?: unknown): Promise<T> {
    if (method.startsWith("session/control")) return controlResponse(method, _params) as T;
      if (method === "initialize") return { protocolVersion: 1 } as T;
      if (method === "session/load") return {} as T;
      if (method === "session/input/snapshot") return { generation: "g1" } as T;
      if (method === "session/bg-tasks") {
        this.snapshotCalls++;
        return { revision: 5, tasks: [{
          task_id: "shell-1", kind: "shell", summary: "Build", started_at: "2026-10-04T00:00:00Z", status: "completed", duration_ms: 9,
        }] } as T;
      }
      throw new Error(`Unexpected request ${method}`);
    }
    async sendRequest<T>(method: string, params?: unknown): Promise<{ response: Promise<T> }> {
      return { response: this.request<T>(method, params) };
    }
    async notify(): Promise<void> {}
    subscribe(listener: (event: JsonRpcNotification) => void): () => void {
      this.listeners.add(listener);
      return () => this.listeners.delete(listener);
    }
    emit(event: JsonRpcNotification): void { for (const listener of this.listeners) listener(event); }
    async *events(): AsyncIterable<JsonRpcNotification> {}
    setRequestHandler(): void {}
    async close(): Promise<void> {}
  }
  const transport = new TaskTransport();
  const sandbox = new Sandbox({
    id: "ws", transportFactory: () => transport,
    storage: {
      deployment: () => ({ args: [], env: {} }),
      getSessions: async () => [],
      getSession: async (id) => ({ id, cwd: "/tmp/ws", title: null, messageCount: 0, createdAt: "now", updatedAt: "now" }),
    },
  });
  const agent = new ManagedAgents({ kv: new MemoryKV() }).createAgent({ path: "/tmp/ws", id: "agent", sandbox });
  await agent.session.start("s1");
  transport.emit(event("bg-task-snapshot", { revision: 1, tasks: [{ task_id: "shell-1", status: "running", kind: "shell" }] }));
  transport.emit(event("bg-task-completed", { task_id: "shell-1", success: true, revision: 5 }));
  await Promise.resolve();
  expect(transport.snapshotCalls).toBe(1);
  expect(tasks(agent.docs).get("shell-1")?.get("status")).toBe("completed");
  await agent.close(closeCommand);
});
