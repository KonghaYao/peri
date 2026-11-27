import type { AcpMcpServer } from "../contracts/acp";
import type {
    ChatInstanceObservation,
    ChatMcpObservation,
    ChatObservation,
    ChatObservationPhase,
    McpServerObservation,
} from "../contracts/observability";
import {
    compareObservationIds,
    headerNamesOf,
    safeEndpointUrl,
} from "../util/observability";
import type { ChatSessionInit } from "./types";

/**
 * ChatService 的只读投影。
 *
 * 放在独立模块里，让 `service.ts` 只保留状态读取；相位映射与脱敏规则不进入
 * 绑定状态机。输入中的 `phase` 是内部阶段，只有本模块把它折算成公开值。
 */
export interface ChatObservationInput {
    chatId: string;
    workspace: { id: string; runtimePath: string };
    /** ChatBinding 的内部阶段；不直接对外输出。 */
    phase: string;
    sessionId: string | null;
    bindingRevision: number;
    mcpSource: "session-init" | "unknown";
    sessionInit: ChatSessionInit;
    port?: {
        running: boolean;
        pendingInputCount: number;
        pendingInteractionCount: number;
    } | null;
    /** ChatService 已登记但尚未入队的发送数。 */
    pendingInputs: number;
}

/**
 * 内部阶段 → 公开相位。
 *
 * `binding`/`switching` 是加载中的过渡态：没有已提交绑定时是 `empty`，
 * 旧绑定仍保留时是 `bound`。`closed` 归入 `closing`（都已不可继续执行），
 * 公开面用 `phaseLimited` 声明粒度损失。
 */
export function deriveChatObservationPhase(
    phase: string,
    hasBinding: boolean,
): ChatObservationPhase {
    switch (phase) {
        case "closing":
        case "cleanup-pending":
        case "closed":
            return "closing";
        case "blocked":
            return "blocked";
        case "ready":
        case "switching":
            // 换绑期间旧绑定仍保留；从 empty 发起换绑时则还没有已提交绑定。
            return hasBinding ? "bound" : "empty";
        case "binding":
        case "empty":
        default:
            return "empty";
    }
}

export function buildChatObservation(
    input: ChatObservationInput,
): ChatObservation {
    return {
        chatId: input.chatId,
        sessionId: input.sessionId,
        bindingRevision: input.bindingRevision,
        phase: deriveChatObservationPhase(
            input.phase,
            input.sessionId !== null,
        ),
        phaseLimited: true,
        workspace: {
            id: input.workspace.id,
            runtimePath: input.workspace.runtimePath,
        },
        instance: buildInstanceObservation(input),
        mcp: buildMcpObservation(input.mcpSource, input.sessionInit),
    };
}

function buildInstanceObservation(
    input: ChatObservationInput,
): ChatInstanceObservation | null {
    const port = input.port;
    const pendingInputCount = Math.max(
        port?.pendingInputCount ?? 0,
        input.pendingInputs,
    );
    if (port == null) return null;
    return {
        state: "live",
        // SDK 持有激活记录，但没有更细的 transport 生命周期；running 仍按端口值。
        running: port.running,
        pendingInputCount,
        pendingInteractionCount: port.pendingInteractionCount,
    };
}

function buildMcpObservation(
    source: "session-init" | "unknown",
    sessionInit: ChatSessionInit,
): ChatMcpObservation {
    // bindExisting 后 SDK 不持有既有 Session 的配置，宁可为空也不伪造。
    if (source === "unknown") return { source: "unknown", servers: [] };
    const servers = sessionInit.mcpServers;
    if (servers === undefined) return { source: "session-init", servers: [] };
    return {
        source: "session-init",
        servers: Object.entries(servers)
            .map(([name, server]) => observeMcpServer(name, server))
            .sort((left, right) =>
                compareObservationIds(left.name, right.name),
            ),
    };
}

function observeMcpServer(
    name: string,
    server: AcpMcpServer,
): McpServerObservation {
    const common = {
        name,
        type: server.type,
        // sandbox 归属由入口按 endpoint 匹配补充；这里只声明配置来源。
        source: "config" as const,
        health: "unknown" as const,
    };
    if (server.type !== "http") {
        // stdio 不输出 command/args/env；acp 不输出实例或工厂引用。
        return common;
    }
    const endpoint = safeEndpointUrl(server.url);
    const headerNames = headerNamesOf(server.headers);
    return {
        ...common,
        ...(endpoint === undefined ? {} : { endpoint }),
        ...(headerNames === undefined ? {} : { headerNames }),
    };
}
