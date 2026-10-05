/** The Emscripten module generated from this workspace's peri-wasm binary. */
export interface NativeWasmAcp {
  free?(): void;
  send(frame: string): Promise<void>;
  recv(): Promise<string | null | undefined>;
  close(): Promise<void>;
}

export interface PeriWasmModule {
  PeriWasmAcp: { start(configJson: string): Promise<NativeWasmAcp> };
  ENV?: Record<string, string>;
}

type ModuleFactory = (options?: Record<string, unknown>) => Promise<PeriWasmModule>;
const modules = new Map<string, Promise<PeriWasmModule>>();
const environments = new Map<string, Record<string, string>>();

/** Load the WASM artifact bundled next to dist/index.js. One module instance is shared per URL. */
export function loadPeriWasm(moduleUrl?: string | URL, env?: Readonly<Record<string, string>>): Promise<PeriWasmModule> {
  const url = moduleUrl ? new URL(String(moduleUrl), import.meta.url) : new URL("./wasm/peri-wasm.js", import.meta.url);
  const key = url.href;
  const environment = Object.fromEntries(Object.entries(env ?? {}).sort(([left], [right]) => left.localeCompare(right)));
  for (const [name, value] of Object.entries(environment)) {
    if (!name || name.includes("=") || name.includes("\0") || typeof value !== "string" || value.includes("\0"))
      throw new Error("Invalid WASM environment variable");
  }
  let loading = modules.get(key);
  if (loading && env !== undefined && JSON.stringify(environments.get(key)) !== JSON.stringify(environment))
    throw new Error("WASM module environment is fixed at initialization; use a separate module URL for another environment");
  if (!loading) {
    environments.set(key, environment);
    loading = import(/* @vite-ignore */ key).then(async (source: { default?: ModuleFactory }) => {
      if (typeof source.default !== "function") throw new Error("Invalid peri-wasm module factory");
      const module = await source.default({
        preRun: [(runtime: PeriWasmModule) => {
          if (Object.keys(environment).length === 0) return;
          if (!runtime.ENV) throw new Error("peri-wasm artifact does not export ENV; rebuild peri-wasm");
          Object.assign(runtime.ENV, environment);
        }],
      });
      if (typeof module.PeriWasmAcp?.start !== "function")
        throw new Error("peri-wasm artifact does not expose the ACP Host");
      return module;
    }).catch((error: unknown) => {
      modules.delete(key);
      environments.delete(key);
      throw error;
    });
    modules.set(key, loading);
  }
  return loading;
}
