import { parseInteractionAnswer, type InteractionAnswer } from "../agent/interaction-responder";
import { ChatActionValidationError } from "./errors";
import { cloneJsonValue, NonJsonValueError } from "./json-value";

/**
 * Chat 用户动作（Phase 0 冻结）。
 *
 * Action 只承载可序列化意图：`type`、稳定 `requestId` 与纯数据 `payload`。
 * 工厂与解析都会把 payload 校验、深拷贝并深冻结为 JSON 数据；函数、BigInt、
 * `undefined`、非有限数、循环引用、Date/Map/Set/class 实例等会被
 * `ChatActionValidationError`（`invalid_action`）拒绝，不会被 `JSON.stringify`
 * 静默丢弃。
 * 不使用 `instanceof` 判断来源；跨 HTTP/WS 传入的普通对象一律经
 * `parseChatUserAction` 按类型与字段校验。解析保留原 `requestId`，不重建。
 */

export type ChatActionType =
    | "create_new_session" | "change_session"
    | "send" | "force_send" | "take_back"
    | "load_earlier_history" | "cancel" | "answer";

export interface ChatUserActionBase<T extends ChatActionType, P> {
    readonly type: T;
    readonly requestId: string;
    readonly payload: P;
}

export type CreateNewSessionAction = ChatUserActionBase<"create_new_session", { title?: string }>;
export type ChangeSessionAction = ChatUserActionBase<"change_session", { sessionId: string }>;
export type SendAction = ChatUserActionBase<"send", { text: string }>;
export type ForceSendAction = ChatUserActionBase<"force_send", { inputId: string }>;
export type TakeBackAction = ChatUserActionBase<"take_back", { inputId: string }>;
export type LoadEarlierHistoryAction = ChatUserActionBase<"load_earlier_history", { limit?: number }>;
export type CancelAction = ChatUserActionBase<"cancel", Record<string, never>>;
export type AnswerAction = ChatUserActionBase<"answer", { interactionId: string; answer: InteractionAnswer }>;
export type SessionAction = CreateNewSessionAction | ChangeSessionAction;
export type ControlAction = ForceSendAction | TakeBackAction | CancelAction | AnswerAction;
export type HistoryAction = LoadEarlierHistoryAction;
export type ChatUserAction = SessionAction | SendAction | ControlAction | HistoryAction;

/** 沿用 CF wire 的单条输入上限。 */
const MAX_SEND_TEXT_LENGTH = 32_768;

const ACTION_TYPES: ReadonlySet<string> = new Set<ChatActionType>([
    "create_new_session", "change_session", "send", "force_send", "take_back",
    "load_earlier_history", "cancel", "answer",
]);

function isRecord(value: unknown): value is Record<string, unknown> {
    return value !== null && typeof value === "object" && !Array.isArray(value);
}

function validationError(requestId: string, message: string): never {
    throw new ChatActionValidationError(requestId, message);
}

function isChatActionType(value: unknown): value is ChatActionType {
    return typeof value === "string" && ACTION_TYPES.has(value);
}

function resolveRequestId(provided: string | undefined): string {
    if (provided === undefined) return crypto.randomUUID();
    if (typeof provided !== "string" || provided.length === 0) {
        validationError(typeof provided === "string" ? provided : "", "requestId must be a non-empty string");
    }
    return provided;
}

function readRequestId(raw: Record<string, unknown>): string {
    const requestId = raw.requestId;
    if (typeof requestId !== "string" || requestId.length === 0) {
        validationError(typeof requestId === "string" ? requestId : "", "requestId must be a non-empty string");
    }
    return requestId;
}

function requireNonEmptyString(value: unknown, requestId: string, field: string): string {
    if (typeof value !== "string" || value.length === 0) {
        validationError(requestId, `${field} must be a non-empty string`);
    }
    return value;
}

function optionalString(value: unknown, requestId: string, field: string): string | undefined {
    if (value === undefined) return undefined;
    if (typeof value !== "string") validationError(requestId, `${field} must be a string`);
    return value;
}

function optionalPositiveInteger(value: unknown, requestId: string, field: string): number | undefined {
    if (value === undefined) return undefined;
    if (typeof value !== "number" || !Number.isInteger(value) || value <= 0) {
        validationError(requestId, `${field} must be a positive integer`);
    }
    return value;
}

/** 工厂与解析共用：payload 进入 Action 前必须是可无损 JSON 往返的纯数据。 */
function freezeJsonPayload<P>(payload: P, requestId: string): P {
    try {
        return cloneJsonValue<P>(payload, "payload");
    } catch (error) {
        // 只区分本模块内部抛出的校验错误，不用于判断跨进程 Action 的来源。
        if (error instanceof NonJsonValueError) validationError(requestId, error.message);
        throw error;
    }
}

function buildAction<T extends ChatActionType, P>(
    type: T,
    requestId: string,
    payload: P,
): ChatUserActionBase<T, P> {
    return Object.freeze({ type, requestId, payload: freezeJsonPayload(payload, requestId) });
}

/** 按 `type` + 字段校验并规范化；失败抛 `ChatActionValidationError`（`invalid_action`）。 */
export function parseChatUserAction(raw: unknown): ChatUserAction {
    if (!isRecord(raw)) validationError("", "action must be a JSON object");
    const requestId = readRequestId(raw);
    const type = raw.type;
    if (!isChatActionType(type)) validationError(requestId, `unsupported action type: ${String(type)}`);
    const payload = raw.payload;
    if (!isRecord(payload)) validationError(requestId, "payload must be a JSON object");

    switch (type) {
        case "create_new_session": {
            const title = optionalString(payload.title, requestId, "payload.title");
            return buildAction("create_new_session", requestId, title === undefined ? {} : { title });
        }
        case "change_session":
            return buildAction("change_session", requestId, {
                sessionId: requireNonEmptyString(payload.sessionId, requestId, "payload.sessionId"),
            });
        case "send": {
            const text = requireNonEmptyString(payload.text, requestId, "payload.text");
            // 与 CF canonical 的 `content.trim().length > 0` 对齐；只校验，不改写文本。
            if (text.trim().length === 0) validationError(requestId, "payload.text must not be blank");
            if (text.length > MAX_SEND_TEXT_LENGTH) {
                validationError(requestId, `payload.text must not exceed ${MAX_SEND_TEXT_LENGTH} characters`);
            }
            return buildAction("send", requestId, { text });
        }
        case "force_send":
            return buildAction("force_send", requestId, {
                inputId: requireNonEmptyString(payload.inputId, requestId, "payload.inputId"),
            });
        case "take_back":
            return buildAction("take_back", requestId, {
                inputId: requireNonEmptyString(payload.inputId, requestId, "payload.inputId"),
            });
        case "load_earlier_history": {
            const limit = optionalPositiveInteger(payload.limit, requestId, "payload.limit");
            return buildAction("load_earlier_history", requestId, limit === undefined ? {} : { limit });
        }
        case "cancel":
            if (Object.keys(payload).length > 0) validationError(requestId, "payload for cancel must be empty");
            return buildAction("cancel", requestId, {});
        case "answer": {
            const interactionId = requireNonEmptyString(payload.interactionId, requestId, "payload.interactionId");
            const answer = parseInteractionAnswer(payload.answer);
            if (!answer) validationError(requestId, "payload.answer is not a valid interaction answer");
            return buildAction("answer", requestId, { interactionId, answer });
        }
    }
}

export const ChatUserAction: {
    createNewSession(input?: { title?: string; requestId?: string }): CreateNewSessionAction;
    changeSessionTo(sessionId: string, options?: { requestId?: string }): ChangeSessionAction;
    send(input: { text: string; requestId?: string }): SendAction;
    forceSend(inputId: string, options?: { requestId?: string }): ForceSendAction;
    takeBack(inputId: string, options?: { requestId?: string }): TakeBackAction;
    loadEarlierHistory(input?: { limit?: number; requestId?: string }): LoadEarlierHistoryAction;
    cancel(options?: { requestId?: string }): CancelAction;
    answer(input: { interactionId: string; answer: InteractionAnswer; requestId?: string }): AnswerAction;
} = {
    createNewSession(input = {}) {
        const requestId = resolveRequestId(input.requestId);
        const title = input.title;
        return parseChatUserAction({
            type: "create_new_session", requestId,
            payload: title === undefined ? {} : { title },
        }) as CreateNewSessionAction;
    },
    changeSessionTo(sessionId, options = {}) {
        const requestId = resolveRequestId(options.requestId);
        return parseChatUserAction({
            type: "change_session", requestId, payload: { sessionId },
        }) as ChangeSessionAction;
    },
    send(input) {
        const requestId = resolveRequestId(input.requestId);
        return parseChatUserAction({
            type: "send", requestId, payload: { text: input.text },
        }) as SendAction;
    },
    forceSend(inputId, options = {}) {
        const requestId = resolveRequestId(options.requestId);
        return parseChatUserAction({
            type: "force_send", requestId, payload: { inputId },
        }) as ForceSendAction;
    },
    takeBack(inputId, options = {}) {
        const requestId = resolveRequestId(options.requestId);
        return parseChatUserAction({
            type: "take_back", requestId, payload: { inputId },
        }) as TakeBackAction;
    },
    loadEarlierHistory(input = {}) {
        const requestId = resolveRequestId(input.requestId);
        return parseChatUserAction({
            type: "load_earlier_history", requestId,
            payload: input.limit === undefined ? {} : { limit: input.limit },
        }) as LoadEarlierHistoryAction;
    },
    cancel(options = {}) {
        const requestId = resolveRequestId(options.requestId);
        return parseChatUserAction({
            type: "cancel", requestId, payload: {},
        }) as CancelAction;
    },
    answer(input) {
        const requestId = resolveRequestId(input.requestId);
        return parseChatUserAction({
            type: "answer", requestId,
            payload: { interactionId: input.interactionId, answer: input.answer },
        }) as AnswerAction;
    },
};
