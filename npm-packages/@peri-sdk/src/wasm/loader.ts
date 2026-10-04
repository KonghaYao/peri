/** The Emscripten module generated from this workspace's peri-wasm binary. */
export interface NativeWasmAcp {
  free?(): void;
  send(frame: string): Promise<void>;
  recv(): Promise<string | null | undefined>;
  close(): Promise<void>;
}

export interface PeriWasmModule {
  PeriWasmAcp: { start(configJson: string): Promise<NativeWasmAcp> };
}

type ModuleFactory = (options?: Record<string, unknown>) => Promise<PeriWasmModule>;
const modules = new Map<string, Promise<PeriWasmModule>>();

/** Load the WASM artifact bundled next to dist/index.js. One module instance is shared per URL. */
export function loadPeriWasm(moduleUrl?: string | URL): Promise<PeriWasmModule> {
  const url = moduleUrl ? new URL(String(moduleUrl), import.meta.url) : new URL("./wasm/peri-wasm.js", import.meta.url);
  const key = url.href;
  let loading = modules.get(key);
  if (!loading) {
    loading = import(/* @vite-ignore */ key).then(async (source: { default?: ModuleFactory }) => {
      if (typeof source.default !== "function") throw new Error("Invalid peri-wasm module factory");
      const module = await source.default();
      if (typeof module.PeriWasmAcp?.start !== "function")
        throw new Error("peri-wasm artifact does not expose the ACP Host");
      return module;
    }).catch((error: unknown) => {
      modules.delete(key);
      throw error;
    });
    modules.set(key, loading);
  }
  return loading;
}
