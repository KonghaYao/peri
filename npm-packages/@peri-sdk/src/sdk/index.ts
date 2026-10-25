/**
 * @peri-code/sdk 主入口：Chat 控制面、展示契约与宿主装配类型。
 *
 * 运行实例由 `ChatManager` 按激活装配；旧 `Agent`/`ManagedAgents`/`SessionDocs`
 * 门面已删除。浏览器安全展示出口见 `/view`，WASM 宿主表面见 `/wasm` 与
 * `/wasm-host`，无进程装配入口见 `/portable`。
 */

export * from "../chat/public";

export { MemoryKV } from "../kv/memory-kv";
export type { AtomicSessionClaimKv } from "../kv/types";
export { SessionClaimConflictError } from "../kv/session-claim-conflict-error";
export { sessionClaimKey } from "../kv/session-claim-key";

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
