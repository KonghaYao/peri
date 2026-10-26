import type { PublicError } from "../state/doc-model";
import type { HistoryPageItem } from "../state/history-merge";
import type { TurnExitStatus } from "../state/turn-machine";
import type { JsonRpcNotification } from "../transport/types";

/** Chat 绑定身份与代次；事实源与视图共用的最小快照。 */
export interface ChatBindingTarget {
    chatId: string;
    sessionId: string;
    bindingRevision: number;
}

/**
 * ChatService 向 View 暴露的运行事实（Phase 0 冻结）。
 *
 * 事实是单向输出：View 把事实投影到自己的 Y.Doc，不能借事实反向执行动作。
 * `sequence` 在单个绑定代次内单调递增；换代后由 `onBinding` 重新开始重放。
 */
export type ChatFact =
    | { kind: "seed-config"; response: unknown }
    | { kind: "history-batch"; notifications: JsonRpcNotification[] }
    | { kind: "notice"; notification: JsonRpcNotification }
    | { kind: "history-page"; items: HistoryPageItem[]; nextCursor: string | null }
    | { kind: "input-queue"; snapshot: unknown }
    | { kind: "input-delivered"; inputId: string; text: string }
    | { kind: "turn-complete"; status: TurnExitStatus; stopReason?: string }
    | { kind: "turn-cancel"; restore: boolean }
    | { kind: "interaction-began"; record: InteractionFactRecord }
    | { kind: "interaction-settled"; id: string; status: "resolved" | "expired"; outcome?: string }
    | { kind: "interactions-expired" }
    | { kind: "task-snapshot"; snapshot: unknown }
    | { kind: "run-state"; running: boolean; executionBlocked: boolean }
    | { kind: "closed"; reason?: PublicError };

export interface ChatFactEnvelope {
    readonly chatId: string;
    readonly sessionId: string;
    readonly bindingRevision: number;
    readonly sequence: number;
    readonly fact: ChatFact;
}

/** AskUserQuestion 的结构化问题；id 稳定为 `ask_user_question_<index>`。 */
export interface InteractionQuestion {
    id: string;
    header: string;
    question: string;
    multiSelect: boolean;
    options: Array<{ value: string; label: string; description?: string }>;
}

/** View 只需要公开字段；resolver/AbortController 留在 InteractionBroker。 */
export interface InteractionFactRecord {
    id: string;
    kind: "permission" | "elicitation";
    sessionId: string;
    turnId?: string;
    toolCallId?: string;
    title?: string;
    message?: string;
    options?: Array<{ optionId: string; name: string }>;
    fields?: Array<{
        key: string;
        title: string;
        description: string;
        type: "string" | "number" | "integer" | "boolean";
        required: boolean;
    }>;
    variant?: "ask_user";
    /** 仅 `variant === "ask_user"`；普通 elicitation 继续使用 `fields`。 */
    questions?: InteractionQuestion[];
    formSupport?: "unsupported";
    formReason?: string;
}

export interface ChatFactObserver {
    onBinding(snapshot: ChatBindingTarget, replay: readonly ChatFactEnvelope[]): void;
    onFact(envelope: ChatFactEnvelope): void;
    onClosed(reason?: Error): void;
}

export interface ChatFactSource {
    readonly binding: ChatBindingTarget | null;
    subscribeFacts(observer: ChatFactObserver): () => void;
}
