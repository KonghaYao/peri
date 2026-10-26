import * as Y from "yjs";
import { DocModel, isTerminal, publicError, type PublicErrorCode } from "./doc-model";
import { string } from "./protocol";

export type TurnExitStatus = "completed" | "cancelled" | "error";
export type TurnInteractionStatus = "none" | "awaiting_permission" | "awaiting_input";

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

    /** A turn record only advances inside its lifecycle; terminal states never move. */
    private advanceTurn(turnId: string, status: string): void {
        const turn = this.docs.turns().get(turnId);
        if (!turn || turn.get("status") === status || isTerminal(turn.get("status"))) return;
        turn.set("status", status);
    }

    /** Streaming marks the turn running on both the current index and the turn record. */
    markRunning(): void {
        const turnId = this.activeTurn();
        if (!turnId) return;
        if (this.docs.info().get("activeTurnStatus") !== "running") this.docs.info().set("activeTurnStatus", "running");
        this.advanceTurn(turnId, "running");
    }

    requestCancel(): { turnId: string; previous: "accepting" | "running" } | null {
        const turnId = this.activeTurn();
        const previous = this.docs.info().get("activeTurnStatus");
        if (!turnId || (previous !== "accepting" && previous !== "running")) return null;
        this.docs.info().set("activeTurnStatus", "cancelling");
        this.advanceTurn(turnId, "cancelling");
        return { turnId, previous };
    }

    restoreCancel(token: { turnId: string; previous: "accepting" | "running" }): void {
        if (this.activeTurn() === token.turnId && this.docs.info().get("activeTurnStatus") === "cancelling") {
            this.docs.info().set("activeTurnStatus", token.previous);
            this.advanceTurn(token.turnId, token.previous);
        }
    }

    start(userText?: string, replayId?: string, replay = false): string {
        const previous = this.activeTurn();
        if (previous) this.exit(replay ? "completed" : "cancelled");
        const turnId = replayId ? `turn:${replayId}` : `turn:${++this.nextTurn}`;
        this.startedTurns++;
        const info = this.docs.info();
        const requestId = string(info.get("pendingRequestId"));
        info.set("activeTurnId", turnId);
        info.set("activeTurnStatus", "accepting");
        info.set("activeRequestId", requestId);
        info.delete("pendingRequestId");
        const turn = new Y.Map<unknown>();
        turn.set("turnId", turnId);
        if (requestId) turn.set("requestId", requestId);
        turn.set("status", "accepting");
        // 重放的历史 turn 没有可信开始时间；不得用当前时钟冒充源时间。
        turn.set("startedAt", replay ? null : this.docs.now());
        turn.set("completedAt", null);
        turn.set("error", null);
        turn.set("interactionStatus", "none");
        this.docs.turns().set(turnId, turn);
        if (userText !== undefined) {
            const user = this.docs.ensureEntry(`${turnId}:user`, turnId, "user", replay);
            if (userText) this.docs.appendText(user, "text", userText);
        }
        this.docs.ensureEntry(`${turnId}:assistant`, turnId, "assistant", replay);
        info.set("activeAssistantEntryId", `${turnId}:assistant`);
        return turnId;
    }

    /** All paths leaving a writable turn converge assistant entries and unfinished tools here. */
    exit(status: TurnExitStatus = "completed", stopReason?: string, errorCode: PublicErrorCode = "execution_failed"): boolean {
        const turnId = this.activeTurn();
        if (!turnId || (!this.writable() && this.docs.info().get("activeTurnStatus") !== "cancelling")) return false;
        this.docs.transactBoth(() => {
            this.docs.info().set("activeTurnStatus", status);
            const turn = this.docs.turns().get(turnId);
            if (turn && !isTerminal(turn.get("status"))) {
                turn.set("status", status);
                turn.set("completedAt", turn.get("startedAt") === null ? null : this.docs.now());
                if (status === "error") turn.set("error", publicError(errorCode));
            }
            if (turn && stopReason) turn.set("stopReason", stopReason);
            for (const id of this.docs.turnEntries(turnId)) {
                const entry = this.docs.entries().get(id)!;
                if (entry.get("role") === "assistant") this.docs.finish(entry, status);
            }
            for (const id of this.docs.turnTools(turnId)) {
                const tool = this.docs.tools().get(id)!;
                this.docs.finish(tool, "cancelled");
            }
        });
        return true;
    }

    /** Canonical history replaces the active timeline and its request identity. */
    resetAfterHistory(turnId: string | null, assistantEntryId: string | null, keepOpen: boolean): void {
        const info = this.docs.info();
        const previousTurn = string(info.get("activeTurnId"));
        if (!keepOpen && previousTurn) {
            // 该 turn 的请求身份已被替换，SDK 不再能观察其结局：不伪装完成，也不声称仍在运行。
            const turn = this.docs.turns().get(previousTurn);
            if (turn && !isTerminal(turn.get("status"))) {
                turn.set("status", "error");
                turn.set("error", publicError("execution_interrupted"));
            }
        }
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
