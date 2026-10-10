import type { ElicitationDecision, ElicitationRequest, PermissionRequest } from "./types";

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

type Pending = {
    sessionId: string;
    kind: InteractionAnswer["kind"];
    answer: (answer: InteractionAnswer) => boolean;
    cancel: () => void;
};

/** Host-side decisions for the interaction IDs already projected into Session Doc. */
export class InteractionResponder {
    private readonly pending = new Map<number, Pending>();

    readonly onPermissionRequest = (request: PermissionRequest): Promise<"allow_once" | "reject_once"> =>
        new Promise((resolve) => {
            if (request.signal.aborted) return resolve("reject_once");
            const settle = (decision: "allow_once" | "reject_once") => {
                this.pending.delete(request.interactionId);
                request.signal.removeEventListener("abort", cancel);
                resolve(decision);
            };
            const cancel = () => settle("reject_once");
            request.signal.addEventListener("abort", cancel, { once: true });
            this.pending.set(request.interactionId, {
                sessionId: request.sessionId, kind: "permission",
                answer: (answer) => {
                    if (answer.kind !== "permission") return false;
                    settle(answer.decision);
                    return true;
                },
                cancel,
            });
        });

    readonly onElicitation = (request: ElicitationRequest): Promise<ElicitationDecision> =>
        new Promise((resolve) => {
            if (request.signal.aborted) return resolve({ action: "decline" });
            const settle = (decision: ElicitationDecision) => {
                this.pending.delete(request.interactionId);
                request.signal.removeEventListener("abort", cancel);
                resolve(decision);
            };
            const cancel = () => settle({ action: "decline" });
            request.signal.addEventListener("abort", cancel, { once: true });
            this.pending.set(request.interactionId, {
                sessionId: request.sessionId, kind: "elicitation",
                answer: (answer) => {
                    if (answer.kind !== "elicitation") return false;
                    settle(answer.decision);
                    return true;
                },
                cancel,
            });
        });

    respond(sessionId: string, interactionId: number, answer: InteractionAnswer): boolean {
        const pending = this.pending.get(interactionId);
        return !!pending && pending.sessionId === sessionId && pending.kind === answer.kind && pending.answer(answer);
    }

    close(): void {
        for (const pending of [...this.pending.values()]) pending.cancel();
    }
}
