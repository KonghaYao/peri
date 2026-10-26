import { withDeadline } from "../util/deadline";
import type { AsyncMutex, ChatBinding } from "./binding";
import { closeChatResources } from "./close-chat";
import {
    ChatActionError,
    ChatCloseIncompleteError,
    ChatClosedError,
} from "./errors";
import type { ChatFactHub } from "./fact-hub";
import type { Activation } from "./service-support";
import type { ChatLifecycleOptions } from "./types";

/** ChatCloseController 访问 ChatService 资源的窄接口。 */
interface ChatCloseContext {
    readonly id: string;
    readonly mutex: AsyncMutex;
    readonly state: ChatBinding;
    readonly facts: ChatFactHub;
    readonly lifecycle: Required<ChatLifecycleOptions>;
    readonly pendingCleanups: Set<Activation>;
    readonly ownedSessions: Set<string>;
    takeActive(): Activation | undefined;
    releaseOwnership(sessionId: string): Promise<void>;
    releaseIndex(sessionId: string): void;
    onActivationClosed(activation: Activation): void;
    abortStartups(): void;
    failInputs(makeError: (requestId: string) => ChatActionError): void;
    onClosed(chatId: string): void;
}

/**
 * Chat 关闭事务：请求、启动结算、激活关闭与失败聚合。
 *
 * `close()` 同步封闭准入并中止启动，然后给在途绑定一个有界出口；
 * finalizeClose **不进入 AsyncMutex**，否则卡住的 loadSession 会永久挡住关闭。
 * 超时一律是 Incomplete：保留 ownership/索引/未确认激活供再次 close/shutdown 重试。
 */
export class ChatCloseController {
    private closePromise?: Promise<void>;
    private requested = false;

    constructor(private readonly context: ChatCloseContext) {}

    get closingRequested(): boolean {
        return this.requested;
    }

    close(): Promise<void> {
        if (this.context.state.phase === "closed") return Promise.resolve();
        if (this.closePromise) return this.closePromise;
        this.requested = true;
        this.context.state.beginClose();
        this.context.abortStartups();
        this.context.failInputs(
            (requestId) =>
                new ChatClosedError(
                    requestId,
                    "Chat closed before input delivery",
                ),
        );
        const attempt = this.finalizeClose();
        this.closePromise = attempt;
        void attempt.then(
            () => {
                this.closePromise = undefined;
            },
            () => {
                this.closePromise = undefined;
            },
        );
        return attempt;
    }

    private async finalizeClose(): Promise<void> {
        try {
            await withDeadline(
                this.context.mutex.settled(),
                this.context.lifecycle.startupSettleTimeoutMs,
                "Chat startup settlement",
            );
        } catch (error) {
            this.context.state.cleanupPending();
            throw new ChatCloseIncompleteError([error]);
        }

        const failures = await closeChatResources(
            {
                takeActive: () => this.context.takeActive(),
                pendingCleanups: this.context.pendingCleanups,
                ownedSessions: this.context.ownedSessions,
                releaseOwnership: (sessionId) =>
                    this.context.releaseOwnership(sessionId),
                releaseIndex: (sessionId) =>
                    this.context.releaseIndex(sessionId),
                onActivationClosed: (activation) =>
                    this.context.onActivationClosed(activation),
            },
            {
                activationCloseTimeoutMs:
                    this.context.lifecycle.activationCloseTimeoutMs,
                releaseOwnership: true,
            },
        );

        if (failures.length > 0) {
            this.context.state.cleanupPending();
            throw new ChatCloseIncompleteError(failures);
        }

        this.context.facts.close();
        this.context.state.closeConfirmed();
        this.context.onClosed(this.context.id);
    }
}
