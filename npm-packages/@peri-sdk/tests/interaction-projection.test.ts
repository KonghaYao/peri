import { describe, expect, test } from "bun:test";
import * as Y from "yjs";
import type { AgentOptions } from "../src/agent/types";
import { DocModel } from "../src/state/doc-model";
import { INTERACTION_HISTORY_LIMIT, InteractionProjection, validateElicitationContent, type FormField } from "../src/state/interaction-projection";
import { TurnMachine } from "../src/state/turn-machine";
import { readSessionView } from "../src/view/session-view";

const options = (callbacks: Partial<AgentOptions> = {}): AgentOptions => ({ id: "a", sandbox: {} as AgentOptions["sandbox"], ...callbacks });
const permission = (id: string) => ({
    sessionId: "s1", toolCall: { toolCallId: id, title: "Run", rawInput: { command: "echo private" } },
    options: [{ optionId: "allow_once", name: "Allow once" }, { optionId: "reject_once", name: "Reject" }],
});
const question = { sessionId: "s1", mode: "form", message: "Please answer", requestedSchema: {
    properties: { answer: { title: "Answer", description: "Choose an answer", type: "string" } },
} };

function fixture() {
    const model = new DocModel();
    const turns = new TurnMachine(model);
    const projection = new InteractionProjection(model);
    turns.start("read");
    const tool = model.ensureTool("call-1", "turn:1", "Run");
    tool.set("status", "running");
    return { model, turns, projection, tool };
}

function records(model: DocModel): Y.Map<Y.Map<unknown>> { return model.interactions(); }
function turnStatus(model: DocModel, turnId = "turn:1"): unknown { return model.turns().get(turnId)?.get("interactionStatus"); }

describe("reverse ACP interaction projection", () => {
    test("permission carries a stable string id into the doc, the tool and the view", async () => {
        const { model, projection, tool } = fixture();
        let resolve!: (value: "allow_once") => void;
        const answer = projection.handle("session/request_permission", permission("call-1"), "s1", options({
            onPermissionRequest: () => new Promise((done) => { resolve = done; }),
        }));
        const pending = records(model);
        expect(pending.size).toBe(1);
        const id = String([...pending.keys()][0]);
        expect(id.startsWith("interaction:")).toBe(true);
        const record = pending.get(id)!;
        expect(record.get("status")).toBe("pending");
        expect(record.get("turnId")).toBe("turn:1");
        expect(record.get("toolCallId")).toBe("call-1");
        expect(tool.get("permissionId")).toBe(id);
        expect(tool.get("status")).toBe("pending");
        expect(turnStatus(model)).toBe("awaiting_permission");
        const view = readSessionView(model.chat, model.session);
        expect(view.pendingInteractions[0]).toMatchObject({ id, kind: "permission", sessionId: "s1", toolCallId: "call-1" });
        expect(view.session.activeTurnInteractionStatus).toBe("awaiting_permission");
        expect(JSON.stringify(model.session.toJSON())).not.toContain("echo private");
        resolve("allow_once");
        expect(await answer).toEqual({ outcome: { outcome: "selected", optionId: "allow_once" } });
        expect(record.get("status")).toBe("resolved");
        expect(record.get("outcome")).toBe("allow_once");
        expect(typeof record.get("resolvedAt")).toBe("string");
        expect(tool.get("status")).toBe("running");
        expect(turnStatus(model)).toBe("none");
        expect(readSessionView(model.chat, model.session).pendingInteractions).toEqual([]);
    });

    test("overlapping requests settle independently; absent callbacks reject and decline", async () => {
        const { model, projection } = fixture();
        let finish!: (value: { action: "accept"; content: Record<string, unknown> }) => void;
        const form = projection.handle("elicitation/create", question, "s1", options({
            onElicitation: () => new Promise((done) => { finish = done; }),
        }));
        const rejected = projection.handle("session/request_permission", permission("call-1"), "s1", options());
        const pending = records(model);
        expect(pending.size).toBe(2);
        const pendingCount = (): number => [...pending.values()].filter((record) => record.get("status") === "pending").length;
        expect(pendingCount()).toBe(2);
        expect(await rejected).toEqual({ outcome: { outcome: "selected", optionId: "reject_once" } });
        expect(pendingCount()).toBe(1);
        expect(turnStatus(model)).toBe("awaiting_input");
        finish({ action: "accept", content: { answer: "yes" } });
        expect(await form).toEqual({ action: "accept", content: { answer: "yes" } });
        expect(pendingCount()).toBe(0);
        expect(await projection.handle("elicitation/create", question, "s1", options())).toEqual({ action: "decline" });
        expect(JSON.stringify(model.session.toJSON())).not.toContain("yes");
    });

    test("failed callbacks and mismatched session cannot approve", async () => {
        const { model, projection } = fixture();
        expect(await projection.handle("session/request_permission", permission("call-1"), "s1", options({
            onPermissionRequest: () => { throw new Error("UI closed"); },
        }))).toEqual({ outcome: { outcome: "selected", optionId: "reject_once" } });
        expect(projection.handle("elicitation/create", question, "s2", options())).rejects.toThrow("sessionId");
    });

    test("turn cancellation expires pending records before late callbacks can approve", async () => {
        const { model, projection } = fixture();
        const pending = projection.handle("session/request_permission", permission("call-1"), "s1", options({
            onPermissionRequest: () => new Promise<"allow_once">(() => {}),
        }));
        projection.abortAll();
        const recordsMap = records(model);
        const record = recordsMap.get(String([...recordsMap.keys()][0]))!;
        expect(record.get("status")).toBe("expired");
        expect(turnStatus(model)).toBe("none");
        expect(await pending).toEqual({ outcome: { outcome: "selected", optionId: "reject_once" } });
        // 迟到的结算不重新激活已失效记录，也不授予权限。
        expect(record.get("status")).toBe("expired");
        expect(readSessionView(model.chat, model.session).pendingInteractions).toEqual([]);
    });

    test("不支持的整表单默认拒绝：嵌套/数组/组合 schema 不误发 accept", async () => {
        const { model, projection } = fixture();
        const accept = options({ onElicitation: () => ({ action: "accept", content: { anything: true } }) });
        const nested = await projection.handle("elicitation/create", { ...question,
            requestedSchema: { properties: { profile: { type: "object", title: "Profile" } } } }, "s1", accept);
        expect(nested).toEqual({ action: "decline" });
        const arrays = await projection.handle("elicitation/create", { ...question,
            requestedSchema: { properties: { tags: { type: "array", items: { type: "string" } } } } }, "s1", accept);
        expect(arrays).toEqual({ action: "decline" });
        const composed = await projection.handle("elicitation/create", { ...question,
            requestedSchema: { properties: { value: { type: "string", anyOf: [{ type: "string" }] } } } }, "s1", accept);
        expect(composed).toEqual({ action: "decline" });
        const recordsList = [...records(model).values()];
        const unsupported = recordsList.find((record) => record.get("formSupport") === "unsupported");
        expect(unsupported).toBeDefined();
        expect(typeof unsupported!.get("formReason")).toBe("string");
        expect(unsupported!.get("status")).toBe("resolved");
        expect(unsupported!.get("outcome")).toBe("decline");
        const supported = await projection.handle("elicitation/create", { ...question,
            requestedSchema: { properties: { level: { type: "string", enum: ["a", "b"], title: "Level" } }, required: ["level"] } }, "s1",
            options({ onElicitation: () => ({ action: "accept", content: { level: "a" } }) }));
        expect(supported).toEqual({ action: "accept", content: { level: "a" } });
    });

    test("表单窗口暴露类型/必填/enum，答案内容不满足约束时不误发 accept", async () => {
        const { model, projection } = fixture();
        const schema = {
            properties: {
                level: { type: "string", enum: ["a", "b"], title: "Level" },
                count: { type: "integer", title: "Count" },
                note: { type: "string", title: "Note" },
                flag: { type: "boolean", title: "Flag" },
            },
            required: ["level", "count"],
        };
        const ask = (content: Record<string, unknown>, action: "accept" | "decline" = "accept") =>
            projection.handle("elicitation/create", { ...question, requestedSchema: schema }, "s1",
                options({ onElicitation: () => action === "accept" ? { action: "accept", content } : { action: "decline" } }));

        // 合法答案（含可选项）→ 接受。
        expect(await ask({ level: "a", count: 2, flag: true })).toEqual({ action: "accept", content: { level: "a", count: 2, flag: true } });
        // 缺必填、类型不符、enum 越界、未知字段 → 一律按拒绝结算，不误发 accept。
        expect(await ask({ count: 2 })).toEqual({ action: "decline" });
        expect(await ask({ level: "a", count: 1.5 })).toEqual({ action: "decline" });
        expect(await ask({ level: "c", count: 2 })).toEqual({ action: "decline" });
        expect(await ask({ level: "a", count: 2, extra: "x" })).toEqual({ action: "decline" });
        // 用户显式拒绝仍是拒绝；视图暴露字段描述供前端渲染。
        expect(await ask({}, "decline")).toEqual({ action: "decline" });
        const view = readSessionView(model.chat, model.session);
        expect(view.interactions.at(-1)?.fields).toEqual([
            { key: "level", title: "Level", description: "", type: "string", required: true, enum: ["a", "b"] },
            { key: "count", title: "Count", description: "", type: "integer", required: true },
            { key: "note", title: "Note", description: "", type: "string", required: false },
            { key: "flag", title: "Flag", description: "", type: "boolean", required: false },
        ]);
    });

    test("答案校验是纯函数：类型/必填/enum/未知字段逐项判定", () => {
        const fields: FormField[] = [
            { key: "name", title: "Name", description: "", type: "string", required: true },
            { key: "n", title: "N", description: "", type: "number", required: false, enum: [1, 2] },
            { key: "b", title: "B", description: "", type: "boolean", required: true },
        ];
        expect(validateElicitationContent(fields, { name: "x", b: false })).toBe(true);
        expect(validateElicitationContent(fields, { name: "x", b: false, n: 2 })).toBe(true);
        expect(validateElicitationContent(fields, { name: "x", b: false, n: 3 })).toBe(false);
        expect(validateElicitationContent(fields, { name: "x" })).toBe(false);
        expect(validateElicitationContent(fields, { name: "", b: true })).toBe(false);
        expect(validateElicitationContent(fields, { name: "x", b: true, other: 1 })).toBe(false);
    });

    test("resolved history is bounded while pending records survive pruning", async () => {
        const { model, projection } = fixture();
        const held = projection.handle("session/request_permission", permission("call-1"), "s1", options({
            onPermissionRequest: () => new Promise<"allow_once">(() => {}),
        }));
        const ids: string[] = [];
        for (let index = 0; index < INTERACTION_HISTORY_LIMIT + 5; index++) {
            const before = new Set(records(model).keys());
            await projection.handle("session/request_permission", permission("call-1"), "s1", options({
                onPermissionRequest: () => "reject_once",
            }));
            const created = [...records(model).keys()].find((id) => !before.has(id));
            if (created) ids.push(created);
        }
        const map = records(model);
        expect(map.size).toBe(INTERACTION_HISTORY_LIMIT);
        expect(map.get(ids[0]!)).toBeUndefined();
        expect([...map.values()].filter((record) => record.get("status") === "pending")).toHaveLength(1);
        projection.abortAll();
        await held;
    });
});
