import type { ChatLifecycleOptions } from "./types";

/** 关闭/启动预算的单一权威默认值；调用方只按需覆盖，不各自发明超时。 */
const DEFAULT_CHAT_LIFECYCLE: Required<ChatLifecycleOptions> =
    Object.freeze({
        closeDrainTimeoutMs: 3000,
        startupSettleTimeoutMs: 5000,
        activationCloseTimeoutMs: 5000,
        shutdownChatTimeoutMs: 15000,
    });

/** 归一化生命周期预算；非法值直接拒绝，避免关闭退化为无界等待。 */
export function resolveChatLifecycle(
    options?: ChatLifecycleOptions,
): Required<ChatLifecycleOptions> {
    const resolved = { ...DEFAULT_CHAT_LIFECYCLE, ...options };
    for (const [name, value] of Object.entries(resolved)) {
        if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
            throw new TypeError(
                `ChatLifecycleOptions.${name} must be a finite non-negative number`,
            );
        }
    }
    return resolved;
}
