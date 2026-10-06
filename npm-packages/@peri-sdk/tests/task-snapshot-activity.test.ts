import { expect, test } from "bun:test";
import * as Y from "yjs";
import { SessionDocs } from "../src/state/session-docs";
import { readSessionView } from "../src/view/session-view";

function send(docs: SessionDocs, event: string, data: Record<string, unknown>): void {
  docs.accept({ jsonrpc: "2.0", method: "peri/unstable_event", params: { sessionId: "s1", event, data } });
}

function task(docs: SessionDocs): Y.Map<unknown> | undefined {
  const tasks = docs.session.getMap("root").get("tasks") as Y.Map<Y.Map<unknown>>;
  return tasks.get("task-1");
}

for (const status of ["running", "lost", "reconciling", "delivery_pending", "waiting", "future_status"]) {
  test(`snapshot ${status} is visible but cannot establish current activity`, () => {
    const docs = new SessionDocs();
    send(docs, "bg-task-snapshot", { revision: 10, tasks: [{
      task_id: "task-1", status, kind: "shell", summary: "Build", duration_ms: 15,
    }] });
    expect(task(docs)?.get("status")).toBe(`unobserved:${status}`);
    expect(readSessionView(docs.chat, docs.session).tasks[0]).toMatchObject({
      status: `unobserved:${status}`, title: "Build", durationMs: 15,
    });
    send(docs, "bg-task-updated", { task_id: "task-1", status: "running", revision: 11 });
    expect(task(docs)?.get("status")).toBe("unobserved:running");
    docs.destroy();
  });
}

for (const status of ["completed", "failed", "cancelled"]) {
  test(`snapshot preserves ${status} and rejects later nonterminal rollback`, () => {
    const docs = new SessionDocs();
    send(docs, "bg-task-snapshot", { revision: 1, tasks: [{ task_id: "task-1", status }] });
    expect(task(docs)?.get("status")).toBe(status);
    send(docs, "bg-task-snapshot", { revision: 2, tasks: [{ task_id: "task-1", status: "running" }] });
    expect(task(docs)?.get("status")).toBe(status);
    docs.destroy();
  });
}

test("current start and updates remain live through snapshots and settle normally", () => {
  const docs = new SessionDocs();
  send(docs, "bg-task-snapshot", { revision: 1, tasks: [{ task_id: "task-1", status: "running" }] });
  send(docs, "bg-task-started", { task_id: "task-1", summary: "Current build", revision: 2 });
  expect(task(docs)?.get("status")).toBe("running");
  send(docs, "bg-task-snapshot", { revision: 3, tasks: [{ task_id: "task-1", status: "running" }] });
  expect(task(docs)?.get("status")).toBe("running");
  send(docs, "bg-task-updated", { task_id: "task-1", status: "waiting", revision: 4 });
  expect(task(docs)?.get("status")).toBe("waiting");
  send(docs, "bg-task-completed", { task_id: "task-1", success: true, revision: 5 });
  expect(task(docs)?.get("status")).toBe("completed");
  docs.destroy();
});

test("snapshot gap repair restores content without granting activity to another task", async () => {
  const docs = new SessionDocs();
  let requested = 0;
  docs.setTaskSnapshotRequester(async () => {
    requested++;
    return { revision: 4, tasks: [
      { task_id: "task-1", status: "running", summary: "Updated build", duration_ms: 22 },
      { task_id: "old-task", status: "running", summary: "Old build" },
    ] };
  });
  send(docs, "bg-task-started", { task_id: "task-1", revision: 1 });
  send(docs, "bg-task-updated", { task_id: "task-1", status: "running", revision: 4 });
  await Promise.resolve();
  expect(requested).toBe(1);
  expect(task(docs)?.get("status")).toBe("running");
  expect(task(docs)?.get("summary")).toBe("Updated build");
  expect(task(docs)?.get("durationMs")).toBe(22);
  expect(readSessionView(docs.chat, docs.session).tasks.find((item) => item.taskId === "old-task")?.status)
    .toBe("unobserved:running");
  docs.destroy();
});

test("removing a task clears its live evidence before a later snapshot reintroduces it", () => {
  const docs = new SessionDocs();
  send(docs, "bg-task-started", { task_id: "task-1", revision: 1 });
  send(docs, "bg-task-snapshot", { revision: 2, tasks: [] });
  expect(task(docs)).toBeUndefined();
  send(docs, "bg-task-snapshot", { revision: 3, tasks: [{ task_id: "task-1", status: "running" }] });
  expect(task(docs)?.get("status")).toBe("unobserved:running");
  docs.destroy();
});

test("snapshot terminal fact settles a current live task", () => {
  const docs = new SessionDocs();
  send(docs, "bg-task-started", { task_id: "task-1", revision: 1 });
  send(docs, "bg-task-snapshot", { revision: 2, tasks: [{ task_id: "task-1", status: "completed" }] });
  expect(task(docs)?.get("status")).toBe("completed");
  send(docs, "bg-task-updated", { task_id: "task-1", status: "running", revision: 3 });
  expect(task(docs)?.get("status")).toBe("completed");
  docs.destroy();
});

for (const status of ["lost", "reconciling", "delivery_pending", "future_status"]) {
  for (const source of ["bg-task-snapshot", "bg-task-updated"]) {
    test(`${source} ${status} revokes live evidence until a new start`, () => {
      const docs = new SessionDocs();
      send(docs, "bg-task-started", { task_id: "task-1", revision: 1 });
      expect(task(docs)?.get("status")).toBe("running");
      send(docs, source, source === "bg-task-snapshot"
        ? { revision: 2, tasks: [{ task_id: "task-1", status, summary: "Retained history" }] }
        : { revision: 2, task_id: "task-1", status, summary: "Retained history" });
      expect(task(docs)?.get("status")).toBe(`unobserved:${status}`);
      expect(task(docs)?.get("summary")).toBe("Retained history");
      send(docs, "bg-task-snapshot", { revision: 3, tasks: [{ task_id: "task-1", status: "running" }] });
      expect(task(docs)?.get("status")).toBe("unobserved:running");
      send(docs, "bg-task-updated", { revision: 4, task_id: "task-1", status: "running" });
      expect(task(docs)?.get("status")).toBe("unobserved:running");
      send(docs, "bg-task-started", { revision: 5, task_id: "task-1" });
      expect(task(docs)?.get("status")).toBe("running");
      docs.destroy();
    });
  }
}
