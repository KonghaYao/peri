import { describe, expect, test } from "bun:test";
import { InteractionBroker } from "../src/agent/interaction-broker";
import type { ChatFact, InteractionFactRecord } from "../src/contracts/chat";

type Fact = { kind: string; record?: InteractionFactRecord };

function harness() {
    const facts: Fact[] = [];
    const opened: string[] = [];
    const broker = new InteractionBroker({
        emit: (fact: ChatFact) => facts.push(fact as unknown as Fact),
        onOpenUrl: (url) => opened.push(url),
    });
    broker.setSession("s1");
    return { broker, facts, opened };
}

function begin(broker: InteractionBroker, params: Record<string, unknown>) {
    return broker.handleRequest("elicitation/create", params);
}

function interactionId(facts: Fact[]): string {
    const began = facts.find((fact) => fact.kind === "interaction-began");
    if (!began?.record) throw new Error("missing interaction-began");
    return began.record.id;
}

describe("MCP elicitation broker", () => {
    test("url fact exposes server/host and only opens after accept", async () => {
        const { broker, facts, opened } = harness();
        // 0728 的 MCP URL elicitation 不再携带 elicitationId；ACP 展示 ID 由宿主生成。
        const pending = begin(broker, {
            mode: "url",
            sessionId: "s1",
            message: "Authorize in browser",
            url: "https://example.com/connect?token=secret",
            _meta: { "io.peri/mcpElicitation": { server: "oauth", tool: "connect" } },
        });

        const fact = facts.find((entry) => entry.kind === "interaction-began")!.record!;
        expect(fact.variant).toBe("mcp_url");
        expect(fact.mcpElicitation).toEqual({
            server: "oauth",
            tool: "connect",
            mode: "url",
            url: "https://example.com/connect?token=secret",
            targetHost: "example.com",
        });
        expect(opened).toEqual([]);
        broker.answer(interactionId(facts), {
            kind: "elicitation",
            decision: { action: "accept", content: { unexpected: true } },
        });
        // URL accept 只表示同意导航：SDK 回包必须省略 content。
        await expect(pending).resolves.toEqual({ action: "accept" });
        expect(opened).toEqual(["https://example.com/connect?token=secret"]);
    });

    test("url decline never opens and session switch expires pending", async () => {
        const { broker, facts, opened } = harness();
        const pending = begin(broker, {
            mode: "url",
            sessionId: "s1",
            message: "Authorize",
            url: "https://example.com/connect",
            _meta: { "io.peri/mcpElicitation": { server: "oauth", tool: "connect" } },
        });
        broker.answer(interactionId(facts), {
            kind: "elicitation",
            decision: { action: "decline" },
        });
        await expect(pending).resolves.toEqual({ action: "decline" });
        expect(opened).toEqual([]);

        const pendingAbort = begin(broker, {
            mode: "url",
            sessionId: "s1",
            message: "Authorize",
            url: "https://example.com/connect",
            _meta: { "io.peri/mcpElicitation": { server: "oauth", tool: "connect" } },
        });
        broker.setSession("s2");
        await expect(pendingAbort).resolves.toEqual({ action: "decline" });
        expect(opened).toEqual([]);
    });

    test("form keeps note text and boolean consent", async () => {
        const { broker, facts } = harness();
        const pending = begin(broker, {
            mode: "form",
            sessionId: "s1",
            message: "Submit the change",
            requestedSchema: {
                type: "object",
                properties: {
                    note: { type: "string", title: "Note" },
                    approve: { type: "boolean", title: "Approve" },
                },
                required: ["approve"],
            },
            _meta: { "io.peri/mcpElicitation": { server: "expense", tool: "submit" } },
        });
        const fact = facts.find((entry) => entry.kind === "interaction-began")!.record!;
        expect(fact.fields?.map((field) => field.key)).toEqual(["note", "approve"]);
        broker.answer(interactionId(facts), {
            kind: "elicitation",
            decision: {
                action: "accept",
                content: { note: "looks good", approve: true },
            },
        });
        await expect(pending).resolves.toEqual({
            action: "accept",
            content: { note: "looks good", approve: true },
        });
    });

    test("ask-user single and multi-select answers keep their shape", async () => {
        const { broker, facts } = harness();
        const pending = begin(broker, {
            mode: "form",
            sessionId: "s1",
            message: "Need your input",
            requestedSchema: {
                type: "object",
                properties: {
                    ask_user_question_0: {
                        type: "string",
                        title: "Where",
                        description: "Deploy where?",
                        oneOf: [
                            { const: "edge", title: "Edge" },
                            { const: "cloud", title: "Cloud" },
                        ],
                    },
                    ask_user_question_1: {
                        type: "array",
                        title: "Features",
                        description: "Pick features",
                        items: {
                            anyOf: [
                                { const: "cache", title: "Cache" },
                                { const: "logs", title: "Logs" },
                            ],
                        },
                    },
                },
                required: ["ask_user_question_0"],
            },
        });
        const fact = facts.find((entry) => entry.kind === "interaction-began")!.record!;
        expect(fact.variant).toBe("ask_user");
        expect(fact.questions?.map((question) => question.multiSelect)).toEqual([false, true]);
        broker.answer(interactionId(facts), {
            kind: "elicitation",
            decision: {
                action: "accept",
                content: {
                    ask_user_question_0: "edge",
                    ask_user_question_1: ["cache", "logs"],
                },
            },
        });
        await expect(pending).resolves.toEqual({
            action: "accept",
            content: {
                ask_user_question_0: "edge",
                ask_user_question_1: ["cache", "logs"],
            },
        });
    });

    test("form marker keeps origin and schema fields; builtin has no marker", async () => {
        const formParams = {
            mode: "form",
            sessionId: "s1",
            message: "Approve?",
            requestedSchema: {
                type: "object",
                properties: {
                    approve: { type: "boolean", title: "Approve" },
                    name: { type: "string", title: "Name" },
                },
                required: ["approve"],
            },
            _meta: { "io.peri/mcpElicitation": { server: "expense", tool: "submit" } },
        };
        const { broker, facts } = harness();
        const pending = begin(broker, formParams);
        const fact = facts.find((entry) => entry.kind === "interaction-began")!.record!;
        expect(fact.mcpElicitation).toEqual({
            server: "expense",
            tool: "submit",
            mode: "form",
        });
        expect(fact.fields?.map((field) => field.key)).toEqual(["approve", "name"]);

        broker.answer(interactionId(facts), {
            kind: "elicitation",
            decision: { action: "accept", content: { approve: true } },
        });
        await expect(pending).resolves.toEqual({
            action: "accept",
            content: { approve: true },
        });

        const builtin = harness();
        const builtinPending = begin(builtin.broker, {
            mode: "form",
            sessionId: "s1",
            message: "Pick",
            requestedSchema: {
                type: "object",
                properties: { q: { type: "string", title: "Q" } },
            },
        });
        const builtinFact = builtin.facts.find((entry) => entry.kind === "interaction-began")!.record!;
        expect(builtinFact.mcpElicitation).toBeUndefined();
        builtin.broker.answer(interactionId(builtin.facts), {
            kind: "elicitation",
            decision: { action: "decline" },
        });
        await expect(builtinPending).resolves.toEqual({ action: "decline" });
    });
});
