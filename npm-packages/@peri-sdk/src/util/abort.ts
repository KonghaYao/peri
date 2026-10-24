/** 取消与有界等待的纯工具；不引入定时器语义以外的副作用。 */
export { DeadlineError, withDeadline } from "./deadline";

/** 等待被调用方取消；`cause` 保留调用方给出的取消原因。 */
export class AbortRaceError extends Error {
    constructor(readonly operation: string, options?: { cause?: unknown }) {
        super(`${operation} was aborted`, options);
        this.name = "AbortRaceError";
    }
}

export function throwIfAborted(signal: AbortSignal | undefined, operation: string): void {
    if (signal?.aborted) throw new AbortRaceError(operation, { cause: signal.reason });
}

/**
 * 只中止等待，不取消底层操作。
 *
 * 底层 promise 迟到结算时其错误由 race 的处理器消费；调用方若需要释放迟到资源，
 * 应自行保留底层 promise 并在结算后清理。
 */
export function raceAbort<T>(
    operation: Promise<T>,
    signal: AbortSignal | undefined,
    label: string,
): Promise<T> {
    if (!signal) return operation;
    if (signal.aborted) return Promise.reject(new AbortRaceError(label, { cause: signal.reason }));
    return new Promise<T>((resolve, reject) => {
        const onAbort = (): void => {
            reject(new AbortRaceError(label, { cause: signal.reason }));
        };
        signal.addEventListener("abort", onAbort, { once: true });
        operation.then(
            (value) => {
                signal.removeEventListener("abort", onAbort);
                resolve(value);
            },
            (error) => {
                signal.removeEventListener("abort", onAbort);
                reject(error);
            },
        );
    });
}
