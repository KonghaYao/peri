import type { ChatFact, InteractionFactRecord } from "../chat/facts";
import type { ElicitationDecision } from "./types";
import { parseInteractionAnswer, type InteractionAnswer } from "./interaction-responder";
import {
    evaluateElicitationForm,
    normalizeAskUserQuestionDecision,
    parseAskUserQuestionElicitation,
    toInteractionQuestions,
    validateElicitationContent,
    type AskUserQuestion,
    type AskUserQuestionDecision,
} from "./ask-user-question";

/**
 * 运行侧交互 broker：权限/elicitation/AskUserQuestion 的 pending、答案结算与安全默认。
 *
 * resolver、AbortController 与问题结构只留在这里；事实只暴露
 * [`InteractionFactRecord`] 的公开字段。关闭/轮次结束时一律按安全默认结算
 * （权限拒绝、表单 decline），绝不误批准。
 */

export type InteractionAnswerErrorCode = "unknown_interaction" | "kind_mismatch" | "invalid_answer" | "closed";

export class InteractionAnswerError extends Error {
    constructor(readonly code: InteractionAnswerErrorCode, message: string) {
        super(message);
        this.name = "InteractionAnswerError";
    }
}

export interface InteractionBrokerOptions {
    emit: (fact: ChatFact) => void;
    /** 当前 turn id；只用于事实展示，不参与身份判定。 */
    currentTurnId?: () => string | null;
    now?: () => string;
}

type PendingQuestion = {
    kind: "permission" | "elicitation";
    sessionId: string;
    settle: (answer: InteractionAnswer) => boolean;
    abort: () => void;
};

type FactFields = InteractionFactRecord["fields"];

function record(value: unknown): Record<string, unknown> | null {
    return value !== null && typeof value === "object" && !Array.isArray(value)
        ? value as Record<string, unknown>
        : null;
}

function nonEmpty(value: unknown): string | null {
    return typeof value === "string" && value.length > 0 ? value : null;
}

function formFields(fields: Array<{
    key: string;
    title: string;
    description: string;
    type: "string" | "number" | "integer" | "boolean";
    required: boolean;
}>): FactFields {
    return fields.map((field) => ({
        key: field.key,
        title: field.title,
        description: field.description,
        type: field.type,
        required: field.required,
    }));
}

export class InteractionBroker {
    private readonly pending = new Map<string, PendingQuestion>();
    private sessionId: string | null = null;
    private closed = false;

    constructor(private readonly options: InteractionBrokerOptions) {}

    get pendingCount(): number { return this.pending.size; }

    /** 绑定会话身份；切换会话时未决交互一律按安全默认失效。 */
    setSession(sessionId: string | null): void {
        if (this.sessionId === sessionId) return;
        if (this.pending.size > 0) this.abortAll();
        this.sessionId = sessionId;
    }

    /** ACP 反向请求入口：只接受权限与 elicitation，未知方法显式失败。 */
    async handleRequest(method: string, params: unknown): Promise<unknown> {
        if (this.closed) throw new Error("Interaction broker is closed");
        if (method === "session/request_permission") return this.permission(params);
        if (method === "elicitation/create") return this.elicitation(params);
        throw new Error(`Unsupported ACP client request: ${method}`);
    }

    /** 只结算当前会话 pending id；kind 不匹配、已结算或非法答案显式拒绝。 */
    answer(interactionId: string, answer: InteractionAnswer): void {
        if (this.closed) throw new InteractionAnswerError("closed", "Interaction broker is closed");
        const parsed = parseInteractionAnswer(answer);
        if (!parsed) throw new InteractionAnswerError("invalid_answer", "Invalid interaction answer");
        const pending = this.pending.get(interactionId);
        if (!pending || pending.sessionId !== this.sessionId)
            throw new InteractionAnswerError("unknown_interaction", `Unknown interaction: ${interactionId}`);
        if (pending.kind !== parsed.kind)
            throw new InteractionAnswerError("kind_mismatch", `Interaction ${interactionId} expects ${pending.kind}`);
        if (!pending.settle(parsed))
            throw new InteractionAnswerError("kind_mismatch", `Interaction ${interactionId} rejected the answer`);
    }

    /** 轮次结束/关闭/会话切换：安全默认结算全部 pending，并发布一次性失效事实。 */
    abortAll(): void {
        if (this.pending.size === 0) return;
        const pending = [...this.pending.values()];
        this.pending.clear();
        for (const item of pending) item.abort();
        this.options.emit({ kind: "interactions-expired" });
    }

    close(): void {
        this.closed = true;
        this.abortAll();
    }

    private async permission(params: unknown): Promise<unknown> {
        const value = record(params);
        const sessionId = this.requireSession(value?.sessionId);
        const tool = record(value?.toolCall);
        const toolCallId = nonEmpty(tool?.toolCallId);
        if (!toolCallId) throw new Error("ACP permission has no toolCallId");
        const choices = Array.isArray(value?.options) ? value.options.flatMap((raw) => {
            const choice = record(raw);
            const optionId = nonEmpty(choice?.optionId);
            return optionId ? [{ optionId, name: nonEmpty(choice?.name) ?? optionId }] : [];
        }) : [];
        const id = `interaction:${crypto.randomUUID()}`;
        const recordFact: InteractionFactRecord = {
            id,
            kind: "permission",
            sessionId,
            toolCallId,
            title: nonEmpty(tool?.title) ?? "Tool",
            options: choices,
            ...(this.turnId() ? { turnId: this.turnId()! } : {}),
        };
        const decision = await this.awaitAnswer<"allow_once" | "reject_once">(
            id,
            "permission",
            sessionId,
            recordFact,
            (answer) => answer.kind === "permission"
                ? answer.decision === "allow_once" && choices.some((choice) => choice.optionId === "allow_once")
                    ? "allow_once" : "reject_once"
                : null,
            () => "reject_once",
        );
        return { outcome: { outcome: "selected", optionId: decision } };
    }

    private async elicitation(params: unknown): Promise<unknown> {
        const value = record(params);
        const sessionId = this.requireSession(value?.sessionId);
        const message = typeof value?.message === "string" ? value.message : "";
        const schema = record(value?.requestedSchema);
        const parsed = parseAskUserQuestionElicitation(value);
        if (parsed.kind === "invalid") {
            // 形状不合契约的 AskUserQuestion 没有可作答界面：不发布记录，直接拒绝。
            console.error("[peri-sdk] Invalid AskUserQuestion elicitation", parsed.reason);
            return { action: "decline" } satisfies ElicitationDecision;
        }
        if (parsed.kind === "ask-user") return this.askUserQuestion(parsed.request, sessionId, message);
        return this.genericElicitation(schema, sessionId, message);
    }

    private async askUserQuestion(
        request: { message: string; questions: AskUserQuestion[] },
        sessionId: string,
        message: string,
    ): Promise<AskUserQuestionDecision> {
        const id = `interaction:${crypto.randomUUID()}`;
        const recordFact: InteractionFactRecord = {
            id,
            kind: "elicitation",
            sessionId,
            title: "需要你的回答",
            message,
            // 普通 elicitation 用 fields，permission 用 options；ask_user 只发结构化 questions。
            questions: toInteractionQuestions(request.questions),
            variant: "ask_user",
            ...(this.turnId() ? { turnId: this.turnId()! } : {}),
        };
        const decision = await this.awaitAnswer<AskUserQuestionDecision>(
            id,
            "elicitation",
            sessionId,
            recordFact,
            (answer) => answer.kind === "elicitation"
                ? normalizeAskUserQuestionDecision(answer.decision, request.questions)
                : null,
            () => ({ action: "decline" }),
            (decision) => decision.action,
        );
        return decision;
    }

    private async genericElicitation(
        schema: Record<string, unknown> | null,
        sessionId: string,
        message: string,
    ): Promise<ElicitationDecision> {
        const id = `interaction:${crypto.randomUUID()}`;
        const form = evaluateElicitationForm(schema);
        const recordFact: InteractionFactRecord = {
            id,
            kind: "elicitation",
            sessionId,
            title: "需要你的输入",
            message,
            fields: formFields(form.fields),
            ...(form.supported ? {} : { formSupport: "unsupported", formReason: form.reason }),
            ...(this.turnId() ? { turnId: this.turnId()! } : {}),
        };
        if (!form.supported) {
            // 不支持的表单没有可作答的界面：只记录一次失效的公开事实，按拒绝结算。
            this.options.emit({ kind: "interaction-began", record: recordFact });
            this.options.emit({ kind: "interaction-settled", id, status: "resolved", outcome: "decline" });
            return { action: "decline" };
        }
        const decision = await this.awaitAnswer<ElicitationDecision>(
            id,
            "elicitation",
            sessionId,
            recordFact,
            (answer) => {
                if (answer.kind !== "elicitation") return null;
                const value = answer.decision;
                if (value.action === "accept") {
                    const content = record(value.content);
                    return content && validateElicitationContent(form.fields, content)
                        ? { action: "accept", content }
                        : { action: "decline" };
                }
                return { action: value.action };
            },
            () => ({ action: "decline" }),
            (decision) => decision.action,
        );
        return decision;
    }

    /**
     * 登记 pending、发布 `interaction-began`，并在 answer/abort 时结算。
     * `normalize` 返回 `null` 表示答案类型不匹配；`fallback` 用于 abort/非法内容。
     */
    private async awaitAnswer<T>(
        id: string,
        kind: "permission" | "elicitation",
        sessionId: string,
        recordFact: InteractionFactRecord,
        normalize: (answer: InteractionAnswer) => T | null,
        fallback: () => T,
        outcome: (value: T) => string = (value) => String(value),
    ): Promise<T> {
        return new Promise<T>((resolve) => {
            let settled = false;
            const finish = (value: T, status: "resolved" | "expired", outcomeText?: string): void => {
                if (settled) return;
                settled = true;
                this.pending.delete(id);
                resolve(value);
                if (status === "expired") return;
                this.options.emit({
                    kind: "interaction-settled",
                    id,
                    status,
                    ...(outcomeText === undefined ? {} : { outcome: outcomeText }),
                });
            };
            this.pending.set(id, {
                kind,
                sessionId,
                settle: (answer) => {
                    const value = normalize(answer);
                    if (value === null) return false;
                    finish(value, "resolved", outcome(value));
                    return true;
                },
                // 失效由 abortAll 统一发布 `interactions-expired`，不逐条伪造已结算。
                abort: () => finish(fallback(), "expired"),
            });
            this.options.emit({ kind: "interaction-began", record: recordFact });
        });
    }

    private requireSession(raw: unknown): string {
        const sessionId = nonEmpty(raw);
        if (!sessionId) throw new Error("ACP interaction has an invalid sessionId");
        if (this.sessionId !== null && sessionId !== this.sessionId)
            throw new Error("ACP interaction has an invalid sessionId");
        return sessionId;
    }

    private turnId(): string | null {
        return this.options.currentTurnId?.() ?? null;
    }
}
