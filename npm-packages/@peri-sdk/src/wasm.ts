/**
 * Platform-neutral Emscripten Host surface: the ACP transport plus host startup, usable from any
 * runtime that provides `fetch`, timers and WebAssembly (browsers included). Platform-specific
 * ports (sockets, DNS) live in `./wasm-host` and are not reachable from this entry.
 */
export { instantiatePeriWasm, loadPeriWasm } from "./wasm/loader";
export type { NativeWasmAcp, PeriWasmInstantiateOptions, PeriWasmModule, PeriWasmModuleFactory } from "./wasm/loader";

export { WasmAcpTransport } from "./transport/wasm-transport";
export type { WasmAcpTransportOptions } from "./transport/wasm-transport";

export { createSchedulerPort } from "./wasm/scheduler";
export type { WasmSchedulerPort } from "./wasm/scheduler";

export { DEFAULT_WASM_HOST_CLEANUP_TIMEOUT_MS, PeriWasmHostStartupError, startPeriWasmHost } from "./wasm/host";
export type {
  PeriWasmHostCleanupOutcome, PeriWasmHostDiagnosticKind, PeriWasmHostLifecycleEvent, PeriWasmHostOptions,
  PeriWasmHostPhase, PeriWasmHostPorts,
} from "./wasm/host";

export { DeadlineError, withDeadline } from "./util/deadline";
export type { Transport, JsonRpcNotification, ReverseRequestHandler } from "./transport/types";
