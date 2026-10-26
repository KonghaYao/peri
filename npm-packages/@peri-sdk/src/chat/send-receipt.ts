import type { ChatBindingTarget, SendReceipt } from "./types";

interface ChatSendReceiptOptions {
    readonly requestId: string;
    readonly inputId: string;
    /** 调用 execute 时固定；之后的迟到结果只归该目标。 */
    readonly target: ChatBindingTarget;
    /** 由 PeriInstance 在 send 之前同步登记的投递 waiter。 */
    readonly delivery: Promise<void>;
    /** 真正提交 enqueue；构造函数把它放到微任务执行。 */
    readonly send: () => Promise<void>;
}

interface Deferred<T> {
    readonly promise: Promise<T>;
    resolve(value: T): void;
    reject(error: unknown): void;
}

function deferred<T>(): Deferred<T> {
    let resolve!: (value: T) => void;
    let reject!: (error: unknown) => void;
    const promise = new Promise<T>((onResolve, onReject) => {
        resolve = onResolve;
        reject = onReject;
    });
    return { promise, resolve, reject };
}

/**
 * 同步返回的回执。
 *
 * - `accepted`：enqueue 命令被接受（或失败）；不等于已投递。
 * - `then`：等待 `user_input_delivered` 事实或 PeriInstance waiter；`isSent` 只在投递后为真。
 * - 撤回/关闭/确定失败通过 `reject` 结算所有等待者；已 resolved 的 `accepted` 不回滚。
 */
export class ChatSendReceipt implements SendReceipt {
    readonly requestId: string;
    readonly inputId: string;
    readonly target: ChatBindingTarget;
    readonly accepted: Promise<void>;

    private readonly delivery: Promise<void>;
    private readonly deliveryGate = deferred<void>();
    private sent = false;
    private settled = false;

    constructor(options: ChatSendReceiptOptions) {
        this.requestId = options.requestId;
        this.inputId = options.inputId;
        this.target = options.target;
        // 同步校验/登记已完成；异步提交放在微任务，避免 PromiseLike 被自动 await。
        this.accepted = Promise.resolve().then(options.send);
        this.delivery = Promise.race([
            options.delivery,
            this.deliveryGate.promise,
        ]).then(
            () => {
                this.sent = true;
                this.settled = true;
            },
            (error: unknown) => {
                this.settled = true;
                throw error;
            },
        );
        // enqueue 失败时也结算投递等待者；已有投递结果时不回滚。
        void this.accepted.catch((error: unknown) => this.reject(error));
        // 调用方可能只 await accepted；投递失败不能变成未处理拒绝。
        void this.delivery.catch(() => {});
    }

    get isSent(): boolean {
        return this.sent;
    }

    /** 事实 `input-delivered` 与 requestId/目标匹配时由 ChatService 调用。 */
    markDelivered(): void {
        if (!this.settled) this.deliveryGate.resolve();
    }

    /** 撤回成功、关闭或确定失败时由 ChatService 调用。 */
    reject(error: unknown): void {
        if (!this.settled) this.deliveryGate.reject(error);
    }

    then<TResult1 = void, TResult2 = never>(
        onfulfilled?:
            | ((value: void) => TResult1 | PromiseLike<TResult1>)
            | null,
        onrejected?:
            | ((reason: unknown) => TResult2 | PromiseLike<TResult2>)
            | null,
    ): Promise<TResult1 | TResult2> {
        return this.delivery.then(onfulfilled, onrejected);
    }
}

interface CorrelatedInput {
    readonly receipt: ChatSendReceipt;
    readonly target: ChatBindingTarget;
}

/**
 * 单个 Chat 的 inputId → 回执关联。
 *
 * 回执按 inputId 结算，不按“当前绑定”重新定位；迟到/旧绑定的投递事实
 * 只可能命中当初登记的同一条输入。
 */
export class InputCorrelation {
    private readonly pending = new Map<string, CorrelatedInput>();

    register(inputId: string, input: CorrelatedInput): void {
        this.pending.set(inputId, input);
    }

    get size(): number {
        return this.pending.size;
    }

    markDelivered(inputId: string): void {
        this.pending.get(inputId)?.receipt.markDelivered();
    }

    settle(inputId: string): void {
        this.pending.delete(inputId);
    }

    /** 取走并移除；调用方负责结算该回执。 */
    take(inputId: string): CorrelatedInput | undefined {
        const input = this.pending.get(inputId);
        if (input) this.pending.delete(inputId);
        return input;
    }

    rejectAll(makeError: (requestId: string) => unknown): void {
        for (const { receipt } of [...this.pending.values()]) {
            receipt.reject(makeError(receipt.requestId));
        }
        this.pending.clear();
    }
}
