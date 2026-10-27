import type { ChatLifecycleOptions, ChatRuntime } from "./types";
import { DEFAULT_WASM_RUNTIME_CLEANUP_TIMEOUT_MS } from "../wasm/budget";

/** 关闭/启动预算的单一权威默认值；调用方只按需覆盖，不各自发明超时。 */
const DEFAULT_TRANSPORT_LIFECYCLE: Required<ChatLifecycleOptions> =
    Object.freeze({
        closeDrainTimeoutMs: 3000,
        startupSettleTimeoutMs: 5000,
        activationCloseTimeoutMs: 5000,
        shutdownChatTimeoutMs: 15000,
    });

/**
 * WASM 激活包含 native 运行后端清理；关闭预算在清理预算上留出结算余量，
 * 避免把正常清理误报为 Incomplete。scheduler 与清理超时由运行后端自持。
 */
const DEFAULT_WASM_LIFECYCLE: Required<ChatLifecycleOptions> = Object.freeze({
    ...DEFAULT_TRANSPORT_LIFECYCLE,
    activationCloseTimeoutMs: DEFAULT_WASM_RUNTIME_CLEANUP_TIMEOUT_MS + 5000,
    shutdownChatTimeoutMs: DEFAULT_WASM_RUNTIME_CLEANUP_TIMEOUT_MS + 10_000,
});

/** 归一化生命周期预算；非法值直接拒绝，避免关闭退化为无界等待。 */
export function resolveChatLifecycle(
    runtimeKind: ChatRuntime["kind"],
    options?: ChatLifecycleOptions,
): Required<ChatLifecycleOptions> {
    const defaults = runtimeKind === "wasm"
        ? DEFAULT_WASM_LIFECYCLE
        : DEFAULT_TRANSPORT_LIFECYCLE;
    const resolved = { ...defaults, ...options };
    for (const [name, value] of Object.entries(resolved)) {
        if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
            throw new TypeError(
                `ChatLifecycleOptions.${name} must be a finite non-negative number`,
            );
        }
    }
    return resolved;
}
