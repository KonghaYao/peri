import { expect, test } from "bun:test";
import * as Y from "yjs";
import { SessionDocs } from "../src/state/session-docs";

const diagnostic = "/Users/fixture/project ~/fixture https://fixture.invalid/api?token=synthetic wss://fixture.invalid password=synthetic Bearer fixture-token";

function tasks(docs: SessionDocs): Y.Map<Y.Map<unknown>> {
    return docs.session.getMap("root").get("tasks") as Y.Map<Y.Map<unknown>>;
}

function background(docs: SessionDocs, name: string, data: Record<string, unknown>): void {
    docs.accept({ jsonrpc: "2.0", method: "peri/unstable_event", params: {
        sessionId: "fixture", event: name, data,
    } });
}

test("background task diagnostics preserve content in live events and snapshots", () => {
    const docs = new SessionDocs();
    background(docs, "bg-task-started", { task_id: "task", kind: "shell", summary: diagnostic, revision: 1 });
    expect(tasks(docs).get("task")?.get("summary")).toBe(diagnostic);
    background(docs, "bg-task-snapshot", { revision: 2, tasks: [{ task_id: "task", kind: "shell", status: "running", summary: diagnostic }] });
    expect(tasks(docs).get("task")?.get("summary")).toBe(diagnostic);
});

test("task summaries retain their existing length and whitespace constraints", () => {
    const docs = new SessionDocs();
    const summary = diagnostic + "x".repeat(600);
    background(docs, "bg-task-started", { task_id: "task", kind: "shell", summary: `  ${summary}  `, revision: 1 });
    expect(tasks(docs).get("task")?.get("summary")).toBe(summary.slice(0, 500));
    expect(tasks(docs).get("task")?.get("title")).toBe(summary.slice(0, 120));
});

test("subagent names preserve diagnostic-shaped content within the title budget", () => {
    const docs = new SessionDocs();
    docs.accept({ jsonrpc: "2.0", method: "peri/agent_event", params: {
        sessionId: "fixture", event_json: JSON.stringify({ type: "subagent_started", value: {
            instance_id: "sub", agent_name: diagnostic, is_background: true,
        } }),
    } });
    expect(tasks(docs).get("sub")?.get("title")).toBe(diagnostic.slice(0, 120));
});
