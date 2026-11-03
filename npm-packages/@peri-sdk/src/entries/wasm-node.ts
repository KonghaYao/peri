/**
 * Node embedding surface for the WASM runtime (Bun, Node, workerd with `nodejs_compat`).
 * The SDK owns ACP runtime startup/close evidence; the embedder owns platform hooks and capacity.
 */

export { instantiatePeriWasm, loadPeriWasm } from "../wasm/loader";
export type { NativeWasmAcp, PeriWasmInstantiateOptions, PeriWasmModule, PeriWasmModuleFactory } from "../wasm/loader";
export { createNodeDnsPort, createNodeNetworkPort } from "../wasm/ports";
export type { WasmDnsAddress, WasmDnsLookupOptions, WasmDnsPort, WasmDnsPortOptions, WasmNetworkPort } from "../wasm/ports";
export { WasmRuntimeStartupError, startWasmRuntime } from "../wasm/runtime";
export { WasmRuntimeLease, WasmRuntimePool } from "../wasm/runtime-pool";
export type {
  WasmRuntimeCloseReport, WasmRuntimeInstanceSnapshot, WasmRuntimeLeaseOptions, WasmRuntimePoolOptions,
  WasmRuntimePoolSnapshot, WasmRuntimeStarter, WasmRuntimeState,
} from "../wasm/runtime-pool";
export type {
  WasmRuntimeCleanupOutcome, WasmRuntimeDiagnosticKind, WasmRuntimeLifecycleEvent, WasmRuntimeOptions,
  WasmRuntimePhase, WasmRuntimePorts,
} from "../wasm/runtime";
export { WasmAcpTransport } from "../wasm/transport";
export type { WasmAcpTransportOptions } from "../wasm/transport";
export { DeadlineError, withDeadline } from "../util/deadline";
export type { Transport, JsonRpcNotification, ReverseRequestHandler } from "../transport/types";
