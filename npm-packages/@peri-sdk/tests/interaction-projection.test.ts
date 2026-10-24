import { describe, expect, test } from "bun:test";
import * as Y from "yjs";
import { DocModel } from "../src/state/doc-model";
import {
    evaluateElicitationForm, INTERACTION_HISTORY_LIMIT, InteractionProjection,
    validateElicitationContent, type FormField,
} from "../src/state/interaction-projection";
import { TurnMachine } from "../src/state/turn-machine";
import { readSessionView } from "../src/view/session-view";

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

describe("interaction display projection", () => {
    test("permission carries a stable id into the doc, the tool and the view", () => {
        const { model, projection, tool } = fixture();
        expect(projection.begin({
            id: "interaction:1", kind: "permission", sessionId: "s1", toolCallId: "call-1", title: "Run",
            options: [{ optionId: "allow_once", name: "Allow once" }, { optionId: "reject_once", name: "Reject" }],
        })).toBe(true);
        const record = records(model).get("interaction:1")!;
        expect(record.get("status")).toBe("pending");
        expect(record.get("turnId")).toBe("turn:1");
        expect(record.get("toolCallId")).toBe("call-1");
        expect(record.get("fields")).toBeUndefined();
        expect(record.get("questions")).toBeUndefined();
        expect(tool.get("permissionId")).toBe("interaction:1");
        expect(tool.get("status")).toBe("pending");
        expect(turnStatus(model)).toBe("awaiting_permission");
        const view = readSessionView(model.chat, model.session);
        expect(view.pendingInteractions[0]).toMatchObject({
            id: "interaction:1", kind: "permission", sessionId: "s1", toolCallId: "call-1",
        });
        expect(view.session.activeTurnInteractionStatus).toBe("awaiting_permission");

        expect(projection.settle("interaction:1", "resolved", "allow_once")).toBe(true);
        expect(record.get("status")).toBe("resolved");
        expect(record.get("outcome")).toBe("allow_once");
        expect(typeof record.get("resolvedAt")).toBe("string");
        expect(tool.get("status")).toBe("running");
        expect(turnStatus(model)).toBe("none");
        expect(readSessionView(model.chat, model.session).pendingInteractions).toEqual([]);
    });

    test("overlapping interactions settle independently and expiry never approves", () => {
        const { model, projection } = fixture();
        projection.begin({ id: "interaction:form", kind: "elicitation", sessionId: "s1", message: "Please answer",
            fields: [{ key: "answer", title: "Answer", description: "", type: "string", required: true }] });
        projection.begin({ id: "interaction:permission", kind: "permission", sessionId: "s1", toolCallId: "call-1", title: "Run" });
        expect(records(model).size).toBe(2);
        expect(records(model).get("interaction:form")?.get("questions")).toBeUndefined();
        expect(records(model).get("interaction:form")?.get("options")).toBeUndefined();
        expect(records(model).get("interaction:form")?.get("fields")).toHaveLength(1);
        expect(records(model).get("interaction:permission")?.get("fields")).toBeUndefined();
        expect(records(model).get("interaction:permission")?.get("questions")).toBeUndefined();
        expect(projection.settle("interaction:permission", "resolved", "reject_once")).toBe(true);
        expect(records(model).get("interaction:form")?.get("status")).toBe("pending");
        expect(projection.expireAll()).toBe(true);
        expect(records(model).get("interaction:form")?.get("status")).toBe("expired");
        expect(records(model).get("interaction:permission")?.get("status")).toBe("resolved");
        // 终态不能再次进入。
        expect(projection.settle("interaction:form", "resolved", "accept")).toBe(false);
        expect(records(model).get("interaction:form")?.get("outcome")).toBeUndefined();
    });

    test("ask_user stores structured questions instead of text fields or permission options", () => {
        const { model, projection } = fixture();
        projection.begin({
            id: "interaction:ask", kind: "elicitation", sessionId: "s1", title: "Questions",
            variant: "ask_user", formSupport: "unsupported", formReason: "multi_step",
            // 旧形态的 fields/options 即使混入也不写入：三者不互相冒充。
            fields: [{ key: "ask_user_question_0", title: "Language", description: "Which language?", type: "string", required: true }],
            options: [{ optionId: "a", name: "A" }],
            questions: [
                {
                    id: "ask_user_question_0", header: "Language", question: "Which language?", multiSelect: false,
                    options: [{ value: "ts", label: "TypeScript", description: "typed" }, { value: "rs", label: "Rust" }],
                },
                {
                    id: "ask_user_question_1", header: "Features", question: "Pick features", multiSelect: true,
                    options: [{ value: "a", label: "A" }, { value: "b", label: "B" }],
                },
            ],
        });
        const record = records(model).get("interaction:ask")!;
        expect(record.get("fields")).toBeUndefined();
        expect(record.get("options")).toBeUndefined();
        const view = readSessionView(model.chat, model.session);
        const interaction = view.pendingInteractions[0]!;
        expect(interaction).toMatchObject({
            id: "interaction:ask", variant: "ask_user", formUnsupported: "multi_step",
        });
        expect(interaction.fields).toEqual([]);
        expect(interaction.options).toBeUndefined();
        expect(interaction.questions).toEqual([
            {
                id: "ask_user_question_0", header: "Language", question: "Which language?", multiSelect: false,
                options: [{ value: "ts", label: "TypeScript", description: "typed" }, { value: "rs", label: "Rust" }],
            },
            {
                id: "ask_user_question_1", header: "Features", question: "Pick features", multiSelect: true,
                options: [{ value: "a", label: "A" }, { value: "b", label: "B" }],
            },
        ]);
        expect(JSON.stringify(model.session.toJSON())).not.toContain("resolver");
    });

    test("legacy ask_user documents with only fields still render a text form", () => {
        const { model } = fixture();
        const legacyFields = [{
            key: "ask_user_question_0", title: "Language",
            description: "Which language?\n- TypeScript：typed\n- Rust", type: "string", required: true,
        }];
        model.session.transact(() => {
            const legacy = new Y.Map<unknown>();
            legacy.set("id", "interaction:legacy");
            legacy.set("kind", "elicitation");
            legacy.set("sessionId", "s1");
            legacy.set("status", "pending");
            legacy.set("createdAt", "2026-10-01T00:00:00.000Z");
            legacy.set("resolvedAt", null);
            legacy.set("variant", "ask_user");
            legacy.set("fields", legacyFields);
            model.interactions().set("interaction:legacy", legacy);
        });
        const interaction = readSessionView(model.chat, model.session).pendingInteractions[0]!;
        expect(interaction).toMatchObject({ id: "interaction:legacy", variant: "ask_user" });
        expect(interaction.questions).toBeUndefined();
        expect(interaction.fields).toEqual(legacyFields);
        expect(interaction.options).toBeUndefined();
    });

    test("resolved history is bounded while pending records survive pruning", () => {
        let tick = 0;
        const model = new DocModel(() => new Date(1_700_000_000_000 + tick++).toISOString());
        const turns = new TurnMachine(model);
        const projection = new InteractionProjection(model);
        turns.start("read");
        projection.begin({ id: "interaction:pending", kind: "permission", sessionId: "s1", title: "Pending" });
        for (let index = 0; index < INTERACTION_HISTORY_LIMIT + 5; index++) {
            const id = `interaction:${index}`;
            projection.begin({ id, kind: "elicitation", sessionId: "s1", message: "m" });
            projection.settle(id, "resolved", "decline");
        }
        expect(records(model).size).toBeLessThanOrEqual(INTERACTION_HISTORY_LIMIT);
        expect(records(model).get("interaction:pending")?.get("status")).toBe("pending");
        expect(records(model).get(`interaction:${INTERACTION_HISTORY_LIMIT + 4}`)).toBeDefined();
    });
});

describe("elicitation form window", () => {
    test("unsupported shapes are rejected without an answerable form", () => {
        expect(evaluateElicitationForm({ properties: { value: { type: "array" } } }))
            .toMatchObject({ supported: false, reason: "unsupported_property_type" });
        expect(evaluateElicitationForm({ properties: { value: { type: "string", properties: {} } } }))
            .toMatchObject({ supported: false, reason: "nested_schema" });
        expect(evaluateElicitationForm({ properties: { value: { type: "string" } }, anyOf: [] }))
            .toMatchObject({ supported: false, reason: "unsupported_anyOf" });
        expect(evaluateElicitationForm({ properties: {} }))
            .toMatchObject({ supported: false, reason: "missing_properties" });
    });

    test("fields expose type/required/enum and answer validation is pure", () => {
        const form = evaluateElicitationForm({
            properties: {
                answer: { title: "Answer", description: "Choose", type: "string", enum: ["yes", "no"] },
                count: { type: "integer" },
            },
            required: ["answer"],
        });
        expect(form.supported).toBe(true);
        const fields: FormField[] = form.fields;
        expect(fields.find((field) => field.key === "answer")).toMatchObject({ required: true, enum: ["yes", "no"] });
        expect(validateElicitationContent(fields, { answer: "yes" })).toBe(true);
        expect(validateElicitationContent(fields, {})).toBe(false);
        expect(validateElicitationContent(fields, { answer: "maybe" })).toBe(false);
        expect(validateElicitationContent(fields, { answer: "yes", count: 1.5 })).toBe(false);
        expect(validateElicitationContent(fields, { answer: "yes", unknown: 1 })).toBe(false);
    });
});
