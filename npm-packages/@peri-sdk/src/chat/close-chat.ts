import { withDeadline } from "../util/deadline";
import type { Activation, CleanupOutcome } from "./service-support";

/**
 * 关闭单个激活资源；只有 port.close 在预算内结算成功才算 confirmed。
 *
 * 超时不是“已停止”的证据：保留原激活，返回失败原因供调用方聚合和重试。
 */
export async function closeActivation(
    activation: Activation,
    timeoutMs: number,
): Promise<CleanupOutcome> {
    activation.abandoned = true;
    try {
        activation.detach();
    } catch {
        // detach 是尽力而为；关闭结果才是所有权证据。
    }
    try {
        await withDeadline(
            activation.port.close(),
            timeoutMs,
            "Chat activation close",
        );
        return { confirmed: true };
    } catch (error) {
        return { confirmed: false, error };
    }
}

interface ChatCloseResources {
    /** 取走当前激活；实现方必须同步清空，避免关闭期间仍收事实。 */
    takeActive(): Activation | undefined;
    readonly pendingCleanups: Set<Activation>;
    readonly ownedSessions: Set<string>;
    releaseOwnership(sessionId: string): Promise<void>;
    releaseIndex(sessionId: string): void;
    onActivationClosed(activation: Activation): void;
}

interface ChatCloseOptions {
    /** 单个激活（运行后端/transport）关闭等待；超时即 Incomplete。 */
    readonly activationCloseTimeoutMs: number;
    /** 只有调用方确认绑定已结算时才允许释放 ownership/索引。 */
    readonly releaseOwnership: boolean;
}

/**
 * 结算一个 Chat 的全部资源。
 *
 * 任一运行后端/端口关闭未确认时不释放 ownership，防止出现双控制者；
 * 返回全部失败项，由调用方决定聚合上报与是否允许重试。
 */
export async function closeChatResources(
    resources: ChatCloseResources,
    options: ChatCloseOptions,
): Promise<unknown[]> {
    const failures: unknown[] = [];
    const cleanups = new Set<Activation>(resources.pendingCleanups);
    const active = resources.takeActive();
    if (active) cleanups.add(active);

    for (const activation of cleanups) {
        const outcome = await closeActivation(
            activation,
            options.activationCloseTimeoutMs,
        );
        if (outcome.confirmed) {
            resources.pendingCleanups.delete(activation);
            resources.onActivationClosed(activation);
        } else {
            resources.pendingCleanups.add(activation);
            failures.push(outcome.error);
        }
    }

    if (options.releaseOwnership && failures.length === 0) {
        for (const sessionId of [...resources.ownedSessions]) {
            try {
                await resources.releaseOwnership(sessionId);
                resources.releaseIndex(sessionId);
            } catch (error) {
                failures.push(error);
            }
        }
    }
    return failures;
}
