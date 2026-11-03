import type { AcpMcpServer } from "../contracts/acp";
import type { ChatSessionOptions } from "./types";

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
 * 复制公开的 Session 建立配置，隔离每个 Chat 的嵌套容器与描述。
 *
 * cwd 不在此复制：它由 `ChatCreateOptions.workspace` 派生后由 Manager 写入
 * 内部 `ChatSessionInit.path`。`mcpServers` 容器、http headers、stdio
 * args/env 都复制为新对象；`acp` 的 `server` 实例/工厂保留原引用（不可克隆
 * 语义），但描述对象本身是新的。Manager 默认值与 `createChat({ session })`
 * 共用本函数。
 */
export function copyChatSessionInit(
    init?: ChatSessionOptions,
): ChatSessionOptions {
    if (init === undefined) return {};
    if (init === null || typeof init !== "object" || Array.isArray(init)) {
        throw new TypeError(
            "session must be an object with instructions/mcpServers",
        );
    }
    // path 已从公开类型移除；显式传入时提示改用 workspace + sessionPath。
    if (Object.prototype.hasOwnProperty.call(init, "path")) {
        throw new TypeError(
            "session.path is not supported; use createChat({ workspace, sessionPath })",
        );
    }
    const copy: ChatSessionOptions = {};
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
