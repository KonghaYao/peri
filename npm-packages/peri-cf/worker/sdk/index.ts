/**
 * The only place peri-cf wires itself to `@peri-code/sdk`. Every runtime value below comes from a
 * published SDK entry: `/portable` for platform-neutral semantics, `/wasm-host` for the owned
 * Emscripten host, `/view` for the browser replica. No `@peri-sdk/src` path is imported anywhere.
 *
 * 当前 ACP 契约里没有执行准入与精确停止协议：取消走 `session/cancel` 通知，关闭证据由宿主
 * 生命周期提供，因此这里不再导出准入/控制类型。
 */

export {
  startPeriWasmHost, PeriWasmHostStartupError, createNodeNetworkPort, createNodeSchedulerPort,
  createNodeDnsPort, DeadlineError, withDeadline,
} from "@peri-code/sdk/wasm-host";
export type {
  NativeWasmAcp, PeriWasmHostCleanupOutcome, PeriWasmHostDiagnosticKind, PeriWasmHostLifecycleEvent,
  PeriWasmHostPhase, PeriWasmHostPorts, PeriWasmModule, WasmDnsPort, WasmNetworkPort, WasmSchedulerPort,
} from "@peri-code/sdk/wasm-host";

export {
  BareHarnessConfig, SessionDocs, TursoStorage, SESSION_BY_ID_SQL, SESSION_LIST_SQL, initializeAcpClient,
  CHECKPOINT_VERSION, SUPPORTED_SCHEMA_VERSIONS, CheckpointImportError,
  InteractionResponder, parseInteractionAnswer, evaluateElicitationForm, validateElicitationContent,
  acknowledgeDelivery, credentialFingerprint, reserveDeliveryFrame, decodeAckFrame, decodeAuthFrame,
  decodeSyncFrame, encodeAckFrame, encodeAuthFrame, encodeSyncFrame, frameBytes, FRAME_ACK, FRAME_AUTH,
  FRAME_SNAPSHOT, FRAME_UPDATE, MAX_ACK_FRAME_BYTES, MAX_AUTH_FRAME_BYTES, MAX_SYNC_FRAME_BYTES,
  SYNC_WIRE_VERSION,
} from "@peri-code/sdk/portable";
export type {
  AckFrame, AuthFrame, DeliveryLedger, DocSnapshot, DocStateVector, DocUpdate, JsonRpcNotification, PeriConfig,
  SessionStorage, SessionSummary, SyncFrame, SyncFramePayload, Transport,
  DisplayCheckpoint, CheckpointBlock, CheckpointEntry, CheckpointToolCall, CheckpointTurn, CheckpointInteraction,
  InteractionAnswer, InteractionCallbacks, PermissionRequest, ElicitationRequest, ElicitationDecision, FormField,
} from "@peri-code/sdk/portable";

export { SessionDocSync, SessionDocReplica, SessionViewStore, readSessionView } from "@peri-code/sdk/view";
export type { SessionView, EntryView, ToolView, TurnView, InteractionView } from "@peri-code/sdk/view";
