import type { JsonRpcNotification } from "../transport/types";
import type { AgentOptions } from "../agent/types";
import { ChatProjection } from "./chat-projection";
import { DocModel } from "./doc-model";
import { InputQueueProjection } from "./input-queue-projection";
import { InteractionProjection } from "./interaction-projection";
import { decodeAgentEvent, object, string } from "./protocol";
import { SessionProjection } from "./session-projection";
import { TaskProjection } from "./task-projection";
import { TurnMachine, type TurnExitStatus } from "./turn-machine";

/** One Agent's live ACP projection. Session.start filters sessionId before accept. */
export class SessionDocs {
    private readonly model = new DocModel();
    private readonly turns = new TurnMachine(this.model);
    private readonly chatProjection = new ChatProjection(this.model, this.turns);
    private readonly sessionProjection = new SessionProjection(this.model, this.turns);
    private readonly taskProjection = new TaskProjection(this.model);
    private readonly inputQueue = new InputQueueProjection(this.model);
    private readonly interaction = new InteractionProjection(this.model);
    private readonly seenEvents = new Set<string>();
    private taskSnapshotInFlight = false;
    private destroyed = false;

    readonly chat = this.model.chat;
    readonly session = this.model.session;

    /** Exact immutable JSON for a current ToolPayloadRef; stale or unknown versions are absent. */
    readPayload(id: string): string | undefined { return this.model.payloads.read(id); }

    /** Ingest a received batch without publishing a Yjs transaction per ACP notification. */
    acceptBatch(notifications: Iterable<JsonRpcNotification>): void {
        if (this.destroyed) return;
        this.model.transactBoth(() => { for (const notification of notifications) this.accept(notification); });
    }

    destroy(): void {
        if (this.destroyed) return;
        this.destroyed = true;
        this.interaction.abortAll();
        this.taskProjection.setSnapshotRequester(null);
        this.seenEvents.clear();
        this.model.destroy();
    }
    completeTurn(status: TurnExitStatus = "completed"): boolean {
        if (this.destroyed) return false;
        const exited = this.turns.exit(status);
        this.interaction.abortAll();
        return exited;
    }
    requestCancel(): { turnId: string; previous: "accepting" | "running" } | null {
        if (this.destroyed) return null;
        const token = this.turns.requestCancel();
        if (token) this.interaction.abortAll();
        return token;
    }
    restoreCancel(token: { turnId: string; previous: "accepting" | "running" }): void {
        if (!this.destroyed) this.turns.restoreCancel(token);
    }
    acceptDeliveredUserInput(inputId: string, text: string): boolean {
        if (this.destroyed) return false;
        this.interaction.abortAll();
        return this.chatProjection.acceptDeliveredUserInput(inputId, text);
    }
    handleRequest(method: string, params: unknown, expectedSessionId: string | undefined, options: AgentOptions): Promise<unknown> {
        if (this.destroyed) return Promise.reject(new Error("SessionDocs is destroyed"));
        return this.interaction.handle(method, params, expectedSessionId, options);
    }

    /** Repair missing background-task revisions through Peri's session/bg-tasks snapshot. */
    setTaskSnapshotRequester(requester: (() => Promise<unknown>) | null): void {
        if (this.destroyed) return;
        this.taskProjection.setSnapshotRequester(requester ? () => {
            if (this.taskSnapshotInFlight || this.destroyed) return;
            this.taskSnapshotInFlight = true;
            let pending: Promise<unknown>;
            try { pending = requester(); }
            catch { this.taskSnapshotInFlight = false; return; }
            void pending.then((snapshot) => {
                if (!this.destroyed) this.taskProjection.acceptBackground("bg-task-snapshot", object(snapshot));
            }).catch(() => {
                // A later task delta retries; the previous revision remains untrusted.
            }).finally(() => { this.taskSnapshotInFlight = false; });
        } : null);
    }

    /** Seed queue state from session/input/snapshot, even when no queue notification has arrived. */
    seedInputQueue(snapshot: unknown): void { if (!this.destroyed) this.inputQueue.seed(snapshot); }
    seedConfig(response: unknown): void { if (!this.destroyed) this.sessionProjection.seedConfig(response); }

    accept(notification: JsonRpcNotification): void {
        if (this.destroyed) return;
        if (!notification || typeof notification !== "object") return;
        const params = object(notification.params);
        if (!params) return;
        const eventId = string(object(object(params._meta)?.peri)?.eventId);
        if (eventId && this.seenEvents.has(eventId)) return;

        let accepted = false;
        if (notification.method === "session/update") {
            const update = object(params.update);
            if (!update) return;
            const previousTurn = this.turns.activeTurn();
            accepted = this.chatProjection.accept(update, params) || this.sessionProjection.acceptUpdate(update, params);
            if (string(update.sessionUpdate) === "user_message_chunk" && previousTurn !== this.turns.activeTurn()) {
                this.interaction.abortAll();
            }
        } else if (notification.method === "peri/agent_event") {
            const event = decodeAgentEvent(params.event_json);
            if (!event) return;
            accepted = this.acceptAgentEvent(event.type, event.value);
        } else if (notification.method === "peri/agent_event_done") {
            const requestId = string(params.requestId);
            if (!this.turns.acceptsDone(requestId)) return;
            const reason = string(params.stopReason);
            const status: TurnExitStatus = reason === "cancelled" ? "cancelled"
                : reason === "max_tokens" || reason === "max_turn_requests" || reason === "error" ? "error" : "completed";
            accepted = this.completeTurn(status);
        } else if (notification.method === "peri/unstable_event") {
            accepted = this.taskProjection.acceptBackground(string(params.event), object(params.data));
        }
        if (accepted && eventId) this.seenEvents.add(eventId);
    }

    private acceptAgentEvent(type: string, value: Record<string, unknown> | null): boolean {
        if (type === "user_input_run_started" && value) {
            const requestId = string(value.request_id);
            if (!requestId) return false;
            this.model.info().set("pendingRequestId", requestId);
            return true;
        }
        if ((type === "compact_completed" || type === "rewind_completed") && value) {
            if (!this.chatProjection.replaceHistory(value.messages_json, false)) return false;
            this.interaction.abortAll();
            this.sessionProjection.acceptAgentEvent(type, value);
            return true;
        }
        return this.inputQueue.acceptAgentEvent(type, value)
            || this.sessionProjection.acceptAgentEvent(type, value)
            || this.taskProjection.acceptSubagent(type, value);
    }
}
