import {
    PeriInstance,
    type PeriHostActivation,
    type PeriHostActivator,
} from "../agent/peri-instance";
import type { Transport } from "../transport/types";
import type { ChatInstanceContext } from "./service-context";

/**
 * Phase 4 装配：把 Chat 句柄的运行后端/Store/生命周期上下文转换成一次 `PeriInstance`。
 *
 * 只做装配，不实现 AgentRuntime、不持有 Y.Doc、不取得 Session ownership。工厂按激活调用；
 * WASM 运行后端租约与 transport 的关闭证据在这里统一映射为 `confirmed` 结果。
 */

/** WASM 租约关闭报告 → PeriInstance 的激活关闭证据。 */
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

function runtimeActivator(context: ChatInstanceContext): PeriHostActivator {
    return async (path, { signal }): Promise<PeriHostActivation> => {
        if (context.runtime.kind === "wasm") {
            const lease = await context.acquireWasmRuntime(signal);
            return {
                transport: lease.transport,
                close: () => closeLease(lease),
            };
        }
        const transport = await context.runtime.factory(path, { signal });
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
 * - `defaultMcpServers` 来自 Manager sessionInit，由 PeriInstance 逐激活解析；
 * - 外层 signal 只中止启动等待，生命周期预算映射到关闭选项；
 * - `agentConfig` 由 Manager 序列化为 WASM 启动配置；transport 运行后端的配置归工厂。
 */
export function createChatInstance(context: ChatInstanceContext): PeriInstance {
    return new PeriInstance({
        acquireHost: runtimeActivator(context),
        resolveSessionPath: async (sessionId) => {
            const entry = await context.database.getSession(sessionId);
            return entry?.cwd ?? null;
        },
        defaultMcpServers: context.sessionInit.mcpServers,
        signal: context.signal,
        close: {
            drainTimeoutMs: context.lifecycle.closeDrainTimeoutMs,
            bindingSettleTimeoutMs: context.lifecycle.startupSettleTimeoutMs,
            activationCloseTimeoutMs: context.lifecycle.activationCloseTimeoutMs,
        },
    });
}
