import * as Y from "yjs";
import type { ElicitationDecision, ElicitationRequest, InteractionCallbacks, PermissionRequest } from "../agent/types";
import { DocModel } from "./doc-model";
import { object, string } from "./protocol";
import type { TurnInteractionStatus } from "./turn-machine";

/** 默认待确认（设计评审起点）：每个 session 文档保留的 interaction 记录数量上限。 */
export const INTERACTION_HISTORY_LIMIT = 50;
/** 默认待确认（设计评审起点）：单步表达式的字段数量上限。 */
export const INTERACTION_FORM_MAX_FIELDS = 32;

type InteractionStatus = "pending" | "resolved" | "expired";

const SUPPORTED_PROPERTY_TYPES = new Set(["string", "number", "integer", "boolean"]);

/** 表单字段的公开描述（无 secrets、无 provider 原文）；仅承载渲染与校验需要的最小约束。 */
export type FormField = {
    key: string;
    title: string;
    description: string;
    type: "string" | "number" | "integer" | "boolean";
    required: boolean;
    enum?: Array<string | number | boolean>;
};

/** 首版表单窗口：单步基本类型；嵌套、数组、组合式 schema 与多步形态不支持。 */
export function evaluateElicitationForm(schema: Record<string, unknown> | null): { supported: boolean; fields: FormField[]; reason?: string } {
    const properties = schema ? object(schema.properties) : null;
    if (!properties) return { supported: false, fields: [], reason: "missing_properties" };
    for (const forbidden of ["$ref", "anyOf", "oneOf", "allOf", "steps"] as const) {
        if (schema?.[forbidden] !== undefined) return { supported: false, fields: [], reason: `unsupported_${forbidden.replace("$", "")}` };
    }
    if (schema?.additionalProperties !== undefined && typeof schema.additionalProperties !== "boolean") {
        return { supported: false, fields: [], reason: "unsupported_additional_properties" };
    }
    const keys = Object.keys(properties);
    if (keys.length === 0) return { supported: false, fields: [], reason: "missing_properties" };
    if (keys.length > INTERACTION_FORM_MAX_FIELDS) return { supported: false, fields: [], reason: "too_many_fields" };
    const requiredKeys = new Set<string>(Array.isArray(schema?.required)
        ? (schema!.required as unknown[]).filter((name): name is string => typeof name === "string") : []);
    const fields: FormField[] = [];
    for (const key of keys) {
        const property = object(properties[key]);
        const type = string(property?.type);
        if (!property || !type || !SUPPORTED_PROPERTY_TYPES.has(type)) return { supported: false, fields: [], reason: "unsupported_property_type" };
        for (const nested of ["properties", "items", "additionalProperties", "anyOf", "oneOf", "allOf", "$ref"] as const) {
            if (property[nested] !== undefined) return { supported: false, fields: [], reason: "nested_schema" };
        }
        if (property.enum !== undefined) {
            const values: unknown = property.enum;
            if (!Array.isArray(values) || values.length === 0) return { supported: false, fields: [], reason: "unsupported_enum" };
            const declared = type === "integer"
                ? values.every((value: unknown) => Number.isInteger(value))
                : values.every((value: unknown) => typeof value === type);
            if (!declared) return { supported: false, fields: [], reason: "unsupported_enum" };
        }
        const declaredEnum = Array.isArray(property.enum) ? (property.enum as Array<string | number | boolean>) : undefined;
        fields.push({
            key, title: string(property.title) ?? key, description: string(property.description) ?? "",
            type: type as FormField["type"], required: requiredKeys.has(key),
            ...(declaredEnum ? { enum: declaredEnum } : {}),
        });
    }
    const required = schema?.required;
    if (required !== undefined) {
        if (!Array.isArray(required) || required.some((name) => typeof name !== "string" || !(name in properties))) {
            return { supported: false, fields: [], reason: "invalid_required" };
        }
    }
    return { supported: true, fields };
}

function interactionId(): string { return `interaction:${crypto.randomUUID()}`; }

/** 答案内容必须满足表单窗口的类型/必填/enum 约束；未知字段拒绝。 */
export function validateElicitationContent(fields: FormField[], content: Record<string, unknown>): boolean {
    const declared = new Map(fields.map((field) => [field.key, field]));
    for (const key of Object.keys(content)) if (!declared.has(key)) return false;
    for (const field of fields) {
        const value = content[field.key];
        const present = value !== undefined && value !== null && value !== "";
        if (!present) { if (field.required) return false; else continue; }
        const typeOk = field.type === "string" ? typeof value === "string"
            : field.type === "boolean" ? typeof value === "boolean"
            : field.type === "integer" ? typeof value === "number" && Number.isInteger(value)
            : typeof value === "number" && Number.isFinite(value);
        if (!typeOk) return false;
        if (field.enum && !field.enum.some((option) => option === value)) return false;
    }
    return true;
}

/** 未决 interaction 派生的 turn 等待状态；permission 优先于 elicitation。 */
export function turnInteractionStatusFor(interactions: Y.Map<Y.Map<unknown>>, turnId: string): TurnInteractionStatus {
    let status: TurnInteractionStatus = "none";
    for (const record of interactions.values()) {
        if (record.get("status") !== "pending" || record.get("turnId") !== turnId) continue;
        const kind = record.get("kind");
        if (kind === "permission") return "awaiting_permission";
        status = "awaiting_input";
    }
    return status;
}

/**
 * Reverse ACP requests are the source of pending human interaction state.
 * Records carry stable string ids; answers, resolvers and abort controllers never enter Yjs.
 */
export class InteractionProjection {
    private readonly cancellations = new Map<string, () => void>();

    constructor(private readonly docs: DocModel) {}

    async handle(method: string, raw: unknown, expectedSessionId: string | undefined, options: InteractionCallbacks): Promise<unknown> {
        const params = object(raw);
        const sessionId = string(params?.sessionId);
        if (!sessionId || (expectedSessionId && sessionId !== expectedSessionId)) {
            throw new Error("ACP interaction has an invalid sessionId");
        }
        if (method === "session/request_permission") return this.permission(params!, sessionId, options);
        if (method === "elicitation/create") return this.elicitation(params!, sessionId, options);
        throw new Error(`Unsupported ACP client request: ${method}`);
    }

    private async permission(params: Record<string, unknown>, sessionId: string, options: InteractionCallbacks): Promise<unknown> {
        const tool = object(params.toolCall);
        const toolCallId = string(tool?.toolCallId);
        if (!toolCallId) throw new Error("ACP permission has no toolCallId");
        const choices = Array.isArray(params.options) ? params.options.flatMap((raw) => {
            const choice = object(raw);
            const optionId = string(choice?.optionId);
            return optionId ? [{ optionId, name: string(choice?.name) ?? optionId }] : [];
        }) : [];
        const id = interactionId();
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
            if (!this.isPending(id)) decision = "reject_once";
            this.converge(id, "resolved", decision);
        }
        return { outcome: { outcome: "selected", optionId: decision } };
    }

    private async elicitation(params: Record<string, unknown>, sessionId: string, options: InteractionCallbacks): Promise<unknown> {
        const schema = object(params.requestedSchema);
        const id = interactionId();
        const abort = new AbortController();
        const request: ElicitationRequest = {
            interactionId: id, sessionId, message: string(params.message) ?? "",
            requestedSchema: schema ?? {}, signal: abort.signal,
        };
        const form = evaluateElicitationForm(schema);
        this.begin(id, { kind: "elicitation", sessionId, message: request.message, fields: form.fields,
            ...(form.supported ? {} : { formSupport: "unsupported", formReason: form.reason }) });
        let decision: ElicitationDecision = { action: "decline" };
        try {
            // 不支持的表单没有可作答的界面：不调用回调，直接按拒绝结算（不挂起、不误发 accept）。
            const result = form.supported ? await this.awaitDecision(id, options.onElicitation?.(request), abort) : undefined;
            const content = result?.action === "accept" ? object(result.content) : null;
            if (form.supported && content && validateElicitationContent(form.fields, content)) decision = { action: "accept", content };
            else if (result?.action === "cancel" || result?.action === "decline") decision = { action: result.action };
            // 其余情况（含不支持表单的 accept）保持默认拒绝，不误发接受。
        } catch { /* A failed callback declines the form without exposing its content. */ }
        finally {
            this.cancellations.delete(id);
            if (!this.isPending(id)) decision = { action: "decline" };
            this.converge(id, "resolved", decision.action);
        }
        return decision;
    }

    abortAll(): void {
        const pending = this.pendingIds();
        if (!pending.length && this.cancellations.size === 0) return;
        for (const cancel of [...this.cancellations.values()]) cancel();
        this.cancellations.clear();
        this.docs.transactBoth(() => {
            for (const id of pending) this.converge(id, "expired");
        });
    }

    private awaitDecision<T>(id: string, value: T | Promise<T> | undefined, controller: AbortController): Promise<T | undefined> {
        return Promise.race([
            Promise.resolve(value),
            new Promise<undefined>((resolve) => this.cancellations.set(id, () => {
                controller.abort();
                resolve(undefined);
            })),
        ]);
    }

    private pendingIds(): string[] {
        const ids: string[] = [];
        for (const [id, record] of this.docs.interactions()) {
            if (record.get("status") === "pending") ids.push(id);
        }
        return ids;
    }

    private isPending(id: string): boolean {
        return this.docs.interactions().get(id)?.get("status") === "pending";
    }

    private begin(id: string, pending: Record<string, unknown>): void {
        this.docs.transactBoth(() => {
            const record = new Y.Map<unknown>();
            record.set("id", id);
            record.set("kind", pending.kind);
            record.set("sessionId", pending.sessionId);
            record.set("status", "pending");
            record.set("createdAt", this.docs.now());
            record.set("resolvedAt", null);
            for (const key of ["toolCallId", "title", "message", "formSupport", "formReason"] as const) {
                if (typeof pending[key] === "string") record.set(key, pending[key]);
            }
            if (Array.isArray(pending.options)) record.set("options", pending.options);
            if (Array.isArray(pending.fields)) record.set("fields", pending.fields);
            const turnId = string(this.docs.info().get("activeTurnId"));
            if (turnId) {
                record.set("turnId", turnId);
                const turn = this.docs.turns().get(turnId);
                if (turn) turn.set("interactionStatus", pending.kind === "permission" ? "awaiting_permission" : "awaiting_input");
            }
            this.docs.interactions().set(id, record);
            if (pending.kind === "permission") {
                const tool = this.docs.tools().get(String(pending.toolCallId));
                if (tool) {
                    tool.set("permissionId", id);
                    if (tool.get("status") === "running") tool.set("status", "pending");
                }
            }
        });
    }

    /** 终态只能从 pending 进入一次；迟到的回答不会重新激活已失效的 interaction。 */
    private converge(id: string, status: Exclude<InteractionStatus, "pending">, outcome?: string): void {
        const map = this.docs.interactions();
        const record = map.get(id);
        if (!record || record.get("status") !== "pending") return;
        record.set("status", status);
        record.set("resolvedAt", this.docs.now());
        if (outcome) record.set("outcome", outcome);
        const turnId = string(record.get("turnId"));
        if (turnId) this.refreshTurnInteractionStatus(turnId);
        const toolCallId = string(record.get("toolCallId"));
        if (record.get("kind") === "permission" && toolCallId) {
            const tool = this.docs.tools().get(toolCallId);
            if (tool?.get("status") === "pending") tool.set("status", "running");
        }
        this.prune(map);
    }

    private refreshTurnInteractionStatus(turnId: string): void {
        const turn = this.docs.turns().get(turnId);
        if (turn) turn.set("interactionStatus", turnInteractionStatusFor(this.docs.interactions(), turnId));
    }

    /** pending 记录永不回收；超出上限时按 resolvedAt 从最旧的终态记录开始删除。 */
    private prune(map: Y.Map<Y.Map<unknown>>): void {
        if (map.size <= INTERACTION_HISTORY_LIMIT) return;
        const terminal = [...map.entries()]
            .filter(([, record]) => record.get("status") !== "pending")
            .sort((a, b) => String(a[1].get("resolvedAt") ?? "").localeCompare(String(b[1].get("resolvedAt") ?? "")));
        for (const [id] of terminal) {
            if (map.size <= INTERACTION_HISTORY_LIMIT) break;
            map.delete(id);
        }
    }
}
