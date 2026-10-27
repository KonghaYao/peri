import { expect, test } from "bun:test";
import * as Y from "yjs";
import { PeriInstance } from "../src/agent/peri-instance";
import { ChatFactHub } from "../src/chat/fact-hub";
import type { ChatFact } from "../src/contracts/chat";
import { ChatDocument } from "../src/view/chat-document";
import { readSessionView } from "../src/view/session-view";
import type { JsonRpcNotification, Transport } from "../src/transport/types";

function tasks(document: ChatDocument): Y.Map<Y.Map<unknown>> {
  return document.session.getMap<unknown>("root").get("tasks") as Y.Map<Y.Map<unknown>>;
}

function event(name: string, data: Record<string, unknown>) {
  return { jsonrpc: "2.0" as const, method: "peri/unstable_event", params: { sessionId: "s1", event: name, data } };
}

test("a missed client revision converges through the runtime task snapshot", () => {
  const document = new ChatDocument();
  document.accept(event("bg-task-snapshot", { revision: 1, tasks: [
    { task_id: "task-1", kind: "shell", summary: "Build", started_at: "2026-10-04T00:00:00Z", status: "running", duration_ms: null, output_preview: null },
  ] }));
  const original = tasks(document).get("task-1");

  // 运行侧 TaskGapDetector 补读后投递结构化 task-snapshot 事实（PeriInstance 测试覆盖触发）。
  document.applyFact({
    kind: "task-snapshot",
    snapshot: { revision: 4, tasks: [
      { task_id: "task-1", kind: "shell", summary: "Build", started_at: "2026-10-04T00:00:00Z", status: "completed", duration_ms: 12, output_preview: "private output" },
    ] },
  });

  expect(tasks(document).get("task-1")).toBe(original);
  expect(original?.get("status")).toBe("completed");
  expect(original?.get("taskSubtype")).toBe("shell");
  expect(original?.get("summary")).toBe("Build");
  expect(original?.get("startedAt")).toBe("2026-10-04T00:00:00Z");
  expect(original?.get("durationMs")).toBe(12);
  expect(JSON.stringify(document.session.getMap("root").toJSON())).not.toContain("private output");
  document.destroy();
});

test("subagent lifecycle preserves background identity and rejects terminal rollback", () => {
  const document = new ChatDocument();
  const send = (type: string, value: Record<string, unknown>) => document.accept({
    jsonrpc: "2.0", method: "peri/agent_event",
    params: { sessionId: "s1", event_json: JSON.stringify({ type, value }) },
  });
  send("subagent_started", { instance_id: "sub-1", agent_name: "Research", is_background: true });
  send("subagent_stopped", { instance_id: "sub-1", agent_name: "Research", is_error: false, result: "private result" });
  send("subagent_started", { instance_id: "sub-1", agent_name: "Research", is_background: true });
  const sub = tasks(document).get("sub-1");
  expect(sub?.get("status")).toBe("completed");
  expect(sub?.get("isBackground")).toBe(true);
  expect(JSON.stringify(document.session.getMap("root").toJSON())).not.toContain("private result");
  document.destroy();
});

test("PeriInstance asks Peri for a task snapshot after a missed notification", async () => {
  class TaskTransport implements Transport {
    private listeners = new Set<(event: JsonRpcNotification) => void>();
    snapshotCalls = 0;
    async request<T>(method: string, _params?: unknown): Promise<T> {
      if (method === "initialize") return { protocolVersion: 1 } as T;
      if (method === "session/load") return {} as T;
      if (method === "session/input/snapshot") return { generation: "g1" } as T;
      if (method === "session/close") return {} as T;
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
  const instance = new PeriInstance({
    acquireHost: async () => ({ transport, close: async () => ({ confirmed: true }) }),
    resolveSessionPath: async () => "/tmp/ws",
    close: { drainTimeoutMs: 1000, bindingSettleTimeoutMs: 1000, activationCloseTimeoutMs: 1000 },
  });
  const document = new ChatDocument();
  instance.attach((fact) => document.applyFact(fact));
  await instance.bindExisting("s1");
  transport.emit(event("bg-task-snapshot", { revision: 1, tasks: [{ task_id: "shell-1", status: "running", kind: "shell" }] }));
  transport.emit(event("bg-task-completed", { task_id: "shell-1", success: true, revision: 5 }));
  await Promise.resolve();
  expect(transport.snapshotCalls).toBe(1);
  expect(tasks(document).get("shell-1")?.get("status")).toBe("completed");
  expect(readSessionView(document.chat, document.session).tasks.find((task) => task.taskId === "shell-1")?.status)
    .toBe("completed");
  await instance.close();
  document.destroy();
});

test("a failing view observer cannot block task facts or other observers", () => {
  const hub = new ChatFactHub("chat-1");
  const seen: ChatFact[] = [];
  hub.bind({ chatId: "chat-1", sessionId: "s1", bindingRevision: 1 }, []);
  hub.subscribe({
    onBinding() {},
    onFact() { throw new Error("projection failed /tmp/fixture?token=synthetic"); },
    onClosed() {},
  });
  hub.subscribe({
    onBinding() {},
    onFact(envelope) { seen.push(envelope.fact); },
    onClosed() {},
  });
  hub.push({ kind: "task-snapshot", snapshot: { revision: 5, tasks: [] } });
  expect(seen).toEqual([{ kind: "task-snapshot", snapshot: { revision: 5, tasks: [] } }]);
});
