/**
 * 只读实例观测入口：组合 Chat 与 Sandbox 快照，供调用方按自身节奏轮询。
 *
 * 不启动 ACP/sandbox，不写任何状态，也不提供订阅通道。Agent 分组规则：
 * 一个 Chat 就是一个 Agent 节点，`agentId` 使用 `chatId`；sandbox 仅通过
 * MCP endpoint（origin + pathname）与 Chat 匹配，每个 Chat 最多关联一个，
 * 同一 sandbox 可被多个 Chat 共享引用。
 */
import type {
    AgentObservation,
    BackendSnapshot,
    ChatObservation,
    ChatObservationSource,
    McpServerObservation,
    SandboxObservation,
    SandboxObservationSource,
} from "../contracts/observability";
import {
    compareObservationIds,
    redactObservationText,
    safeEndpointUrl,
} from "../util/observability";

export type * from "../contracts/observability";

/**
 * 输入只要求结构能力：`ChatManager.observe()` 与 `SandboxManager.snapshot()`。
 *
 * 不 import 具体 sandbox 实现，主入口也不会因此引入 sandbox 依赖。
 */
export interface ObserveBackendInput {
    chats: ChatObservationSource;
    sandboxes?: SandboxObservationSource;
}

export function observeBackend(input: ObserveBackendInput): BackendSnapshot {
    if (
        !input ||
        typeof input !== "object" ||
        typeof input.chats?.observe !== "function"
    ) {
        throw new TypeError(
            "observeBackend requires chats with a sync observe() method",
        );
    }
    if (
        input.sandboxes !== undefined &&
        typeof input.sandboxes.snapshot !== "function"
    ) {
        throw new TypeError(
            "observeBackend requires sandboxes with a sync snapshot() method",
        );
    }

    const chats = input.chats
        .observe()
        .map(normalizeChat)
        .sort((left, right) =>
            compareObservationIds(left.chatId, right.chatId),
        );
    const sandboxes = (input.sandboxes?.snapshot() ?? [])
        .map(normalizeSandbox)
        .sort((left, right) =>
            compareObservationIds(left.sandboxId, right.sandboxId),
        );

    const sandboxEndpoints = new Set<string>();
    for (const sandbox of sandboxes) {
        if (sandbox.mcpEndpoint !== null) {
            sandboxEndpoints.add(sandbox.mcpEndpoint);
        }
    }
    const chatEndpoints = new Set<string>();
    for (const chat of chats) {
        for (const server of chat.mcp.servers) {
            if (server.endpoint !== undefined) chatEndpoints.add(server.endpoint);
        }
    }

    // sandbox 可被多个 Chat 共享：每个引用其 endpoint 的 Chat 都拿到同一个
    // 观测对象；多个 sandbox 同 endpoint 时按 sandboxId 顺序取第一个。
    const agents: AgentObservation[] = chats.map((chat) => {
        const sandbox = sandboxes.find(
            (candidate) =>
                candidate.mcpEndpoint !== null &&
                chat.mcp.servers.some(
                    (server) => server.endpoint === candidate.mcpEndpoint,
                ),
        );
        return {
            agentId: chat.chatId,
            chat: refineChatSources(chat, sandboxEndpoints),
            sandbox: sandbox ?? null,
        };
    });

    // 定义按字面执行：只要没有任何 Chat 引用该 endpoint，就是 orphan；
    // 同一 endpoint 的重复 sandbox 都算被引用，共享引用不影响该判定。
    const orphanSandboxes = sandboxes.filter(
        (sandbox) =>
            sandbox.mcpEndpoint === null ||
            !chatEndpoints.has(sandbox.mcpEndpoint),
    );

    const chatServers = agents.flatMap((agent) => agent.chat.mcp.servers);
    return {
        observedAt: new Date().toISOString(),
        agents,
        orphanSandboxes,
        totals: {
            agents: agents.length,
            chats: chats.length,
            activeInstances: chats.filter(
                (chat) =>
                    chat.instance !== null && chat.instance.state === "live",
            ).length,
            mcpServers: chatServers.length,
            sandboxMcpServers: chatServers.filter(
                (server) => server.source === "sandbox",
            ).length,
            sandboxes: sandboxes.length,
        },
    };
}

function normalizeChat(chat: ChatObservation): ChatObservation {
    // `unknown` 时没有权威配置，拒绝下游顺手附带的服务列表。
    const mcpSource =
        chat.mcp.source === "session-init" ? "session-init" : "unknown";
    return {
        chatId: chat.chatId,
        sessionId: chat.sessionId,
        bindingRevision: chat.bindingRevision,
        phase: chat.phase,
        phaseLimited: true,
        workspace: {
            id: chat.workspace.id,
            runtimePath: chat.workspace.runtimePath,
        },
        instance:
            chat.instance === null
                ? null
                : {
                      state: chat.instance.state,
                      running: chat.instance.running,
                      pendingInputCount: chat.instance.pendingInputCount,
                      pendingInteractionCount:
                          chat.instance.pendingInteractionCount,
                  },
        mcp: {
            source: mcpSource,
            servers:
                mcpSource === "session-init"
                    ? chat.mcp.servers.map(normalizeChatServer)
                    : [],
        },
    };
}

/** 公共入口再次做安全投影：自定义实现也不能把 header 值带进快照。 */
function normalizeChatServer(
    server: McpServerObservation,
): McpServerObservation {
    const endpoint = safeEndpointUrl(server.endpoint);
    return {
        name: server.name,
        type: server.type,
        source: server.source === "sandbox" ? "sandbox" : "config",
        health: "unknown",
        ...(endpoint === undefined ? {} : { endpoint }),
        ...(server.headerNames === undefined
            ? {}
            : { headerNames: [...server.headerNames] }),
    };
}

function normalizeSandbox(sandbox: SandboxObservation): SandboxObservation {
    return {
        sandboxId: sandbox.sandboxId,
        state: sandbox.state,
        serverName: sandbox.serverName,
        mcpEndpoint: safeEndpointUrl(sandbox.mcpEndpoint) ?? null,
        workspace:
            sandbox.workspace === null
                ? null
                : {
                      id: sandbox.workspace.id,
                      runtimePath: sandbox.workspace.runtimePath,
                      ...(sandbox.workspace.hostPath === undefined
                          ? {}
                          : { hostPath: sandbox.workspace.hostPath }),
                  },
        lastError:
            sandbox.lastError === null
                ? null
                : {
                      code: sandbox.lastError.code,
                      message: redactObservationText(
                          sandbox.lastError.message,
                      ),
                  },
    };
}

function refineChatSources(
    chat: ChatObservation,
    sandboxEndpoints: ReadonlySet<string>,
): ChatObservation {
    if (chat.mcp.servers.length === 0) return chat;
    return {
        ...chat,
        mcp: {
            ...chat.mcp,
            servers: chat.mcp.servers.map((server) => {
                const source =
                    server.endpoint !== undefined &&
                    sandboxEndpoints.has(server.endpoint)
                        ? ("sandbox" as const)
                        : ("config" as const);
                return source === server.source
                    ? server
                    : {
                          name: server.name,
                          type: server.type,
                          source,
                          health: "unknown",
                          ...(server.endpoint === undefined
                              ? {}
                              : { endpoint: server.endpoint }),
                          ...(server.headerNames === undefined
                              ? {}
                              : {
                                    headerNames: [...server.headerNames],
                                }),
                      };
            }),
        },
    };
}
