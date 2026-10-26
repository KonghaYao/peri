import { ChatClosedError, ChatViewReplayOverflowError } from "./errors";
import type { ChatFact, ChatFactObserver } from "./facts";
import { FactLog } from "./service-support";
import type { ChatBindingTarget } from "./types";

/**
 * ChatService 的事实 fan-out 与代次日志。
 *
 * - 订阅同步登记，随后同步回放当前代次；
 * - 绑定提交时整批切换日志，并向已挂观察者发 `onBinding`；
 * - 日志溢出后新观察者被拒绝，已挂观察者继续收 live fact；
 * - 关闭只通知一次，观察者异常互相隔离。
 */
export class ChatFactHub {
    private readonly observers = new Set<ChatFactObserver>();
    private log?: FactLog;
    private target?: ChatBindingTarget;
    private closed = false;

    constructor(private readonly chatId: string) {}

    subscribe(observer: ChatFactObserver): () => void {
        if (this.closed) {
            try {
                observer.onClosed(
                    new ChatClosedError("", "Chat is already closed"),
                );
            } catch {
                // observer 故障隔离。
            }
            return () => {};
        }
        this.observers.add(observer);
        const log = this.log;
        const target = this.target;
        if (target && log) {
            if (log.overflowed) {
                this.observers.delete(observer);
                throw new ChatViewReplayOverflowError(
                    "Chat fact log overflowed; rebind before attaching a new view",
                );
            }
            try {
                observer.onBinding(target, log.replay());
            } catch {
                // 一个 View 的投影失败不能影响其他观察者或运行侧。
            }
        }
        return () => {
            this.observers.delete(observer);
        };
    }

    /** 提交新绑定：替换日志、回放初始化材料，再进入 live。 */
    bind(target: ChatBindingTarget, initialFacts: readonly ChatFact[]): void {
        const log = new FactLog(this.chatId, target);
        for (const fact of initialFacts) log.append(fact);
        this.log = log;
        this.target = target;
        if (log.overflowed) return;
        const replay = log.replay();
        for (const observer of [...this.observers]) {
            try {
                observer.onBinding(target, replay);
            } catch {
                // View 故障隔离：单个投影失败不阻断绑定提交。
            }
        }
    }

    push(fact: ChatFact): void {
        if (this.closed || !this.log) return;
        const envelope = this.log.append(fact);
        for (const observer of [...this.observers]) {
            try {
                observer.onFact(envelope);
            } catch {
                // 一个 View 的投影失败不能影响其他观察者或运行侧。
            }
        }
    }

    close(reason?: Error): void {
        if (this.closed) return;
        this.closed = true;
        for (const observer of [...this.observers]) {
            try {
                observer.onClosed(reason);
            } catch {
                // observer 故障隔离。
            }
        }
        this.observers.clear();
        this.log = undefined;
        this.target = undefined;
    }
}
