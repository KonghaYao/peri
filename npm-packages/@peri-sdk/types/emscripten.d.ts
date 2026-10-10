/** Emscripten 产物由 `bun run build` 生成；Wrangler 探针以 ES module 与 wasm 资源导入它们。 */
declare module "*peri-wasm.js" {
  const factory: (options?: Record<string, unknown>) => Promise<Record<string, unknown>>;
  export default factory;
}

declare module "*.wasm" {
  const module: WebAssembly.Module;
  export default module;
}
