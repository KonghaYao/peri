import type { ElicitationDecision } from "./types";

export type InteractionAnswer =
    | { kind: "permission"; decision: "allow_once" | "reject_once" }
    | { kind: "elicitation"; decision: ElicitationDecision };

export function parseInteractionAnswer(raw: unknown): InteractionAnswer | null {
    if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
    const value = raw as Record<string, unknown>;
    if (value.kind === "permission" && (value.decision === "allow_once" || value.decision === "reject_once"))
        return { kind: "permission", decision: value.decision };
    if (value.kind !== "elicitation" || !value.decision || typeof value.decision !== "object" || Array.isArray(value.decision)) return null;
    const decision = value.decision as Record<string, unknown>;
    if (decision.action === "decline" || decision.action === "cancel") return { kind: "elicitation", decision: { action: decision.action } };
    if (decision.action === "accept" && decision.content && typeof decision.content === "object" && !Array.isArray(decision.content))
        return { kind: "elicitation", decision: { action: "accept", content: decision.content as Record<string, unknown> } };
    return null;
}
