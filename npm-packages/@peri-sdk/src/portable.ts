/**
 * 不装配 Bun/stdio 进程或本地 SQLite 的 SDK 入口。
 * 准入摘要仍使用 node:crypto；Workers 需要 nodejs_compat，不能视作纯浏览器入口。
 * 存储与凭证由调用方显式传入，SDK 不读取宿主环境变量。
 */

export { initializeAcpClient, ACP_PROTOCOL_VERSION } from "./agent/acp-handshake";
export type { AcpInitializeOptions } from "./agent/acp-handshake";
export { BareHarnessConfig } from "./config/bare-harness-config";
export type { MetaHarnessKey, PeriConfig } from "./config/peri-config";
export { ExecutionAdmissionCore } from "./execution/admission-core";
export type {
  AdmissionLedger, AdmissionRequestRecord, AdmissionStepClaim, AdmissionSnapshot, AdmissionRequest,
  AdmissionOutcome, SettlementRequest, SettlementOutcome, EntryRequest, EntryOutcome, ExecutionAdmissionCoreOptions,
} from "./execution/admission-core";
export { KvExecutionRegistry } from "./execution/kv-registry";
export type { AtomicExecutionKv, ExecutionKvValue } from "./execution/kv-registry";
export { MemoryExecutionRegistry, MemoryExecutionKv } from "./execution/memory-registry";
export { ExecutionCoordinator } from "./execution/coordinator";
export type { ExecutionCoordinatorOptions, AdmissionResult } from "./execution/coordinator";
export { ExecutionMutationConflictError } from "./execution/registry-ledger";
export type * from "./execution/types";
export { SessionDocs } from "./state/session-docs";
export type { SessionDocsOptions } from "./state/session-docs";
export { CheckpointImportError, CHECKPOINT_VERSION, SUPPORTED_SCHEMA_VERSIONS } from "./state/checkpoint";
export type {
  DisplayCheckpoint, CheckpointBlock, CheckpointEntry, CheckpointToolCall, CheckpointTurn,
  CheckpointInteraction, CheckpointImportErrorCode,
} from "./state/checkpoint";
export type { PublicError, PublicErrorCode } from "./state/doc-model";
export { ToolPayloadCapacityError } from "./state/tool-payloads";
export {
  TOOL_PAYLOAD_INLINE_BYTES, TOOL_PAYLOAD_PREVIEW_CHARS, TOOL_PAYLOAD_MAX_BYTES,
  SESSION_INLINE_PAYLOAD_BUDGET_BYTES, SESSION_EXTERNAL_PAYLOAD_BUDGET_BYTES,
} from "./state/tool-payloads";
export type { ToolPayloadBudgets, ToolPayloadRef } from "./state/tool-payloads";
export { INTERACTION_HISTORY_LIMIT, INTERACTION_FORM_MAX_FIELDS, evaluateElicitationForm, validateElicitationContent } from "./state/interaction-projection";
export type { FormField } from "./state/interaction-projection";
export { InteractionResponder, parseInteractionAnswer } from "./agent/interaction-responder";
export type { InteractionAnswer } from "./agent/interaction-responder";
export type {
  AgentOptions, InteractionCallbacks, PermissionRequest, ElicitationRequest, ElicitationDecision,
} from "./agent/types";
export { TursoStorage } from "./storage/turso-storage";
export { SESSION_BY_ID_SQL, SESSION_LIST_SQL } from "./storage/session-summary";
export type { SessionStorage } from "./storage/types";
export type { SessionSummary } from "./storage/session-summary";
export { EventStreamOverflowError } from "./transport/notification-budget";
export { RpcError } from "./transport/rpc-error";
export type { JsonRpcNotification, ReverseRequestHandler, Transport } from "./transport/types";
export * from "./sync/index";
export { readSessionView, SessionViewStore } from "./view/session-view";
export type {
  SessionView, EntryView, EntryBlockView, ToolView, PlanEntryView, TaskView, InteractionView,
  InputQueueView, SessionInfoView,
} from "./view/session-view";
