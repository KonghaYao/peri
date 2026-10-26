import type { ChatBindingTarget } from "../chat/types";
import type { ChatFactEnvelope, ChatFactObserver, ChatFactSource } from "../chat/facts";
import { SessionDocSync } from "../sync/index";
import { ChatDocument } from "./chat-document";
import { ChatSyncConnection, type ChatSyncOptions } from "./chat-sync";
import type { ChatYjsChannel } from "./channel";

export type ChatViewOptions = {
    sync?: ChatSyncOptions;
};

export type ChatViewSync = { close(): Promise<void> };

/**
 * 会话事实 → Yjs 投影与同步生命周期。
 *
 * 每个绑定代次拥有独立文档对与 SessionDocSync；晚挂 View 由事实日志重放重建。
 * View 只读事实：不执行动作、不决定权限、不取消任务、不关闭宿主。
 */
export class ChatView {
    private document: ChatDocument | null = null;
    private syncSource: SessionDocSync | null = null;
    private target: ChatBindingTarget | null = null;
    private readonly connections = new Set<ChatSyncConnection>();
    private readonly pendingConnections = new Set<ChatSyncConnection>();
    private pendingSync: SessionDocSync | null = null;
    private pendingDocument: ChatDocument | null = null;
    private unsubscribeFacts: (() => void) | null = null;
    private disposed = false;
    private sourceClosed = false;
    private disposal: Promise<void> | null = null;

    constructor(
        private readonly source: ChatFactSource,
        private readonly options: ChatViewOptions = {},
    ) {
        const observer: ChatFactObserver = {
            onBinding: (target, replay) => this.install(target, replay),
            onFact: (envelope) => this.apply(envelope),
            onClosed: () => this.closeDisplay(),
        };
        this.unsubscribeFacts = source.subscribeFacts(observer);
    }

    /** 当前绑定身份（只读诊断）；展示数据只经 ChatReplica 读取。 */
    get binding(): ChatBindingTarget | null { return this.target; }

    sync(channel: ChatYjsChannel): ChatViewSync {
        if (this.disposed) throw new Error("ChatView is disposed");
        const connection = new ChatSyncConnection(() => this.syncSource, channel, {
            ...this.options.sync,
            onTerminated: () => { this.connections.delete(connection); },
        });
        this.connections.add(connection);
        if (this.syncSource) connection.replaceSource(this.syncSource);
        return { close: () => connection.close() };
    }

    /**
     * 释放订阅、连接与文档。
     *
     * 首次调用同步封闭 View 并取走资源；任一释放步骤失败时保留失败资源，
     * 清除 rejected 缓存并抛错，下一次调用只重试未完成的部分。
     */
    dispose(): Promise<void> {
        if (this.disposal) return this.disposal;
        if (!this.disposed) {
            this.disposed = true;
            this.unsubscribeFacts?.();
            this.unsubscribeFacts = null;
            for (const connection of this.connections) {
                this.pendingConnections.add(connection);
            }
            this.connections.clear();
            this.pendingSync = this.syncSource;
            this.pendingDocument = this.document;
            this.syncSource = null;
            this.document = null;
            this.target = null;
        }
        const attempt = this.releasePending();
        this.disposal = attempt;
        void attempt.catch(() => {
            if (this.disposal === attempt) this.disposal = null;
        });
        return attempt;
    }

    private async releasePending(): Promise<void> {
        const failures: unknown[] = [];
        const connections = [...this.pendingConnections];
        const results = await Promise.allSettled(
            connections.map(async (connection) => {
                await connection.close();
                this.pendingConnections.delete(connection);
            }),
        );
        for (const result of results) {
            if (result.status === "rejected") failures.push(result.reason);
        }
        if (this.pendingSync) {
            try {
                await this.pendingSync.close();
                this.pendingSync = null;
            } catch (error) {
                failures.push(error);
            }
        }
        if (this.pendingDocument) {
            try {
                this.pendingDocument.destroy();
                this.pendingDocument = null;
            } catch (error) {
                failures.push(error);
            }
        }
        if (failures.length > 0) {
            throw new AggregateError(
                failures,
                "ChatView disposal failed to close every sync connection",
            );
        }
    }

    private install(target: ChatBindingTarget, replay: readonly ChatFactEnvelope[]): void {
        if (this.disposed) return;
        if (this.target?.bindingRevision === target.bindingRevision
            && this.target.sessionId === target.sessionId
            && this.document) return;
        // 先构建新代次；失败时不替换当前展示。
        const document = new ChatDocument({ binding: target });
        try {
            document.applyFacts(replay);
        } catch (error) {
            document.destroy();
            throw error;
        }
        const sync = new SessionDocSync(document.chat, document.session);
        const previousDocument = this.document;
        const previousSync = this.syncSource;
        this.document = document;
        this.syncSource = sync;
        this.target = { ...target };
        for (const connection of [...this.connections]) connection.replaceSource(sync);
        if (previousSync) void previousSync.close().catch(() => {});
        previousDocument?.destroy();
    }

    private apply(envelope: ChatFactEnvelope): void {
        if (this.disposed || this.sourceClosed || !this.document) return;
        if (this.target && envelope.bindingRevision !== this.target.bindingRevision) return;
        this.document.applyEnvelope(envelope);
    }

    private closeDisplay(): void {
        if (this.disposed || !this.document) return;
        this.sourceClosed = true;
        this.document.applyClosed();
    }
}
