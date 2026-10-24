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
export interface ChatCloseHost {
    readonly id: string;
    readonly mutex: AsyncMutex;
    readonly state: ChatBinding;
    readonly facts: ChatFactHub;
    readonly lifecycle: Required<ChatLifecycleOptions>;
    readonly pendingCleanups: Set<Activation>;
    readonly claimedSessions: Set<string>;
    takeActive(): Activation | undefined;
    releaseClaim(sessionId: string): Promise<void>;
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
 * 超时一律是 Incomplete：保留 claim/索引/未确认激活供再次 close/shutdown 重试。
 */
export class ChatCloseController {
    private closePromise?: Promise<void>;
    private requested = false;

    constructor(private readonly host: ChatCloseHost) {}

    get closingRequested(): boolean {
        return this.requested;
    }

    close(): Promise<void> {
        if (this.host.state.phase === "closed") return Promise.resolve();
        if (this.closePromise) return this.closePromise;
        this.requested = true;
        this.host.state.beginClose();
        this.host.abortStartups();
        this.host.failInputs(
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
                this.host.mutex.settled(),
                this.host.lifecycle.startupSettleTimeoutMs,
                "Chat startup settlement",
            );
        } catch (error) {
            this.host.state.cleanupPending();
            throw new ChatCloseIncompleteError([error]);
        }

        const failures = await closeChatResources(
            {
                takeActive: () => this.host.takeActive(),
                pendingCleanups: this.host.pendingCleanups,
                claimedSessions: this.host.claimedSessions,
                releaseClaim: (sessionId) =>
                    this.host.releaseClaim(sessionId),
                releaseIndex: (sessionId) =>
                    this.host.releaseIndex(sessionId),
                onActivationClosed: (activation) =>
                    this.host.onActivationClosed(activation),
            },
            {
                activationCloseTimeoutMs:
                    this.host.lifecycle.activationCloseTimeoutMs,
                releaseClaims: true,
            },
        );

        if (failures.length > 0) {
            this.host.state.cleanupPending();
            throw new ChatCloseIncompleteError(failures);
        }

        this.host.facts.close();
        this.host.state.closeConfirmed();
        this.host.onClosed(this.host.id);
    }
}
