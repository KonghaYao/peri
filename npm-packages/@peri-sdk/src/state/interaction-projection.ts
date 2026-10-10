import type { AgentOptions, ElicitationDecision, ElicitationRequest, PermissionRequest } from "../agent/types";
import * as Y from "yjs";
import { DocModel } from "./doc-model";
import { object, string } from "./protocol";

/** Reverse ACP requests are the source of pending human interaction state. */
export class InteractionProjection {
    private nextId = 0;
    private readonly cancellations = new Map<number, () => void>();

    constructor(private readonly docs: DocModel) {}

    async handle(method: string, raw: unknown, expectedSessionId: string | undefined, options: AgentOptions): Promise<unknown> {
        const params = object(raw);
        const sessionId = string(params?.sessionId);
        if (!sessionId || (expectedSessionId && sessionId !== expectedSessionId)) {
            throw new Error("ACP interaction has an invalid sessionId");
        }
        if (method === "session/request_permission") return this.permission(params!, sessionId, options);
        if (method === "elicitation/create") return this.elicitation(params!, sessionId, options);
        throw new Error(`Unsupported ACP client request: ${method}`);
    }

    private async permission(params: Record<string, unknown>, sessionId: string, options: AgentOptions): Promise<unknown> {
        const tool = object(params.toolCall);
        const toolCallId = string(tool?.toolCallId);
        if (!toolCallId) throw new Error("ACP permission has no toolCallId");
        const choices = Array.isArray(params.options) ? params.options.flatMap((raw) => {
            const choice = object(raw);
            const optionId = string(choice?.optionId);
            return optionId ? [{ optionId, name: string(choice?.name) ?? optionId }] : [];
        }) : [];
        const id = ++this.nextId;
        const abort = new AbortController();
        const request: PermissionRequest = {
            interactionId: id, sessionId, toolCallId, title: string(tool?.title) ?? "Tool",
            input: tool?.rawInput, options: choices, signal: abort.signal,
        };
        this.begin(id, { kind: "permission", sessionId, toolCallId, title: request.title, options: choices });
        let decision: "allow_once" | "reject_once" = "reject_once";
        try {
            const result = await this.awaitDecision(id, options.onPermissionRequest?.(request), abort);
            if (result === "allow_once" && choices.some((choice) => choice.optionId === "allow_once")) decision = result;
        } catch { /* An unanswered or failed callback never grants a tool permission. */ }
        finally {
            this.cancellations.delete(id);
            if (!this.pending().has(String(id))) decision = "reject_once";
            this.finish(id, decision);
        }
        return { outcome: { outcome: "selected", optionId: decision } };
    }

    private async elicitation(params: Record<string, unknown>, sessionId: string, options: AgentOptions): Promise<unknown> {
        const schema = object(params.requestedSchema);
        const id = ++this.nextId;
        const abort = new AbortController();
        const request: ElicitationRequest = {
            interactionId: id, sessionId, message: string(params.message) ?? "",
            requestedSchema: schema ?? {}, signal: abort.signal,
        };
        const properties = object(schema?.properties);
        const fields = properties ? Object.entries(properties).flatMap(([key, raw]) => {
            const property = object(raw);
            return property ? [{ key, title: string(property.title) ?? key, description: string(property.description) ?? "" }] : [];
        }) : [];
        this.begin(id, { kind: "elicitation", sessionId, message: request.message, fields });
        let decision: ElicitationDecision = { action: "decline" };
        try {
            const result = await this.awaitDecision(id, options.onElicitation?.(request), abort);
            if (result?.action === "accept" && object(result.content)) decision = { action: "accept", content: result.content };
            else if (result?.action === "cancel" || result?.action === "decline") decision = { action: result.action };
        } catch { /* A failed callback declines the form without exposing its content. */ }
        finally {
            this.cancellations.delete(id);
            if (!this.pending().has(String(id))) decision = { action: "decline" };
            this.finish(id, decision.action);
        }
        return decision;
    }

    abortAll(): void {
        const pending = this.docs.info().get("pendingInteractions") as Y.Map<unknown> | undefined;
        if (!pending?.size && this.cancellations.size === 0) return;
        for (const id of this.cancellations.keys()) this.cancellations.get(id)?.();
        this.docs.transactBoth(() => {
            pending?.clear();
            this.docs.info().delete("activeTurnInteractionStatus");
        });
    }

    private awaitDecision<T>(id: number, value: T | Promise<T> | undefined, controller: AbortController): Promise<T | undefined> {
        return Promise.race([
            Promise.resolve(value),
            new Promise<undefined>((resolve) => this.cancellations.set(id, () => {
                controller.abort();
                resolve(undefined);
            })),
        ]);
    }

    private begin(id: number, pending: Record<string, unknown>): void {
        this.docs.transactBoth(() => {
            this.pending().set(String(id), { id, ...pending });
            const info = this.docs.info();
            if (info.get("activeTurnId")) info.set("activeTurnInteractionStatus", pending.kind === "permission" ? "awaiting_permission" : "awaiting_input");
            if (pending.kind === "permission") {
                const tool = this.docs.tools().get(String(pending.toolCallId));
                if (tool?.get("status") === "running") tool.set("status", "pending");
            }
        });
    }

    private finish(id: number, outcome: string): void {
        if (!this.pending().has(String(id))) return;
        this.docs.transactBoth(() => {
            const pendingMap = this.pending();
            const pending = object(pendingMap.get(String(id)));
            if (!pending) return;
            pendingMap.delete(String(id));
            const info = this.docs.info();
            info.set("lastInteraction", { id, kind: pending.kind, outcome });
            const remaining = Array.from(pendingMap.values()).map(object);
            const next = remaining.find((item) => item?.kind === "permission") ?? remaining[0];
            if (next) info.set("activeTurnInteractionStatus", next.kind === "permission" ? "awaiting_permission" : "awaiting_input");
            else info.delete("activeTurnInteractionStatus");
            if (pending.kind === "permission") {
                const tool = this.docs.tools().get(String(pending.toolCallId));
                if (tool?.get("status") === "pending") tool.set("status", "running");
            }
        });
    }

    private pending(): Y.Map<Record<string, unknown>> {
        const info = this.docs.info();
        let pending = info.get("pendingInteractions") as Y.Map<Record<string, unknown>> | undefined;
        if (!pending) {
            pending = new Y.Map<Record<string, unknown>>();
            info.set("pendingInteractions", pending);
        }
        return pending;
    }
}
