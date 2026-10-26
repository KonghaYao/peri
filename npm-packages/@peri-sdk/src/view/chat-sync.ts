import {
    acknowledgeDelivery, decodeAckFrame, decodeHelloFrame, FRAME_ACK, FRAME_HELLO,
    reserveDeliveryFrame, SessionDocSync, SyncBackpressureError,
    type DeliveryCreditPolicy, type DeliveryLedger, type DocSnapshot, type DocUpdate, type HelloFrame,
} from "../sync/index";
import { withDeadline } from "../util/deadline";
import type { ChatYjsChannel } from "./channel";

/** 延迟发送超出确认信用或截止时间；关闭该连接，不拖住其他客户端。 */
export class ChatSyncTimeoutError extends Error {
    constructor(message = "Chat sync delivery acknowledgement timed out") {
        super(message);
        this.name = "ChatSyncTimeoutError";
    }
}

/** 客户端发来非 HELLO/ACK 帧，或重复握手。 */
class ChatSyncProtocolError extends Error {
    constructor(message: string) {
        super(message);
        this.name = "ChatSyncProtocolError";
    }
}

export type ChatSyncOptions = {
    /** 主动 close 时等待 peer 确认已发送帧的上限。 */
    closeTimeoutMs?: number;
    /** 单连接未确认帧数/字节上限。 */
    maxOutstandingFrames?: number;
    maxOutstandingBytes?: number;
    /** 未确认帧的最长等待时间。 */
    ackTimeoutMs?: number;
    now?: () => number;
    /** 连接终止（失败或主动关闭）后的通知；用于从 ChatView 的连接表移除。 */
    onTerminated?: () => void;
};

type Payload = { type: "snapshot"; snapshot: DocSnapshot } | { type: "update"; update: DocUpdate };

const DEFAULTS = {
    closeTimeoutMs: 5_000,
    maxOutstandingFrames: 64,
    maxOutstandingBytes: 4 * 1024 * 1024,
    ackTimeoutMs: 10_000,
} as const;

function positive(value: number, name: string): number {
    if (!Number.isSafeInteger(value) || value <= 0) throw new RangeError(`${name} must be a positive safe integer`);
    return value;
}

/**
 * 单条 ChatYjsChannel 的服务端。等待 HELLO 后才发送 SNAPSHOT，之后发送 UPDATE；
 * 每个连接独立 DeliveryLedger，ACK 驱动信用。慢客户端触发显式关闭。
 */
export class ChatSyncConnection {
    private readonly channel: ChatYjsChannel;
    private readonly policy: DeliveryCreditPolicy;
    private readonly closeTimeoutMs: number;
    private readonly ledger: DeliveryLedger = { nextDelivery: 1, acknowledged: 0, pending: [] };
    private hello: HelloFrame | null = null;
    private source: SessionDocSync | null = null;
    private unsubscribeSource: (() => void) | null = null;
    private detachMessage: () => void;
    private detachClose: () => void;
    private closed = false;
    private failure: Error | undefined;
    private closePromise: Promise<void> | null = null;
    private channelCloseDone = false;
    private timer: ReturnType<typeof setTimeout> | undefined;
    private readonly waiters = new Set<() => void>();
    private readonly onTerminated: (() => void) | undefined;

    constructor(
        private readonly getSource: () => SessionDocSync | null,
        channel: ChatYjsChannel,
        options: ChatSyncOptions = {},
    ) {
        this.channel = channel;
        this.onTerminated = options.onTerminated;
        this.closeTimeoutMs = positive(options.closeTimeoutMs ?? DEFAULTS.closeTimeoutMs, "closeTimeoutMs");
        this.policy = {
            maxFrames: positive(options.maxOutstandingFrames ?? DEFAULTS.maxOutstandingFrames, "maxOutstandingFrames"),
            maxBytes: positive(options.maxOutstandingBytes ?? DEFAULTS.maxOutstandingBytes, "maxOutstandingBytes"),
            timeoutMs: positive(options.ackTimeoutMs ?? DEFAULTS.ackTimeoutMs, "ackTimeoutMs"),
            now: options.now,
        };
        this.detachMessage = channel.onMessage((frame) => this.receive(frame));
        this.detachClose = channel.onClose((error) => this.terminate(
            error ?? new Error("Chat sync channel closed before acknowledgement"), false));
        if (channel.closed) {
            this.terminate(new Error("Chat sync channel is already closed"), false);
            return;
        }
        this.replaceSource(getSource());
    }

    /** 文档换代：退订旧代次，等 HELLO 已到达时对当前连接发新代次的 full SNAPSHOT。 */
    replaceSource(source: SessionDocSync | null): void {
        if (this.closed) return;
        this.unsubscribeSource?.();
        this.unsubscribeSource = null;
        this.source = null;
        if (!source || !this.hello) return;
        this.source = source;
        const connection = source.subscribe((update) => this.send({ type: "update", update }), {
            resume: this.hello.resume,
            onError: (error) => this.terminate(error, true),
        });
        this.unsubscribeSource = connection.unsubscribe;
        this.send({ type: "snapshot", snapshot: connection.snapshot });
    }

    flush(): void { this.source?.flush(); }

    /**
     * 关闭连接并关闭底层 channel。
     *
     * 清理只执行一次；channel.close 失败时保留可重试状态，后续 close 只重试
     * 未完成的 channel 关闭，不会让 rejected 结果永久缓存。
     */
    close(): Promise<void> {
        if (this.closePromise) return this.closePromise;
        const attempt = this.finishClose();
        this.closePromise = attempt;
        void attempt.catch(() => {
            if (this.closePromise === attempt) this.closePromise = null;
        });
        return attempt;
    }

    private async finishClose(): Promise<void> {
        if (!this.closed) {
            this.source?.flush();
            if (!this.closed) {
                this.unsubscribeSource?.();
                this.unsubscribeSource = null;
                if (this.ledger.pending.length) {
                    try {
                        await withDeadline(this.drained(), this.closeTimeoutMs, "Chat sync close");
                    } catch (error) {
                        this.failure ??= error instanceof Error ? error : new Error("Chat sync close failed");
                    }
                }
                this.cleanup();
                this.onTerminated?.();
            }
        }
        if (this.channelCloseDone || this.channel.closed) {
            this.channelCloseDone = true;
            return;
        }
        await this.channel.close(this.failure);
        this.channelCloseDone = true;
    }

    private receive(raw: Uint8Array): void {
        if (this.closed) return;
        try {
            this.assertDeadlines();
            if (raw.byteLength === 0) throw new ChatSyncProtocolError("Empty chat sync frame");
            const tag = raw[0];
            if (tag === FRAME_HELLO) {
                if (this.hello) throw new ChatSyncProtocolError("Duplicate chat sync hello");
                this.hello = decodeHelloFrame(raw);
                this.replaceSource(this.getSource());
                return;
            }
            if (tag === FRAME_ACK) {
                acknowledgeDelivery(this.ledger, decodeAckFrame(raw).delivery);
                if (!this.ledger.pending.length) this.resolveDrained();
                this.scheduleDeadline();
                return;
            }
            throw new ChatSyncProtocolError(`Unexpected chat sync frame tag ${String(tag)}`);
        } catch (error) {
            this.terminate(error instanceof Error ? error : new Error("Invalid chat sync frame"), true);
        }
    }

    private send(payload: Payload): void {
        if (this.closed) return;
        let encoded: Uint8Array;
        try {
            encoded = reserveDeliveryFrame(this.ledger, payload, this.policy);
        } catch (error) {
            this.terminate(error instanceof SyncBackpressureError
                ? error
                : new SyncBackpressureError(), true);
            return;
        }
        this.scheduleDeadline();
        try {
            const pending = this.channel.send(encoded);
            if (pending) void Promise.resolve(pending).catch((error: unknown) => {
                this.terminate(error instanceof Error ? error : new Error("Chat sync send failed"), true);
            });
        } catch (error) {
            this.terminate(error instanceof Error ? error : new Error("Chat sync send failed"), true);
        }
    }

    private scheduleDeadline(): void {
        if (this.timer) { clearTimeout(this.timer); this.timer = undefined; }
        if (this.closed || !this.ledger.pending.length) return;
        const now = this.policy.now?.() ?? Date.now();
        const deadline = this.ledger.pending[0]!.deadline;
        this.timer = setTimeout(() => {
            this.timer = undefined;
            if (this.closed) return;
            if ((this.policy.now?.() ?? Date.now()) >= deadline) {
                this.terminate(new ChatSyncTimeoutError(), true);
                return;
            }
            this.scheduleDeadline();
        }, Math.max(0, deadline - now));
        // 不阻止进程退出；连接关闭时总会清理。
        (this.timer as { unref?: () => void }).unref?.();
    }

    private assertDeadlines(): void {
        const pending = this.ledger.pending[0];
        if (pending && (this.policy.now?.() ?? Date.now()) >= pending.deadline) {
            throw new ChatSyncTimeoutError();
        }
    }

    private drained(): Promise<void> {
        if (!this.ledger.pending.length || this.closed) return Promise.resolve();
        return new Promise((resolve) => this.waiters.add(resolve));
    }

    private resolveDrained(): void {
        for (const resolve of this.waiters) resolve();
        this.waiters.clear();
    }

    private terminate(error: Error, closeChannel: boolean): void {
        if (this.closed) return;
        this.cleanup();
        this.failure = error;
        if (closeChannel && !this.channel.closed) {
            void Promise.resolve(this.channel.close(error)).catch(() => {});
        }
        this.onTerminated?.();
    }

    private cleanup(): void {
        if (this.closed) return;
        this.closed = true;
        if (this.timer) { clearTimeout(this.timer); this.timer = undefined; }
        this.unsubscribeSource?.();
        this.unsubscribeSource = null;
        this.source = null;
        this.detachMessage();
        this.detachClose();
        this.resolveDrained();
    }
}
