import type { ChatBindingTarget, ChatFact, ChatFactEnvelope } from "../contracts/chat";
import { ChatFactReplica } from "./chat-fact-replica";
import type { SessionView } from "./session-view";

/** `handle.facts()` 的结构形状：整份快照与当前绑定代次。 */
export type ChatStateSnapshotInput = {
    readonly binding: ChatBindingTarget | null;
    readonly envelopes: readonly ChatFactEnvelope[];
    readonly overflow: boolean;
};

/**
 * Recursively readonly projection of the public snapshot.
 *
 * `Readonly<T>` only protects top-level properties; without this, arrays and nested
 * entry/turn/session objects would still be statically writable even though every object in the
 * snapshot is frozen at runtime.
 */
export type DeepReadonly<T> = T extends (...args: never[]) => unknown
    ? T
    : T extends readonly (infer Item)[]
        ? readonly DeepReadonly<Item>[]
        : T extends object
            ? { readonly [Key in keyof T]: DeepReadonly<T[Key]> }
            : T;

/** `empty()`/`subscribe()` 对外提供的状态；复用 `SessionView`，只补消息投影。运行时深度冻结。 */
export type ChatStateSnapshot = DeepReadonly<SessionView> & {
    readonly messages: readonly DeepReadonly<ChatStateMessage>[];
};

export type ChatStateMessage = {
    readonly entryId: string;
    readonly role: "user" | "assistant";
    readonly text: string;
};

export type ChatStateInputErrorReason =
    | "invalid_snapshot"
    | "invalid_binding"
    | "invalid_envelope";

export class ChatStateInputError extends Error {
    constructor(readonly reason: ChatStateInputErrorReason, message: string) {
        super(message);
        this.name = "ChatStateInputError";
    }
}

const FACT_KINDS = new Set<ChatFact["kind"]>([
    "seed-config",
    "history-batch",
    "notice",
    "history-page",
    "input-queue",
    "input-delivered",
    "turn-complete",
    "turn-cancel",
    "interaction-began",
    "interaction-settled",
    "interactions-expired",
    "task-snapshot",
    "run-state",
    "closed",
]);

function isRecord(value: unknown): value is Record<string, unknown> {
    return value !== null && typeof value === "object" && !Array.isArray(value);
}

function invalid(reason: ChatStateInputErrorReason, message: string): never {
    throw new ChatStateInputError(reason, message);
}

function validateBinding(value: unknown, field: string): ChatBindingTarget {
    if (!isRecord(value)
        || typeof value.chatId !== "string"
        || typeof value.sessionId !== "string"
        || !Number.isSafeInteger(value.bindingRevision)
        || (value.bindingRevision as number) < 0) {
        invalid("invalid_binding", `${field} must be a ChatBindingTarget`);
    }
    return {
        chatId: value.chatId,
        sessionId: value.sessionId,
        bindingRevision: value.bindingRevision as number,
    };
}

function validateEnvelope(value: unknown, field: string): ChatFactEnvelope {
    if (!isRecord(value)
        || typeof value.chatId !== "string"
        || typeof value.sessionId !== "string"
        || !Number.isSafeInteger(value.bindingRevision)
        || (value.bindingRevision as number) < 0
        || !Number.isSafeInteger(value.sequence)
        || (value.sequence as number) < 0
        || !isRecord(value.fact)
        || typeof value.fact.kind !== "string"
        || !FACT_KINDS.has(value.fact.kind as ChatFact["kind"])) {
        invalid("invalid_envelope", `${field} must be a ChatFactEnvelope`);
    }
    return {
        chatId: value.chatId,
        sessionId: value.sessionId,
        bindingRevision: value.bindingRevision as number,
        sequence: value.sequence as number,
        fact: value.fact as ChatFact,
    };
}

function isSnapshotInput(input: unknown): input is ChatStateSnapshotInput {
    return isRecord(input) && ("binding" in input || "envelopes" in input || "overflow" in input);
}

function messagesOf(view: SessionView): ChatStateMessage[] {
    return view.entries.flatMap((entry) => {
        const text = entry.blocks.flatMap((block) => (block.type === "text" ? [block.text] : [])).join("");
        // 会话开始时 assistant 条目先建立、正文后到；空占位不是一条消息。
        return text.length === 0 ? [] : [{ entryId: entry.entryId, role: entry.role, text }];
    });
}

function freeze<T>(value: T): T {
    if (value && typeof value === "object" && !Object.isFrozen(value)) {
        for (const child of Object.values(value)) freeze(child);
        Object.freeze(value);
    }
    return value;
}

function snapshotOf(view: SessionView): ChatStateSnapshot {
    return freeze({
        ...view,
        messages: messagesOf(view),
    });
}

/**
 * 会话事实 → 只读状态的薄门面。
 *
 * 转换与通知都由 `ChatFactReplica`/`ChatDocument` 完成；这里只做边界校验、
 * 消息投影与冻结，不复制 reducer。`apply` 接受整份 `facts()` 快照或单条
 * `chatFact`，并原地更新；没有绑定时增量事实无法归属，等下一次快照重放。
 */
export class ChatState {
    static empty(): ChatState {
        return new ChatState(new ChatFactReplica());
    }

    private readonly replica: ChatFactReplica;
    private readonly listeners = new Set<(snapshot: ChatStateSnapshot) => void>();
    private current: ChatStateSnapshot;

    private constructor(replica: ChatFactReplica) {
        this.replica = replica;
        this.current = snapshotOf(replica.read());
        replica.subscribe((view) => {
            this.current = snapshotOf(view);
            for (const listener of [...this.listeners]) listener(this.current);
        });
    }

    /** 当前只读快照；与最近一次 `subscribe` 回调收到的对象一致。 */
    get snapshot(): ChatStateSnapshot {
        return this.current;
    }

    /** 吸收整份快照或单条增量；输入结构非法时抛 `ChatStateInputError`。 */
    apply(input: ChatStateSnapshotInput | ChatFactEnvelope): void {
        if (isSnapshotInput(input)) {
            if (!Array.isArray(input.envelopes)) {
                invalid("invalid_snapshot", "snapshot.envelopes must be an array");
            }
            if (typeof input.overflow !== "boolean") {
                invalid("invalid_snapshot", "snapshot.overflow must be a boolean");
            }
            const binding = input.binding === null
                ? null
                : validateBinding(input.binding, "snapshot.binding");
            const envelopes = input.envelopes.map((envelope, index) =>
                validateEnvelope(envelope, `snapshot.envelopes[${index}]`),
            );
            if (binding === null) {
                if (envelopes.length > 0) {
                    invalid("invalid_snapshot", "an unbound snapshot cannot carry envelopes");
                }
                this.replica.reset();
                return;
            }
            this.replica.applySnapshot({ binding, envelopes, overflow: input.overflow });
            return;
        }
        this.replica.applyFact(validateEnvelope(input, "fact"));
    }

    /** 订阅只读快照；返回退订函数，退订后不再收到通知。 */
    subscribe(listener: (snapshot: ChatStateSnapshot) => void): () => void {
        this.listeners.add(listener);
        return () => {
            this.listeners.delete(listener);
        };
    }
}
