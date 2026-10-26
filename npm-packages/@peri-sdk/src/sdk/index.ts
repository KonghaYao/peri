/**
 * @peri-code/sdk 主入口：Chat 控制面、展示契约与运行后端装配类型。
 *
 * 运行实例由 `ChatManager` 按激活装配；旧业务门面已删除。
 * 浏览器安全展示出口见 `/view`，WASM 运行后端表面见 `/wasm` 与
 * `/wasm-node`，无进程装配入口见 `/portable`。
 */

export * from "../chat/public";

export { MemoryKV } from "../kv/memory-kv";
export type { AtomicSessionOwnershipKv } from "../kv/types";
export { SessionOwnershipConflictError } from "../kv/session-ownership-conflict-error";
export { sessionOwnershipKey } from "../kv/session-ownership-key";

export { BareHarnessConfig } from "../config/bare-harness-config";
export type { MetaHarnessKey, PeriConfig } from "../config/peri-config";
export { TursoStorage } from "../storage/turso-storage";
export type { SessionStorage } from "../storage/types";
export type { SessionSummary } from "../storage/session-summary";
export {
    SESSION_BY_ID_SQL,
    SESSION_LIST_SQL,
} from "../storage/session-summary";

export type {
    AcpMcpServer,
    AcpMcpServerInstance,
    AcpMcpServerInstanceFactory,
    AcpMcpTransport,
    ElicitationDecision,
    ElicitationRequest,
    PermissionRequest,
} from "../agent/types";

export { RpcError } from "../transport/rpc-error";
export type {
    JsonRpcNotification,
    ReverseRequestHandler,
    Transport,
} from "../transport/types";
