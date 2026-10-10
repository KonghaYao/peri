/** ACP transport for the Emscripten Host, sharing the standard SDK interfaces. */
export { instantiatePeriWasm, loadPeriWasm } from "./wasm/loader";
export type { NativeWasmAcp, PeriWasmInstantiateOptions, PeriWasmModule, PeriWasmModuleFactory } from "./wasm/loader";

export { WasmAcpTransport } from "./transport/wasm-transport";
export type { WasmAcpTransportOptions } from "./transport/wasm-transport";
