import type { ChatLifecycleOptions } from "./types";

/** 关闭/启动预算的单一权威默认值；调用方只按需覆盖，不各自发明超时。 */
const DEFAULT_LIFECYCLE: Required<ChatLifecycleOptions> = Object.freeze({
    closeDrainTimeoutMs: 3000,
    startupSettleTimeoutMs: 5000,
    activationCloseTimeoutMs: 5000,
    shutdownChatTimeoutMs: 15000,
});

/** 非法预算直接拒绝，避免关闭退化为无界等待。 */
function validateLifecycle(
    lifecycle: Required<ChatLifecycleOptions>,
): Required<ChatLifecycleOptions> {
    for (const [name, value] of Object.entries(lifecycle)) {
        if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
            throw new TypeError(
                `ChatLifecycleOptions.${name} must be a finite non-negative number`,
            );
        }
    }
    return lifecycle;
}

/** 归一化生命周期预算；调用方未给出的字段取默认值。 */
export function resolveChatLifecycle(
    options?: ChatLifecycleOptions,
): Required<ChatLifecycleOptions> {
    return validateLifecycle({ ...DEFAULT_LIFECYCLE, ...options });
}

/**
 * 合并 per-chat 生命周期覆盖。
 *
 * `shutdownChatTimeoutMs` 是 Manager 聚合等待预算，保持 Manager 值：
 * 单个 Chat 的覆盖不能改变 `ChatManager.shutdown()` 的整体上界。
 */
export function mergeChatLifecycle(
    base: Required<ChatLifecycleOptions>,
    overrides?: ChatLifecycleOptions,
): Required<ChatLifecycleOptions> {
    if (overrides === undefined) return base;
    return resolveChatLifecycle({
        ...base,
        ...overrides,
        shutdownChatTimeoutMs: base.shutdownChatTimeoutMs,
    });
}
