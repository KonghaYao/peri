/**
 * Node embedding surface for the WASM runtime (Bun, Node, workerd with `nodejs_compat`).
 * The SDK owns ACP runtime startup/close evidence; the embedder owns platform hooks and capacity.
 */

export { instantiatePeriWasm, loadPeriWasm } from "./wasm/loader";
export type { NativeWasmAcp, PeriWasmInstantiateOptions, PeriWasmModule, PeriWasmModuleFactory } from "./wasm/loader";
export { createNodeDnsPort, createNodeNetworkPort } from "./wasm/ports";
export type { WasmDnsAddress, WasmDnsLookupOptions, WasmDnsPort, WasmDnsPortOptions, WasmNetworkPort } from "./wasm/ports";
export { createSchedulerPort } from "./wasm/scheduler";
export type { WasmSchedulerPort } from "./wasm/scheduler";
export { DEFAULT_WASM_RUNTIME_CLEANUP_TIMEOUT_MS, WasmRuntimeStartupError, startWasmRuntime } from "./wasm/runtime";
export {
  DEFAULT_WASM_RUNTIME_STARTUP_TIMEOUT_MS, WasmRuntimeLease, WasmRuntimePool,
} from "./wasm/runtime-pool";
export type {
  WasmRuntimeCloseReport, WasmRuntimeLeaseOptions, WasmRuntimePoolOptions, WasmRuntimeStarter,
  WasmRuntimeState,
} from "./wasm/runtime-pool";
export type {
  WasmRuntimeCleanupOutcome, WasmRuntimeDiagnosticKind, WasmRuntimeLifecycleEvent, WasmRuntimeOptions,
  WasmRuntimePhase, WasmRuntimePorts,
} from "./wasm/runtime";
export { WasmAcpTransport } from "./transport/wasm-transport";
export type { WasmAcpTransportOptions } from "./transport/wasm-transport";
export { DeadlineError, withDeadline } from "./util/deadline";
export type { Transport, JsonRpcNotification, ReverseRequestHandler } from "./transport/types";
