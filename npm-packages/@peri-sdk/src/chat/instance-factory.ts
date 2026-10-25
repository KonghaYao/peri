import {
    PeriInstance,
    type PeriHostActivation,
    type PeriHostActivator,
    type PeriInstanceOptions,
} from "../agent/peri-instance";
import type { Transport } from "../transport/types";
import type { ChatInstanceContext, ChatInstanceFactory } from "./service-support";
import type { ChatLifecycleOptions } from "./types";

/**
 * Phase 4 装配：把 Chat 句柄的宿主/Store/生命周期上下文转换成一次 `PeriInstance`。
 *
 * 只做装配，不实现 AgentRuntime、不持有 Y.Doc、不 claim Session。工厂按激活调用；
 * WASM 宿主租约与 transport 的关闭证据在这里统一映射为 `confirmed` 结果。
 */

/** WASM 租约关闭报告 → PeriInstance 的宿主关闭证据。 */
async function closeLease(lease: {
    close(): Promise<{ confirmed: boolean; error?: unknown }>;
}): Promise<{ confirmed: boolean; error?: unknown }> {
    const report = await lease.close();
    return report.confirmed
        ? { confirmed: true }
        : { confirmed: false, error: report.error };
}

/** transport 关闭成功即确认；失败保留原因，由 PeriInstance 包装为清理错误。 */
async function closeTransport(
    transport: Transport,
): Promise<{ confirmed: boolean; error?: unknown }> {
    try {
        await transport.close();
        return { confirmed: true };
    } catch (error) {
        return { confirmed: false, error };
    }
}

function hostActivator(context: ChatInstanceContext): PeriHostActivator {
    return async (path, { signal }): Promise<PeriHostActivation> => {
        if (context.host.kind === "wasm") {
            const lease = await context.acquireWasmHost(signal);
            return {
                transport: lease.transport,
                close: () => closeLease(lease),
            };
        }
        const transport = await context.host.factory(path, { signal });
        return {
            transport,
            close: () => closeTransport(transport),
        };
    };
}

/**
 * 单次激活装配。
 *
 * - `resolveSessionPath` 只读数据库中的持久 cwd，不回退句柄 path；
 * - `defaultMcpServers` 来自 Manager defaults，由 PeriInstance 逐激活解析；
 * - 外层 signal 只中止启动等待，生命周期预算映射到关闭选项；
 * - `agentConfig` 由 Manager 序列化为 WASM 启动配置；transport 宿主的配置归工厂。
 */
export function createChatInstance(context: ChatInstanceContext): PeriInstance {
    return new PeriInstance({
        acquireHost: hostActivator(context),
        resolveSessionPath: async (sessionId) => {
            const entry = await context.database.getSession(sessionId);
            return entry?.cwd ?? null;
        },
        defaultMcpServers: context.defaults.mcpServers,
        signal: context.signal,
        close: {
            drainTimeoutMs: context.lifecycle.closeDrainTimeoutMs,
            bindingSettleTimeoutMs: context.lifecycle.startupSettleTimeoutMs,
            activationCloseTimeoutMs: context.lifecycle.activationCloseTimeoutMs,
        },
    });
}

/**
 * 默认工厂：绑定 Manager 的生命周期预算，每次激活调用 `createChatInstance`。
 *
 * Manager 默认使用它，测试仍可通过 `ChatManagerInternalOptions.instanceFactory` 覆盖。
 */
export class DefaultChatInstanceFactory {
    readonly create: ChatInstanceFactory;

    constructor(private readonly lifecycle: Required<ChatLifecycleOptions>) {
        this.create = (context) =>
            createChatInstance({ ...context, lifecycle: this.lifecycle });
    }
}

/** 计划 §1.7 的工厂构造入口；Manager 默认装配使用。 */
export function createPeriInstanceFactory(
    lifecycle: Required<ChatLifecycleOptions>,
): ChatInstanceFactory {
    return new DefaultChatInstanceFactory(lifecycle).create;
}
