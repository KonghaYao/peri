import type { ChatBindingTarget } from "../src/chat/types";
import type {
    ChatFact,
    ChatFactEnvelope,
    ChatFactObserver,
    ChatFactSource,
} from "../src/chat/facts";
import type { ChatYjsChannel } from "../src/view/channel";

/** `session/update` 通知帧；`sessionId` 必须与当前绑定一致。 */
export function update(sessionId: string, kind: string, fields: Record<string, unknown>) {
    return { jsonrpc: "2.0" as const, method: "session/update", params: {
        sessionId, update: { sessionUpdate: kind, ...fields },
    } };
}

export class TestChannel implements ChatYjsChannel {
    peer: TestChannel | null = null;
    closed = false;
    closeReason: Error | undefined;
    private readonly messages = new Set<(frame: Uint8Array) => void>();
    private readonly closes = new Set<(error?: Error) => void>();

    send(frame: Uint8Array): void {
        if (this.closed) throw new Error("channel is closed");
        const peer = this.peer;
        if (!peer || peer.closed) throw new Error("peer is closed");
        for (const listener of [...peer.messages]) listener(new Uint8Array(frame));
    }

    onMessage(listener: (frame: Uint8Array) => void): () => void {
        this.messages.add(listener);
        return () => this.messages.delete(listener);
    }

    onClose(listener: (error?: Error) => void): () => void {
        this.closes.add(listener);
        return () => this.closes.delete(listener);
    }

    close(reason?: Error): void {
        if (this.closed) return;
        this.closed = true;
        this.closeReason = reason;
        for (const listener of [...this.closes]) listener(reason);
        const peer = this.peer;
        if (peer && !peer.closed) peer.close(reason);
    }
}

export function channelPair(): { server: TestChannel; client: TestChannel } {
    const server = new TestChannel();
    const client = new TestChannel();
    server.peer = client;
    client.peer = server;
    return { server, client };
}

/** 只握手、收帧、不 ACK 的慢客户端。 */
export class MuteChannel implements ChatYjsChannel {
    closed = false;
    closeReason: Error | undefined;
    frames: Uint8Array[] = [];
    private readonly messages = new Set<(frame: Uint8Array) => void>();
    private readonly closes = new Set<(error?: Error) => void>();

    send(frame: Uint8Array): void {
        if (this.closed) throw new Error("channel is closed");
        this.frames.push(new Uint8Array(frame));
    }
    onMessage(listener: (frame: Uint8Array) => void): () => void {
        this.messages.add(listener);
        return () => this.messages.delete(listener);
    }
    onClose(listener: (error?: Error) => void): () => void {
        this.closes.add(listener);
        return () => this.closes.delete(listener);
    }
    close(reason?: Error): void {
        if (this.closed) return;
        this.closed = true;
        this.closeReason = reason;
        for (const listener of [...this.closes]) listener(reason);
    }
    hello(): void {
        const frame = new Uint8Array([0x01, 0x02, 0x00]);
        for (const listener of [...this.messages]) listener(frame);
    }
}

/** 内存事实源：绑定换代重置序号与事实日志，观察者同步收到通知。 */
export class FakeFactSource implements ChatFactSource {
    binding: ChatBindingTarget | null = null;
    private sequence = 0;
    private log: ChatFactEnvelope[] = [];
    private readonly observers = new Set<ChatFactObserver>();

    subscribeFacts(observer: ChatFactObserver): () => void {
        this.observers.add(observer);
        if (this.binding) observer.onBinding(this.binding, this.log);
        return () => this.observers.delete(observer);
    }

    bind(target: ChatBindingTarget): void {
        this.binding = { ...target };
        this.sequence = 0;
        this.log = [];
        for (const observer of [...this.observers]) observer.onBinding(this.binding, this.log);
    }

    emit(fact: ChatFact): ChatFactEnvelope {
        if (!this.binding) throw new Error("source has no binding");
        const envelope: ChatFactEnvelope = { ...this.binding, sequence: ++this.sequence, fact };
        this.log.push(envelope);
        for (const observer of [...this.observers]) observer.onFact(envelope);
        return envelope;
    }

    close(reason?: Error): void {
        for (const observer of [...this.observers]) observer.onClosed(reason);
    }

    get observerCount(): number { return this.observers.size; }
}

export async function waitFor(check: () => boolean, timeoutMs = 1500): Promise<void> {
    const started = Date.now();
    while (!check()) {
        if (Date.now() - started > timeoutMs) throw new Error("condition timed out");
        await new Promise((resolve) => setTimeout(resolve, 5));
    }
}
