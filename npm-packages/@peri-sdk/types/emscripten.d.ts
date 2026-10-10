/**
 * `bun run build:workers` 把 SDK 产物（`dist/wasm/` 与 `dist/wasm-host.js`）复制到
 * `examples/workers/dist/`；这些声明让示例的 JS 也进入 tsc 检查，且沿用 SDK 的真实类型。
 * 复制产物的模块名在 `include` 之外，因此按 specifier 声明；示例新用到其他导出时在此追加。
 */
declare module "*peri-wasm.js" {
  const factory: (options?: Record<string, unknown>) => Promise<import("../src/wasm/loader").PeriWasmModule>;
  export default factory;
}

declare module "*wasm-host.js" {
  export const startPeriWasmHost: typeof import("../src/wasm-host").startPeriWasmHost;
  export const createNodeSchedulerPort: typeof import("../src/wasm-host").createNodeSchedulerPort;
}

declare module "*.wasm" {
  const module: WebAssembly.Module;
  export default module;
}
