/**
 * Scheduler port for the Emscripten ACP runtime. Timers stay owned by the runtime so a closed
 * runtime cannot run callbacks afterwards.
 *
 * 定时器不是平台专属能力：Node 兼容运行时与浏览器都提供 `setTimeout`，浏览器宿主同样注入本端口
 * （WASM glue 的 `setImmediate`/`setTimeout` 均映射到它）。
 */

export interface WasmSchedulerPort {
    schedule(callback: () => void, delayMs?: number): unknown;
    cancel(timer: unknown): void;
    /** Stop accepting callbacks and drop every pending timer owned by the runtime. */
    close(): void;
}

export function createSchedulerPort(): WasmSchedulerPort {
    const timers = new Set<ReturnType<typeof setTimeout>>();
    let closed = false;
    return {
        schedule(callback: () => void, delayMs = 0): ReturnType<typeof setTimeout> {
            if (closed) throw new Error("WASM scheduler is closed");
            const timer = setTimeout(() => {
                timers.delete(timer);
                if (!closed) callback();
            }, delayMs);
            timers.add(timer);
            return timer;
        },
        cancel(timer: unknown): void {
            clearTimeout(timer as ReturnType<typeof setTimeout>);
            timers.delete(timer as ReturnType<typeof setTimeout>);
        },
        close(): void {
            closed = true;
            for (const timer of timers) clearTimeout(timer);
            timers.clear();
        },
    };
}
