import { RpcError } from "../transport/rpc-error";

/**
 * 会话关闭的有界重试。
 *
 * 当前 ACP 的 `session/close` 只有在 CloseCoordinator 结算完成时才返回成功；仍在收尾
 * （prompt 未停、子 agent 未交接、Store 结算未知）时返回可重试的 incomplete 错误。SDK
 * 不缓存“已接受”当成功：预算内重放同一请求，超时抛出携带最后一次原因的 incomplete 错误。
 */

export type CloseOptions = { drainTimeoutMs?: number };

/** 当前 ACP 用 -32010 表达“关闭尚未完成，可重试”。 */
const INCOMPLETE_CLOSE_CODE = -32010;
const RETRY_INTERVAL_MS = 50;

export class SessionCloseIncompleteError extends Error {
    constructor(readonly detail: string, cause?: unknown) {
        super(`Session close is incomplete: ${detail}`, { cause });
        this.name = "SessionCloseIncompleteError";
    }
}

export type CloseRetryClock = {
    now: () => number;
    sleep: (milliseconds: number) => Promise<unknown>;
};

const clock: CloseRetryClock = {
    now: () => performance.now(),
    sleep: (milliseconds) => new Promise((done) => setTimeout(done, milliseconds)),
};

export function closeDrainBudget(options: CloseOptions): number {
    const milliseconds = options.drainTimeoutMs ?? 3_000;
    if (!Number.isSafeInteger(milliseconds) || milliseconds < 0)
        throw new TypeError("Close drainTimeoutMs must be a nonnegative safe integer");
    return milliseconds;
}

export async function closeUntilSettled(
    close: () => Promise<unknown>,
    options: CloseOptions = {},
    timing: CloseRetryClock = clock,
): Promise<void> {
    const budget = closeDrainBudget(options);
    const deadline = timing.now() + budget;
    let incomplete: RpcError | undefined;
    while (true) {
        try {
            await close();
            return;
        } catch (error) {
            if (!(error instanceof RpcError) || error.code !== INCOMPLETE_CLOSE_CODE) {
                if (incomplete) throw new SessionCloseIncompleteError(incomplete.message,
                    new AggregateError([incomplete, error], "Session close ended with a new failure after an incomplete attempt"));
                throw error;
            }
            const remaining = deadline - timing.now();
            if (budget === 0 || remaining <= 0)
                throw new SessionCloseIncompleteError(error.message, incomplete
                    ? new AggregateError([incomplete, error], "Session close stayed incomplete until the drain deadline") : error);
            incomplete = error;
            await timing.sleep(Math.min(RETRY_INTERVAL_MS, remaining));
        }
    }
}
