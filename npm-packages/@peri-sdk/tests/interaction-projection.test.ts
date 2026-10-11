import { describe, expect, test } from "bun:test";
import * as Y from "yjs";
import type { AgentOptions } from "../src/agent/types";
import { DocModel } from "../src/state/doc-model";
import { InteractionProjection } from "../src/state/interaction-projection";

const options = (callbacks: Partial<AgentOptions> = {}): AgentOptions => ({ id: "a", sandbox: {} as AgentOptions["sandbox"], ...callbacks });
const permission = (id: string) => ({
    sessionId: "s1", toolCall: { toolCallId: id, title: "Run", rawInput: { command: "echo private" } },
    options: [{ optionId: "allow_once", name: "Allow once" }, { optionId: "reject_once", name: "Reject" }],
});
const question = { sessionId: "s1", mode: "form", message: "Please answer", requestedSchema: {
    properties: { answer: { title: "Answer", description: "Choose an answer", type: "string" } },
} };

describe("reverse ACP interaction projection", () => {
    test("permission is pending in Yjs until callback resolves; only selected approval grants", async () => {
        const model = new DocModel();
        const projection = new InteractionProjection(model);
        model.info().set("activeTurnId", "turn-1");
        const tool = model.ensureTool("call-1", "turn-1", "Run");
        tool.set("status", "running");
        let resolve!: (value: "allow_once") => void;
        const answer = projection.handle("session/request_permission", permission("call-1"), "s1", options({
            onPermissionRequest: () => new Promise((done) => { resolve = done; }),
        }));
        const pending = model.info().get("pendingInteractions") as Y.Map<Record<string, unknown>>;
        expect(pending.size).toBe(1);
        expect(model.info().get("activeTurnInteractionStatus")).toBe("awaiting_permission");
        expect(tool.get("status")).toBe("pending");
        expect(JSON.stringify(model.session.toJSON())).not.toContain("echo private");
        resolve("allow_once");
        expect(await answer).toEqual({ outcome: { outcome: "selected", optionId: "allow_once" } });
        expect(pending.size).toBe(0);
        expect(tool.get("status")).toBe("running");
        expect(model.info().get("activeTurnInteractionStatus")).toBeUndefined();
    });

    test("overlapping requests settle independently; absent callbacks reject and decline", async () => {
        const model = new DocModel();
        const projection = new InteractionProjection(model);
        let finish!: (value: { action: "accept"; content: Record<string, unknown> }) => void;
        const form = projection.handle("elicitation/create", question, "s1", options({
            onElicitation: () => new Promise((done) => { finish = done; }),
        }));
        const rejected = projection.handle("session/request_permission", permission("call-2"), "s1", options());
        const pending = model.info().get("pendingInteractions") as Y.Map<Record<string, unknown>>;
        expect(pending.size).toBe(2);
        expect(await rejected).toEqual({ outcome: { outcome: "selected", optionId: "reject_once" } });
        expect(pending.size).toBe(1);
        finish({ action: "accept", content: { answer: "yes" } });
        expect(await form).toEqual({ action: "accept", content: { answer: "yes" } });
        expect(pending.size).toBe(0);
        expect(await projection.handle("elicitation/create", question, "s1", options())).toEqual({ action: "decline" });
        expect(JSON.stringify(model.session.toJSON())).not.toContain("yes");
    });

    test("failed callbacks and mismatched session cannot approve", async () => {
        const model = new DocModel();
        const projection = new InteractionProjection(model);
        expect(await projection.handle("session/request_permission", permission("call-3"), "s1", options({
            onPermissionRequest: () => { throw new Error("UI closed"); },
        }))).toEqual({ outcome: { outcome: "selected", optionId: "reject_once" } });
        expect(projection.handle("elicitation/create", question, "s2", options())).rejects.toThrow("sessionId");
    });

    test("turn cancellation settles pending requests before late callbacks can approve", async () => {
        const model = new DocModel();
        const projection = new InteractionProjection(model);
        const pending = projection.handle("session/request_permission", permission("call-4"), "s1", options({
            onPermissionRequest: () => new Promise<"allow_once">(() => {}),
        }));
        projection.abortAll();
        expect(await pending).toEqual({ outcome: { outcome: "selected", optionId: "reject_once" } });
        expect((model.info().get("pendingInteractions") as Y.Map<unknown>).size).toBe(0);
    });
});
