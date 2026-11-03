import { expect, test } from "bun:test";
import { parseInteractionAnswer } from "../src/contracts/interaction";

test("interaction command parser accepts only supported decisions", () => {
    expect(parseInteractionAnswer({ kind: "permission", decision: "allow_once" })).toEqual({ kind: "permission", decision: "allow_once" });
    expect(parseInteractionAnswer({ kind: "permission", decision: "allow_always" })).toBeNull();
    expect(parseInteractionAnswer({ kind: "elicitation", decision: { action: "accept", content: { answer: "yes" } } }))
        .toEqual({ kind: "elicitation", decision: { action: "accept", content: { answer: "yes" } } });
    expect(parseInteractionAnswer({ kind: "elicitation", decision: { action: "accept", content: [] } })).toBeNull();
});
