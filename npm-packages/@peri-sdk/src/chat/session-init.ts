import type { AcpMcpServer } from "../contracts/acp";
import type { ChatSessionInit } from "./types";

function copyMcpServer(server: AcpMcpServer): AcpMcpServer {
    if (server.type === "http") {
        return {
            type: "http",
            url: server.url,
            ...(server.headers === undefined
                ? {}
                : { headers: { ...server.headers } }),
        };
    }
    if (server.type === "stdio") {
        return {
            type: "stdio",
            command: server.command,
            ...(server.args === undefined ? {} : { args: [...server.args] }),
            ...(server.env === undefined ? {} : { env: { ...server.env } }),
        };
    }
    // `server` 可能是调用方持有的实例或工厂，不可克隆；只复制描述对象本身。
    return {
        type: "acp",
        server: server.server,
        ...(server.serverId === undefined ? {} : { serverId: server.serverId }),
    };
}

/**
 * 复制 Session 建立输入，隔离每个 Chat 的嵌套容器与描述。
 *
 * `mcpServers` 容器、http headers、stdio args/env 都复制为新对象；`acp` 的
 * `server` 实例/工厂保留原引用（不可克隆语义），但描述对象本身是新的。
 * Manager 默认值与 `createChat({ session })` 共用本函数。
 */
export function copyChatSessionInit(
    init?: ChatSessionInit,
): ChatSessionInit {
    if (init === undefined) return {};
    const copy: ChatSessionInit = {};
    if (init.path !== undefined) copy.path = init.path;
    if (init.instructions !== undefined) copy.instructions = init.instructions;
    if (init.mcpServers !== undefined) {
        const servers: Record<string, AcpMcpServer> = {};
        for (const [name, server] of Object.entries(init.mcpServers)) {
            Object.defineProperty(servers, name, {
                value: copyMcpServer(server),
                enumerable: true,
                writable: true,
                configurable: true,
            });
        }
        copy.mcpServers = servers;
    }
    return copy;
}
