/**
 * Host embedding surface for Node-compatible runtimes (Bun, Node, workerd with `nodejs_compat`).
 * The SDK owns ACP host startup/close evidence; the embedder owns platform hooks and capacity.
 */

export { instantiatePeriWasm, loadPeriWasm } from "./wasm/loader";
export type { NativeWasmAcp, PeriWasmInstantiateOptions, PeriWasmModule, PeriWasmModuleFactory } from "./wasm/loader";
export { createNodeDnsPort, createNodeNetworkPort, createNodeSchedulerPort } from "./wasm/ports";
export type { WasmDnsAddress, WasmDnsLookupOptions, WasmDnsPort, WasmDnsPortOptions, WasmNetworkPort, WasmSchedulerPort } from "./wasm/ports";
export { DEFAULT_WASM_HOST_CLEANUP_TIMEOUT_MS, PeriWasmHostStartupError, startPeriWasmHost } from "./wasm/host";
export type {
  PeriWasmHostCleanupOutcome, PeriWasmHostDiagnosticKind, PeriWasmHostLifecycleEvent, PeriWasmHostOptions,
  PeriWasmHostPhase, PeriWasmHostPorts,
} from "./wasm/host";
export { WasmAcpTransport } from "./transport/wasm-transport";
export type { WasmAcpTransportOptions } from "./transport/wasm-transport";
export { DeadlineError, withDeadline } from "./util/deadline";
export type { Transport, JsonRpcNotification, ReverseRequestHandler } from "./transport/types";
