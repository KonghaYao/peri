/**
 * Host embedding surface for Node-compatible runtimes (Bun, Node, workerd with `nodejs_compat`).
 * The SDK owns ACP host startup/close evidence; the embedder owns platform hooks and capacity.
 */

export { instantiatePeriWasm, loadPeriWasm } from "./wasm/loader";
export type { NativeWasmAcp, PeriWasmInstantiateOptions, PeriWasmModule, PeriWasmModuleFactory } from "./wasm/loader";
export { createNodeDnsPort, createNodeNetworkPort } from "./wasm/ports";
export type { WasmDnsAddress, WasmDnsLookupOptions, WasmDnsPort, WasmDnsPortOptions, WasmNetworkPort } from "./wasm/ports";
export { createSchedulerPort } from "./wasm/scheduler";
export type { WasmSchedulerPort } from "./wasm/scheduler";
export { DEFAULT_WASM_HOST_CLEANUP_TIMEOUT_MS, PeriWasmHostStartupError, startPeriWasmHost } from "./wasm/host";
export {
  DEFAULT_WASM_HOST_STARTUP_TIMEOUT_MS, PeriWasmHostLease, PeriWasmHostManager,
} from "./wasm/host-lifecycle";
export type {
  PeriWasmHostCloseReport, PeriWasmHostLeaseOptions, PeriWasmHostManagerOptions, PeriWasmHostStarter,
  PeriWasmHostState,
} from "./wasm/host-lifecycle";
export type {
  PeriWasmHostCleanupOutcome, PeriWasmHostDiagnosticKind, PeriWasmHostLifecycleEvent, PeriWasmHostOptions,
  PeriWasmHostPhase, PeriWasmHostPorts,
} from "./wasm/host";
export { WasmAcpTransport } from "./transport/wasm-transport";
export type { WasmAcpTransportOptions } from "./transport/wasm-transport";
export { DeadlineError, withDeadline } from "./util/deadline";
export type { Transport, JsonRpcNotification, ReverseRequestHandler } from "./transport/types";
