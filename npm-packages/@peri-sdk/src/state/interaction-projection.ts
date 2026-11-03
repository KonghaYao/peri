import * as Y from "yjs";
import { DocModel } from "./doc-model";
import { object, string } from "./protocol";
import type { TurnInteractionStatus } from "./turn-machine";

/** 默认待确认（设计评审起点）：每个 session 文档保留的 interaction 记录数量上限。 */
export const INTERACTION_HISTORY_LIMIT = 50;
/** 默认待确认（设计评审起点）：单步表达式的字段数量上限。 */
const INTERACTION_FORM_MAX_FIELDS = 32;

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

/** AskUserQuestion 的结构化展示问题；与运行侧 `InteractionQuestion` 同构。 */
type InteractionQuestionDisplay = {
    id: string;
    header: string;
    question: string;
    multiSelect: boolean;
    options: Array<{ value: string; label: string; description?: string }>;
};

/** 展示层只需要运行侧给出的公开字段；resolver 与回答正文不进入文档。 */
type InteractionDisplayRecord = {
    id: string;
    kind: "permission" | "elicitation";
    sessionId: string;
    turnId?: string;
    toolCallId?: string;
    title?: string;
    message?: string;
    /** 仅 permission。 */
    options?: Array<{ optionId: string; name: string }>;
    /** 仅普通 elicitation。 */
    fields?: FormField[];
    variant?: "ask_user";
    /** 仅 `variant === "ask_user"`；不回退写入 `fields`。 */
    questions?: InteractionQuestionDisplay[];
    formSupport?: "unsupported";
    formReason?: string;
};

/** 复制成文档自有的普通数据，避免跨层共享可变引用。 */
function displayQuestions(questions: InteractionQuestionDisplay[]): InteractionQuestionDisplay[] {
    return questions.map((question) => ({
        id: question.id,
        header: question.header,
        question: question.question,
        multiSelect: question.multiSelect,
        options: question.options.map((option) => ({
            value: option.value,
            label: option.label,
            ...(option.description === undefined ? {} : { description: option.description }),
        })),
    }));
}

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
 * 交互的纯展示投影。
 *
 * 运行侧 InteractionBroker 持有 resolver/AbortController，并通过事实把公开字段送进来；
 * 这里只写展示记录与 turn/tool 的等待状态，不调用回调、不决定权限。
 */
export class InteractionProjection {
    constructor(private readonly docs: DocModel) {}

    /** 幂等接纳一条待决交互；相同 id 的迟到重复不覆盖已写入的记录。 */
    begin(input: InteractionDisplayRecord): boolean {
        const kind = input.kind;
        if (!input.id || (kind !== "permission" && kind !== "elicitation")) return false;
        if (this.docs.interactions().has(input.id)) return false;
        this.docs.transactBoth(() => {
            const record = new Y.Map<unknown>();
            record.set("id", input.id);
            record.set("kind", kind);
            record.set("sessionId", input.sessionId);
            record.set("status", "pending");
            record.set("createdAt", this.docs.now());
            record.set("resolvedAt", null);
            const turnId = input.turnId ?? string(this.docs.info().get("activeTurnId"));
            if (turnId) record.set("turnId", turnId);
            for (const key of ["toolCallId", "title", "message", "variant", "formSupport", "formReason"] as const) {
                const value = input[key];
                if (typeof value === "string" && value) record.set(key, value);
            }
            if (kind === "permission") {
                if (input.options?.length) {
                    record.set("options", input.options.map((option) => ({ optionId: option.optionId, name: option.name })));
                }
            } else if (input.variant === "ask_user") {
                if (input.questions?.length) record.set("questions", displayQuestions(input.questions));
            } else if (input.fields?.length) {
                record.set("fields", input.fields.map((field) => ({ ...field, ...(field.enum ? { enum: [...field.enum] } : {}) })));
            }
            this.docs.interactions().set(input.id, record);
            if (turnId) {
                const turn = this.docs.turns().get(turnId);
                if (turn) turn.set("interactionStatus", kind === "permission" ? "awaiting_permission" : "awaiting_input");
            }
            if (kind === "permission" && input.toolCallId) {
                const tool = this.docs.tools().get(input.toolCallId);
                if (tool) {
                    tool.set("permissionId", input.id);
                    if (tool.get("status") === "running") tool.set("status", "pending");
                }
            }
        });
        return true;
    }

    /** 终态只能从 pending 进入一次；迟到的回答不会重新激活已失效的 interaction。 */
    settle(id: string, status: Exclude<InteractionStatus, "pending">, outcome?: string): boolean {
        const record = this.docs.interactions().get(id);
        if (!record || record.get("status") !== "pending") return false;
        this.docs.transactBoth(() => {
            record.set("status", status);
            record.set("resolvedAt", this.docs.now());
            if (outcome) record.set("outcome", outcome);
            const turnId = string(record.get("turnId"));
            if (turnId) {
                const turn = this.docs.turns().get(turnId);
                if (turn) turn.set("interactionStatus", turnInteractionStatusFor(this.docs.interactions(), turnId));
            }
            const toolCallId = string(record.get("toolCallId"));
            if (record.get("kind") === "permission" && toolCallId) {
                const tool = this.docs.tools().get(toolCallId);
                if (tool?.get("status") === "pending") tool.set("status", "running");
            }
            this.prune();
        });
        return true;
    }

    /** 运行侧关闭/abort 时发出；不回答、不批准，只把展示状态收敛为 expired。 */
    expireAll(): boolean {
        const pending = [...this.docs.interactions().entries()]
            .filter(([, record]) => record.get("status") === "pending")
            .map(([id]) => id);
        if (!pending.length) return false;
        this.docs.transactBoth(() => {
            for (const id of pending) {
                const record = this.docs.interactions().get(id)!;
                record.set("status", "expired");
                record.set("resolvedAt", this.docs.now());
                const turnId = string(record.get("turnId"));
                if (turnId) {
                    const turn = this.docs.turns().get(turnId);
                    if (turn) turn.set("interactionStatus", turnInteractionStatusFor(this.docs.interactions(), turnId));
                }
            }
            this.prune();
        });
        return true;
    }

    /** pending 记录永不回收；超出上限时按 resolvedAt 从最旧的终态记录开始删除。 */
    private prune(): void {
        const map = this.docs.interactions();
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
