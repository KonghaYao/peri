/**
 * Platform-neutral Emscripten WASM runtime surface: the ACP transport plus runtime startup, usable
 * from any runtime that provides `fetch`, timers and WebAssembly (browsers included). Platform-specific
 * ports (sockets, DNS) live in `./wasm-node` and are not reachable from this entry.
 */
export { instantiatePeriWasm, loadPeriWasm } from "../wasm/loader";
export type { NativeWasmAcp, PeriWasmInstantiateOptions, PeriWasmModule, PeriWasmModuleFactory } from "../wasm/loader";

export { WasmAcpTransport } from "../wasm/transport";
export type { WasmAcpTransportOptions } from "../wasm/transport";

export { WasmRuntimeStartupError, startWasmRuntime } from "../wasm/runtime";
export { WasmRuntimeLease, WasmRuntimePool } from "../wasm/runtime-pool";
export type {
  WasmRuntimeAcquireOptions, WasmRuntimeCloseReport, WasmRuntimeInstanceSnapshot, WasmRuntimeLeaseOptions,
  WasmRuntimePoolOptions, WasmRuntimePoolSnapshot, WasmRuntimeStarter, WasmRuntimeStarterOptions,
  WasmRuntimeState,
} from "../wasm/runtime-pool";
export type {
  WasmRuntimeCleanupOutcome, WasmRuntimeDiagnosticKind, WasmRuntimeLifecycleEvent, WasmRuntimeOptions,
  WasmRuntimePhase, WasmRuntimePorts,
} from "../wasm/runtime";

export { DeadlineError, withDeadline } from "../util/deadline";
export type { Transport, JsonRpcNotification, ReverseRequestHandler } from "../transport/types";
