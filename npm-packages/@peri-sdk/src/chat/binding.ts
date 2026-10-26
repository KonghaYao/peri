import type { ChatBindingTarget } from "./types";

/**
 * ChatService 的绑定状态。
 *
 * - `empty`：尚无绑定；只允许 create_new_session 或 change_session。
 * - `binding` / `switching`：正在加载候选；禁止发送和新绑定动作。
 * - `ready`：唯一可发送/可换绑的状态（换绑前还要通过空闲判定）。
 * - `cleanup-pending` / `blocked`：资源清理未确认或旧运行已失效；禁止新执行。
 * - `closing` / `closed`：关闭中/已确认关闭。
 */
type ChatBindingPhase =
    | "empty"
    | "binding"
    | "ready"
    | "switching"
    | "cleanup-pending"
    | "closing"
    | "closed"
    | "blocked";

/** 绑定代次只表达“当前提交的是哪一次绑定”，不是 Peri turn/attempt。 */
export class ChatBinding {
    private phaseValue: ChatBindingPhase = "empty";
    private targetValue: ChatBindingTarget | null = null;
    private revisionValue = 0;

    constructor(private readonly chatId: string) {}

    get phase(): ChatBindingPhase {
        return this.phaseValue;
    }

    get binding(): ChatBindingTarget | null {
        return this.targetValue;
    }

    get sessionId(): string | null {
        return this.targetValue?.sessionId ?? null;
    }

    get bindingRevision(): number {
        return this.revisionValue;
    }

    beginCreate(): void {
        if (this.phaseValue !== "empty") {
            throw new Error(
                `Cannot create a session from binding phase ${this.phaseValue}`,
            );
        }
        this.phaseValue = "binding";
    }

    beginSwitch(): void {
        if (this.phaseValue !== "empty" && this.phaseValue !== "ready") {
            throw new Error(
                `Cannot change session from binding phase ${this.phaseValue}`,
            );
        }
        this.phaseValue = "switching";
    }

    /** 候选加载失败且资源清理已确认：恢复旧绑定状态。 */
    restore(previous: "empty" | "ready"): void {
        this.phaseValue = previous;
    }

    /** 只有成功提交新绑定才递增代次。 */
    commit(sessionId: string): ChatBindingTarget {
        if (sessionId.length === 0)
            throw new TypeError("Session id must not be empty");
        this.revisionValue += 1;
        this.targetValue = Object.freeze({
            chatId: this.chatId,
            sessionId,
            bindingRevision: this.revisionValue,
        });
        this.phaseValue = "ready";
        return this.targetValue;
    }

    beginClose(): void {
        if (this.phaseValue !== "closed") this.phaseValue = "closing";
    }

    /** 关闭或换绑清理未确认；保留 target 供诊断与 findChatBySession。 */
    cleanupPending(): void {
        if (this.phaseValue !== "closed") this.phaseValue = "cleanup-pending";
    }

    /** 旧运行资源已不可继续使用；保留 target 但不允许新执行。 */
    block(): void {
        if (this.phaseValue !== "closed") this.phaseValue = "blocked";
    }

    closeConfirmed(): void {
        this.phaseValue = "closed";
    }
}

interface ChatIdleFacts {
    running: boolean;
    pendingInputs: number;
    pendingInteractions: number;
    /** 已同步登记、尚未拿到 enqueue 结果的发送。 */
    pendingAdmissions: number;
    needsReload: boolean;
}

/** 换绑前的空闲判定；调用方必须在同一同步临界区内先检查再改状态。 */
export function isChatIdle(facts: ChatIdleFacts): boolean {
    return (
        !facts.running &&
        facts.pendingInputs === 0 &&
        facts.pendingInteractions === 0 &&
        facts.pendingAdmissions === 0 &&
        !facts.needsReload
    );
}

/**
 * 串行异步临界区。
 *
 * 任务按提交顺序执行；前一个任务失败不会阻断后续任务。
 */
export class AsyncMutex {
    private tail: Promise<void> = Promise.resolve();

    run<T>(task: () => Promise<T>): Promise<T> {
        const result = this.tail.then(task, task);
        this.tail = result.then(
            () => undefined,
            () => undefined,
        );
        return result;
    }

    /**
     * 等待调用时已排队的任务全部结算（失败不传播）。
     *
     * 关闭用它给在途绑定一个有界出口；调用后新入队的任务不会延长该等待。
     */
    settled(): Promise<void> {
        return this.tail;
    }
}
