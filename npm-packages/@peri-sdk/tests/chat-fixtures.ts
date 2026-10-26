import type { PeriInstancePort } from "../src/agent/port";
import type {
    ChatDatabase,
    ChatSessionDirectoryEntry,
} from "../src/chat/types";
import type { ChatFact } from "../src/contracts/chat";
import type { InteractionAnswer } from "../src/contracts/interaction";
import {
    PayloadReadError,
    type ReadPayloadOptions,
    type ToolPayloadReadRef,
    type ToolPayloadResponse,
} from "../src/payload/reader";

interface Deferred {
    readonly promise: Promise<void>;
    resolve(): void;
    reject(error: unknown): void;
}

function deferred(): Deferred {
    let resolve!: () => void;
    let reject!: (error: unknown) => void;
    const promise = new Promise<void>((onResolve, onReject) => {
        resolve = onResolve;
        reject = onReject;
    });
    return { promise, resolve, reject };
}

/** 可编程的 PeriInstancePort；测试不依赖 Stream A 的真实实现。 */
export class FakePort implements PeriInstancePort {
    readonly calls: string[] = [];
    readonly sendCalls: Array<{ inputId: string; text: string }> = [];
    readonly dispatches: string[] = [];
    readonly takeBacks: string[] = [];
    readonly answers: Array<{
        interactionId: string;
        answer: InteractionAnswer;
    }> = [];

    runningValue = false;
    pendingInputValue = 0;
    pendingInteractionValue = 0;
    needsReloadValue = false;
    closeCalls = 0;
    closeError?: unknown;
    bindNewError?: unknown;
    bindExistingError?: unknown;
    /** 在 `session/new` 结果结算前暂停（模拟启动中的慢宿主）。 */
    bindNewGate?: Promise<void>;
    /** 在 `onSessionCreated` 回调后暂停（模拟已创建会话、后续阶段未完成）。 */
    bindNewCreatedGate?: Promise<void>;
    bindExistingGate?: Promise<void>;
    sendGate?: Promise<void>;
    bindNewSessionId = "session-new";
    bindNewFacts: ChatFact[] = [];
    bindExistingFacts: ChatFact[] = [];
    historyResult = { applied: 0, nextCursor: null as string | null };
    historyError?: unknown;
    cancelStatus: "requested" | "idle" = "requested";
    cancelError?: unknown;
    dispatchError?: unknown;
    takeBackError?: unknown;
    answerError?: unknown;
    payloadReader?:
        | ((ref: ToolPayloadReadRef) => Promise<ToolPayloadResponse>)
        | undefined;

    private readonly sinks = new Set<(fact: ChatFact) => void>();
    private readonly waiters = new Map<string, Deferred>();
    private readonly delivered = new Set<string>();
    private readonly bindNewStartedDeferred = deferred();
    private readonly bindExistingStartedDeferred = deferred();

    readonly bindNewStarted = this.bindNewStartedDeferred.promise;
    readonly bindExistingStarted = this.bindExistingStartedDeferred.promise;

    get running(): boolean {
        return this.runningValue;
    }

    get pendingInputCount(): number {
        return this.pendingInputValue;
    }

    get pendingInteractionCount(): number {
        return this.pendingInteractionValue;
    }

    get needsReload(): boolean {
        return this.needsReloadValue;
    }

    attach(sink: (fact: ChatFact) => void): () => void {
        this.sinks.add(sink);
        return () => {
            this.sinks.delete(sink);
        };
    }

    emit(fact: ChatFact): void {
        for (const sink of [...this.sinks]) sink(fact);
    }

    async bindNew(options: {
        onSessionCreated?: (sessionId: string) => void | Promise<void>;
    } = {}): Promise<{ sessionId: string }> {
        this.calls.push("bindNew");
        this.bindNewStartedDeferred.resolve();
        if (this.bindNewGate) await this.bindNewGate;
        if (this.bindNewError) throw this.bindNewError;
        await options.onSessionCreated?.(this.bindNewSessionId);
        if (this.bindNewCreatedGate) await this.bindNewCreatedGate;
        for (const fact of this.bindNewFacts) this.emit(fact);
        return { sessionId: this.bindNewSessionId };
    }

    async bindExisting(sessionId: string): Promise<{ sessionId: string }> {
        this.calls.push(`bindExisting:${sessionId}`);
        this.bindExistingStartedDeferred.resolve();
        if (this.bindExistingGate) await this.bindExistingGate;
        if (this.bindExistingError) throw this.bindExistingError;
        for (const fact of this.bindExistingFacts) this.emit(fact);
        return { sessionId };
    }

    async send(inputId: string, text: string): Promise<void> {
        this.calls.push(`send:${inputId}`);
        this.sendCalls.push({ inputId, text });
        if (this.sendGate) await this.sendGate;
        this.pendingInputValue += 1;
    }

    waitForDelivery(inputId: string): Promise<void> {
        this.calls.push(`wait:${inputId}`);
        if (this.delivered.delete(inputId)) return Promise.resolve();
        const existing = this.waiters.get(inputId);
        if (existing) return existing.promise;
        const waiter = deferred();
        this.waiters.set(inputId, waiter);
        return waiter.promise;
    }

    deliver(inputId: string, text = "delivered"): void {
        this.pendingInputValue = Math.max(0, this.pendingInputValue - 1);
        this.delivered.add(inputId);
        this.waiters.get(inputId)?.resolve();
        this.waiters.delete(inputId);
        this.emit({ kind: "input-delivered", inputId, text });
    }

    failDelivery(inputId: string, error: unknown): void {
        this.waiters.get(inputId)?.reject(error);
        this.waiters.delete(inputId);
    }

    async dispatch(inputId: string): Promise<void> {
        this.calls.push(`dispatch:${inputId}`);
        if (this.dispatchError) throw this.dispatchError;
        this.dispatches.push(inputId);
    }

    async takeBack(inputId: string): Promise<void> {
        this.calls.push(`takeBack:${inputId}`);
        if (this.takeBackError) throw this.takeBackError;
        this.takeBacks.push(inputId);
    }

    async cancel(): Promise<"requested" | "idle"> {
        this.calls.push("cancel");
        if (this.cancelError) throw this.cancelError;
        if (!this.runningValue) return "idle";
        return this.cancelStatus;
    }

    async answer(
        interactionId: string,
        answer: InteractionAnswer,
    ): Promise<void> {
        this.calls.push(`answer:${interactionId}`);
        if (this.answerError) throw this.answerError;
        this.answers.push({ interactionId, answer });
    }

    async loadEarlierHistory(): Promise<{
        applied: number;
        nextCursor: string | null;
    }> {
        this.calls.push("loadEarlierHistory");
        if (this.historyError) throw this.historyError;
        return this.historyResult;
    }

    async close(): Promise<void> {
        this.closeCalls += 1;
        this.calls.push("close");
        if (this.closeError) throw this.closeError;
        this.emit({ kind: "closed" });
    }

    /** 显式实现冻结端口；没有配置载荷仓库时返回 `unavailable`，不做能力探测。 */
    async readPayload(
        ref: ToolPayloadReadRef,
        options: ReadPayloadOptions = {},
    ): Promise<ToolPayloadResponse> {
        this.calls.push(`readPayload:${ref.id}`);
        if (options.signal?.aborted) {
            throw new PayloadReadError("aborted", "载荷读取已取消");
        }
        if (!this.payloadReader) {
            throw new PayloadReadError("unavailable", "FakePort 没有可读载荷");
        }
        return this.payloadReader(ref);
    }
}

export class FakeDatabase implements ChatDatabase {
    readonly entries = new Map<string, ChatSessionDirectoryEntry>();
    readonly listCalls: Array<string | undefined> = [];
    readonly getCalls: string[] = [];
    listError?: unknown;

    seed(entry: ChatSessionDirectoryEntry): void {
        this.entries.set(entry.sessionId, entry);
    }

    async listSessions(
        path?: string,
    ): Promise<ChatSessionDirectoryEntry[]> {
        this.listCalls.push(path);
        if (this.listError) throw this.listError;
        return [...this.entries.values()];
    }

    async getSession(
        id: string,
    ): Promise<ChatSessionDirectoryEntry | null> {
        this.getCalls.push(id);
        return this.entries.get(id) ?? null;
    }
}

export function directoryEntry(
    sessionId: string,
    cwd = "/workspace",
): ChatSessionDirectoryEntry {
    return {
        sessionId,
        title: null,
        cwd,
        messageCount: 0,
        createdAt: "2026-01-01T00:00:00.000Z",
        updatedAt: "2026-01-01T00:00:00.000Z",
    };
}
