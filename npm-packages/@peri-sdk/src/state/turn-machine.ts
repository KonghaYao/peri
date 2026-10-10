import { DocModel } from "./doc-model";
import { string } from "./protocol";

export type TurnExitStatus = "completed" | "cancelled" | "error";

/** Single authority for turn identity, write guards, and terminal convergence. */
export class TurnMachine {
    private nextTurn = 0;
    private startedTurns = 0;

    constructor(private readonly docs: DocModel) {}

    activeTurn(): string | null { return string(this.docs.info().get("activeTurnId")); }
    writable(): boolean {
        const status = this.docs.info().get("activeTurnStatus");
        return status === "accepting" || status === "running";
    }

    requestCancel(): { turnId: string; previous: "accepting" | "running" } | null {
        const turnId = this.activeTurn();
        const previous = this.docs.info().get("activeTurnStatus");
        if (!turnId || (previous !== "accepting" && previous !== "running")) return null;
        this.docs.info().set("activeTurnStatus", "cancelling");
        return { turnId, previous };
    }

    restoreCancel(token: { turnId: string; previous: "accepting" | "running" }): void {
        if (this.activeTurn() === token.turnId && this.docs.info().get("activeTurnStatus") === "cancelling") {
            this.docs.info().set("activeTurnStatus", token.previous);
        }
    }

    start(userText?: string, replayId?: string, replay = false): string {
        const previous = this.activeTurn();
        if (previous) this.exit(replay ? "completed" : "cancelled");
        const turnId = replayId ? `turn:${replayId}` : `turn:${++this.nextTurn}`;
        this.startedTurns++;
        const info = this.docs.info();
        info.set("activeTurnId", turnId);
        info.set("activeTurnStatus", "accepting");
        info.set("activeRequestId", string(info.get("pendingRequestId")));
        info.delete("pendingRequestId");
        if (userText !== undefined) {
            const user = this.docs.ensureEntry(`${turnId}:user`, turnId, "user");
            if (userText) this.docs.appendText(user, "text", userText);
        }
        this.docs.ensureEntry(`${turnId}:assistant`, turnId, "assistant");
        info.set("activeAssistantEntryId", `${turnId}:assistant`);
        return turnId;
    }

    /** All paths leaving a writable turn converge assistant entries and unfinished tools here. */
    exit(status: TurnExitStatus = "completed"): boolean {
        const turnId = this.activeTurn();
        if (!turnId || (!this.writable() && this.docs.info().get("activeTurnStatus") !== "cancelling")) return false;
        this.docs.transactBoth(() => {
            this.docs.info().set("activeTurnStatus", status);
            for (const id of this.docs.turnEntries(turnId)) {
                const entry = this.docs.entries().get(id)!;
                if (entry.get("role") === "assistant" && entry.get("status") !== status) entry.set("status", status);
            }
            for (const id of this.docs.turnTools(turnId)) {
                const tool = this.docs.tools().get(id)!;
                if (["running", "pending", "awaiting_permission"].includes(String(tool.get("status")))) tool.set("status", "cancelled");
            }
        });
        return true;
    }

    /** Canonical history replaces the active timeline and its request identity. */
    resetAfterHistory(turnId: string | null, assistantEntryId: string | null, keepOpen: boolean): void {
        const info = this.docs.info();
        info.set("activeTurnId", keepOpen ? turnId : null);
        info.set("activeTurnStatus", keepOpen ? "running" : null);
        info.set("activeAssistantEntryId", keepOpen ? assistantEntryId : null);
        info.delete("pendingRequestId");
        if (!keepOpen) info.delete("activeRequestId");
    }

    acceptsDone(requestId: string | null): boolean {
        const activeRequestId = string(this.docs.info().get("activeRequestId"));
        if (requestId && activeRequestId) return requestId === activeRequestId;
        // First-turn ACP implementations may emit done without a run-started event.
        // Later turns need an identity before a wire request may terminate them.
        if (requestId && !activeRequestId) return this.startedTurns <= 1;
        return true;
    }
}
