import { expect, test } from "bun:test";
import { InteractionResponder, parseInteractionAnswer } from "../src/agent/interaction-responder";

test("interaction command parser accepts only supported decisions", () => {
    expect(parseInteractionAnswer({ kind: "permission", decision: "allow_once" })).toEqual({ kind: "permission", decision: "allow_once" });
    expect(parseInteractionAnswer({ kind: "permission", decision: "allow_always" })).toBeNull();
    expect(parseInteractionAnswer({ kind: "elicitation", decision: { action: "accept", content: { answer: "yes" } } }))
        .toEqual({ kind: "elicitation", decision: { action: "accept", content: { answer: "yes" } } });
    expect(parseInteractionAnswer({ kind: "elicitation", decision: { action: "accept", content: [] } })).toBeNull();
});

test("interaction replies require the matching session and kind", async () => {
    const responder = new InteractionResponder();
    const signal = new AbortController().signal;
    const pending = responder.onPermissionRequest({
        interactionId: 7, sessionId: "s1", toolCallId: "tool-1", title: "Run", input: {},
        options: [{ optionId: "allow_once", name: "Allow" }], signal,
    });
    expect(responder.respond("s2", 7, { kind: "permission", decision: "allow_once" })).toBe(false);
    expect(responder.respond("s1", 7, { kind: "elicitation", decision: { action: "decline" } })).toBe(false);
    expect(responder.respond("s1", 7, { kind: "permission", decision: "allow_once" })).toBe(true);
    expect(await pending).toBe("allow_once");
    expect(responder.respond("s1", 7, { kind: "permission", decision: "allow_once" })).toBe(false);
});

test("turn abort and responder close settle pending human requests", async () => {
    const responder = new InteractionResponder();
    const abort = new AbortController();
    const form = responder.onElicitation({
        interactionId: 1, sessionId: "s1", message: "Choose", requestedSchema: {}, signal: abort.signal,
    });
    abort.abort();
    expect(await form).toEqual({ action: "decline" });
    expect(responder.respond("s1", 1, { kind: "elicitation", decision: { action: "accept", content: {} } })).toBe(false);
    const permission = responder.onPermissionRequest({
        interactionId: 2, sessionId: "s1", toolCallId: "tool-2", title: "Run", input: {}, options: [], signal: new AbortController().signal,
    });
    responder.close();
    expect(await permission).toBe("reject_once");
});
