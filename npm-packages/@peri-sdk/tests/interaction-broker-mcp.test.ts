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
        const pending = begin(broker, {
            mode: "url",
            sessionId: "s1",
            message: "Authorize in browser",
            url: "https://example.com/connect?token=secret",
            elicitationId: "e-1",
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
            decision: { action: "accept", content: {} },
        });
        await expect(pending).resolves.toEqual({ action: "accept", content: {} });
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
