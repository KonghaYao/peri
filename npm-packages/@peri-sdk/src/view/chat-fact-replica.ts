import * as Y from "yjs";
import type { ChatBindingTarget, ChatFactEnvelope } from "../contracts/chat";
import type { ProjectionClock } from "../state/doc-model";
import { ChatDocument } from "./chat-document";
import { ReadCache } from "./read-cache";
import { readSessionView, type SessionView } from "./session-view";

export type ChatFactReplicaStatus = "connecting" | "synced" | "resynced" | "failed";

export type ChatFactReplicaOptions = {
    now?: ProjectionClock;
    /** 预绑定代次；文档立即就绪，但 status 仍为 connecting，直到收到快照。 */
    binding?: ChatBindingTarget;
};

export type ChatFactSnapshot = {
    binding: ChatBindingTarget;
    envelopes: readonly ChatFactEnvelope[];
    /** 事实日志已截断：只能重建同代次的部分历史，status 标记为 resynced。 */
    overflow: boolean;
};

function emptyView(): SessionView {
    const chat = new Y.Doc();
    const session = new Y.Doc();
    try {
        return readSessionView(chat, session);
    } finally {
        chat.destroy();
        session.destroy();
    }
}

function copyBinding(target: ChatBindingTarget): ChatBindingTarget {
    return {
        chatId: target.chatId,
        sessionId: target.sessionId,
        bindingRevision: target.bindingRevision,
    };
}

function sameBinding(left: ChatBindingTarget | null, right: ChatBindingTarget): boolean {
    return left !== null
        && left.chatId === right.chatId
        && left.sessionId === right.sessionId
        && left.bindingRevision === right.bindingRevision;
}

function belongs(target: ChatBindingTarget, envelope: ChatFactEnvelope): boolean {
    return envelope.chatId === target.chatId
        && envelope.sessionId === target.sessionId
        && envelope.bindingRevision === target.bindingRevision;
}

/**
 * 事实日志 → 只读 SessionView 的浏览器投影。
 *
 * 与 `ChatReplica` 的数据来源不同：这里直接消费 `ChatFactEnvelope` 快照与增量，
 * 不经 Yjs 同步通道，也不发起任何 IO。绑定换代时销毁旧文档并重建，旧代次事实
 * 不会写入新文档；通知按微任务合并，读取复用 `ReadCache`。
 */
export class ChatFactReplica {
    private readonly now: ProjectionClock;
    private readonly empty: SessionView;
    private document: ChatDocument | null = null;
    private cache = new ReadCache();
    private view: SessionView;
    private readonly listeners = new Set<() => void>();
    private statusValue: ChatFactReplicaStatus = "connecting";
    private target: ChatBindingTarget | null = null;
    private disposed = false;
    private scheduled = false;
    private readonly onTransaction = (transaction: Y.Transaction): void => {
        if (!transaction.changed.size) return;
        if ([...transaction.changed.keys()].some((type) => type.parent === null)) this.cache = new ReadCache();
        else this.cache.invalidate(transaction);
        this.schedule();
    };

    constructor(options: ChatFactReplicaOptions = {}) {
        this.now = options.now ?? (() => new Date().toISOString());
        this.empty = emptyView();
        this.view = this.empty;
        if (options.binding) {
            this.replaceDocument(options.binding, []);
            this.view = this.recompute();
        }
    }

    get status(): ChatFactReplicaStatus { return this.statusValue; }
    get binding(): ChatBindingTarget | null { return this.target; }
    /** 无绑定时为 0；换代后立即反映新代次。 */
    get bindingRevision(): number { return this.target?.bindingRevision ?? 0; }

    /**
     * 应用一次事实快照。
     *
     * 同一绑定内的快照是增量重放（按 sequence 去重）；绑定换代则重建文档，
     * 快照中不属于新代次的事实被丢弃。overflow 只影响状态，不改变事实过滤规则。
     */
    applySnapshot(input: ChatFactSnapshot): void {
        if (this.disposed) return;
        const binding = copyBinding(input.binding);
        if (!this.document || !sameBinding(this.target, binding)) {
            this.replaceDocument(binding, input.envelopes);
        } else {
            this.document.applyFacts(input.envelopes.filter((envelope) => belongs(binding, envelope)));
        }
        this.statusValue = input.overflow ? "resynced" : "synced";
        this.schedule();
    }

    /** 应用单条增量事实；重复序号、旧代次或已关闭会话返回 false。 */
    applyFact(envelope: ChatFactEnvelope): boolean {
        const document = this.document;
        if (this.disposed || !document || !this.target || !belongs(this.target, envelope)) return false;
        const applied = document.applyEnvelope(envelope);
        if (applied) this.schedule();
        return applied;
    }

    read(): SessionView {
        return this.view;
    }

    subscribe(listener: () => void): () => void {
        if (this.disposed) return () => {};
        this.listeners.add(listener);
        return () => { this.listeners.delete(listener); };
    }

    /** 数据源报告传输失败；错误诊断由调用方记录，副本只切换状态。 */
    fail(_error: Error): void {
        if (this.disposed) return;
        this.statusValue = "failed";
        this.schedule();
    }

    /** 清空绑定与文档，回到等待首屏的 connecting 状态。 */
    reset(): void {
        if (this.disposed) return;
        this.detach();
        this.document?.destroy();
        this.document = null;
        this.target = null;
        this.cache = new ReadCache();
        this.view = this.empty;
        this.statusValue = "connecting";
        this.schedule();
    }

    dispose(): void {
        if (this.disposed) return;
        this.disposed = true;
        this.detach();
        this.document?.destroy();
        this.document = null;
        this.target = null;
        this.cache = new ReadCache();
        this.view = this.empty;
        this.listeners.clear();
    }

    /** 先构建并重放新代次，成功后才替换当前展示。 */
    private replaceDocument(binding: ChatBindingTarget, envelopes: readonly ChatFactEnvelope[]): void {
        const document = new ChatDocument({ now: this.now, binding });
        try {
            document.applyFacts(envelopes.filter((envelope) => belongs(binding, envelope)));
        } catch (error) {
            document.destroy();
            throw error;
        }
        this.detach();
        this.document?.destroy();
        this.document = document;
        this.target = copyBinding(binding);
        this.cache = new ReadCache();
        this.attach();
    }

    private attach(): void {
        this.document?.chat.on("afterTransaction", this.onTransaction);
        this.document?.session.on("afterTransaction", this.onTransaction);
    }

    private detach(): void {
        this.document?.chat.off("afterTransaction", this.onTransaction);
        this.document?.session.off("afterTransaction", this.onTransaction);
    }

    private schedule(): void {
        if (this.scheduled || this.disposed) return;
        this.scheduled = true;
        queueMicrotask(() => {
            this.scheduled = false;
            if (this.disposed) return;
            this.view = this.recompute();
            for (const listener of [...this.listeners]) listener();
        });
    }

    private recompute(): SessionView {
        const document = this.document;
        return document ? readSessionView(document.chat, document.session, this.cache) : this.empty;
    }
}
