import { loadWasmArtifact, wasmArtifactUrl } from "./artifact-cache";

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
  /**
   * Emscripten 实例导出的线性内存；只读监控用。
   * 只有 loader 自己实例化产物时才会挂载；宿主 injected factory 可自行提供。
   */
  readonly wasmMemory?: WebAssembly.Memory;
}

export type PeriWasmModuleFactory = (options?: Record<string, unknown>) => Promise<PeriWasmModule>;

export interface PeriWasmInstantiateOptions {
  /**
   * Artifact location, resolved against `import.meta.url`; defaults to the `wasm/peri-wasm.js`
   * shipped beside the SDK entry. Only read when no `moduleFactory` is injected.
   */
  moduleUrl?: string | URL;
  /** Environment fixed for this module instance; merged into the artifact's `ENV` before startup. */
  env?: Readonly<Record<string, string>>;
  /**
   * 静态导入的 Emscripten factory；注入时运行后端已拥有产物位置，`moduleUrl` 既不解析也不要求可解析。
   * workerd 打包模块的 `import.meta.url` 是 `undefined`，这类运行后端只能走本入口。
   */
  moduleFactory?: (options: Record<string, unknown>) => Promise<PeriWasmModule>;
  /** Runtime-owned Emscripten options (network, timers, DNS, `instantiateWasm`) for this instance. */
  moduleOptions?: Readonly<Record<string, unknown>>;
}

const modules = new Map<string, Promise<PeriWasmModule>>();
const environments = new Map<string, Record<string, string>>();

function normalizeEnvironment(env?: Readonly<Record<string, string>>): Record<string, string> {
  const environment = Object.fromEntries(Object.entries(env ?? {}).sort(([left], [right]) => left.localeCompare(right)));
  for (const [name, value] of Object.entries(environment)) {
    if (!name || name.includes("=") || name.includes("\0") || typeof value !== "string" || value.includes("\0"))
      throw new Error("Invalid WASM environment variable");
  }
  return environment;
}

const DEFAULT_ARTIFACT = "./wasm/peri-wasm.js";

/** 解析产物位置；失败时给出运行后端可执行的结论，而不是裸 `TypeError: Invalid URL string`。 */
function artifactUrl(moduleUrl?: string | URL): URL {
  const specifier = String(moduleUrl ?? DEFAULT_ARTIFACT);
  try {
    return new URL(specifier, import.meta.url);
  } catch (cause) {
    throw new Error(
      `Cannot resolve the Peri WASM artifact URL from ${JSON.stringify(specifier)}: this runtime does not ` +
      `provide a usable import.meta.url (workerd reports undefined for bundled modules). Pass an absolute ` +
      `artifact URL, or inject moduleFactory so the runtime owns the artifact location.`,
      { cause },
    );
  }
}

/** 从 Emscripten 实例导出中找线性内存；不依赖压缩后的导出名。 */
function findWasmMemory(instance: WebAssembly.Instance): WebAssembly.Memory | undefined {
  try {
    for (const value of Object.values(instance.exports)) {
      if (value instanceof WebAssembly.Memory) return value;
    }
  } catch {
    // 导出表不可读时监控降级为不可用，不影响启动。
  }
  return undefined;
}

/**
 * 把 loader 观测到的线性内存挂到模块对象上。
 *
 * 只作为只读监控数据：`memory.buffer.byteLength` 是当前容量，读取 O(1)；
 * 冻结/不可扩展的模块对象挂载失败时静默降级，不影响启动。
 */
function attachWasmMemory(module: PeriWasmModule, memory: WebAssembly.Memory | undefined): void {
  if (!memory) return;
  try {
    if (module.wasmMemory !== undefined) return;
    Object.defineProperty(module, "wasmMemory", {
      value: memory,
      enumerable: false,
      configurable: true,
    });
  } catch {
    // 宿主模块对象不可扩展；linearMemoryBytes 将返回 null。
  }
}

/**
 * Emscripten only accepts an instance through this callback and ignores the return value, so a
 * failed instantiation has no rejection channel into the factory promise. Log failures and let the
 * runtime's startup timeout bound the wait, like any other stalled glue startup.
 *
 * `successCallback` runs on a promise chain, so a synchronous throw from it would otherwise become
 * an unhandled rejection that can take down the host. Catch it, log it, and keep the process alive.
 * The factory cannot be rejected from here: Emscripten exposes no hook for that, and the startup
 * timeout is the only bound for a glue that keeps waiting.
 */
function instantiateSharedWasm(artifact: WebAssembly.Module,
  onInstance?: (instance: WebAssembly.Instance) => void) {
  return (imports: WebAssembly.Imports,
    successCallback: (instance: WebAssembly.Instance, module: WebAssembly.Module) => void) => {
    void WebAssembly.instantiate(artifact, imports).then(
      (instance) => {
        try {
          successCallback(instance, artifact);
          onInstance?.(instance);
        } catch (error) {
          console.error("[peri-sdk] Peri WASM instantiation callback failed", error);
        }
      },
    ).catch((error: unknown) => {
      console.error("[peri-sdk] Peri WASM instance creation failed", error);
    });
    return {};
  };
}

/**
 * Compile the binary once when this loader owns the artifact location.
 *
 * `locateFile` and `instantiateWasm` are runtime-owned choices about where and how the binary is
 * loaded; with either present the glue keeps its current path and the cache stays unused. A missing
 * binary, a failed read or a read that exceeds the artifact budget also resolves `null` so the glue
 * can fall back to its own loading path; the cache evicts that result so a later activation retries.
 */
async function loadSharedWasmModule(url: URL,
  moduleOptions: Readonly<Record<string, unknown>>): Promise<WebAssembly.Module | null> {
  if (moduleOptions.instantiateWasm !== undefined || moduleOptions.locateFile !== undefined) return null;
  return loadWasmArtifact(wasmArtifactUrl(url));
}

async function instantiate(url: URL | undefined, environment: Record<string, string>,
  moduleOptions: Readonly<Record<string, unknown>>,
  factory?: PeriWasmInstantiateOptions["moduleFactory"]): Promise<PeriWasmModule> {
  // 注入 factory 时产物位置归宿主所有，共享编译缓存不介入该路径。
  const loaderOwnsArtifact = factory === undefined;
  let resolved = factory;
  if (!resolved) {
    if (!url) throw new Error("Peri WASM artifact URL is unavailable; pass moduleUrl or inject moduleFactory");
    const source = await import(/* @vite-ignore */ url.href) as { default?: PeriWasmModuleFactory };
    resolved = source.default;
    if (typeof resolved !== "function") throw new Error("Invalid peri-wasm module factory");
  }
  if (typeof resolved !== "function") throw new Error("Invalid peri-wasm module factory");
  const hooks = moduleOptions.preRun;
  if (hooks !== undefined && typeof hooks !== "function" && !Array.isArray(hooks))
    throw new TypeError("Invalid WASM preRun hooks");
  const preRun = typeof hooks === "function" ? [hooks] : [...(hooks as unknown[] ?? [])];
  if (preRun.some((hook) => typeof hook !== "function")) throw new TypeError("Invalid WASM preRun hooks");
  if (Object.keys(environment).length)
    preRun.push((runtime: PeriWasmModule) => {
      if (!runtime.ENV) throw new Error("peri-wasm artifact does not export ENV; rebuild peri-wasm");
      Object.assign(runtime.ENV, environment);
    });
  const shared = loaderOwnsArtifact && url ? await loadSharedWasmModule(url, moduleOptions) : null;
  let wasmMemory: WebAssembly.Memory | undefined;
  let moduleValue: PeriWasmModule | undefined;
  const module = await resolved({
    ...moduleOptions,
    preRun,
    ...(shared ? {
      instantiateWasm: instantiateSharedWasm(shared, (instance) => {
        wasmMemory = findWasmMemory(instance);
        if (moduleValue) attachWasmMemory(moduleValue, wasmMemory);
      }),
    } : {}),
  });
  if (typeof module.PeriWasmAcp?.start !== "function")
    throw new Error("peri-wasm artifact does not expose the ACP Host");
  moduleValue = module;
  attachWasmMemory(module, wasmMemory);
  return module;
}

/**
 * Build a fresh module instance for one runtime. The compiled artifact may be shared by URL, but
 * each caller gets its own module object, `ENV` and ACP Host; nothing else is shared.
 *
 * 环境在调用时同步取快照，失败一律以 reject 结算（`loadPeriWasm` 的校验失败仍是同步抛出）。
 */
export function instantiatePeriWasm(options: PeriWasmInstantiateOptions = {}): Promise<PeriWasmModule> {
  let environment: Record<string, string>;
  let url: URL | undefined;
  try {
    environment = normalizeEnvironment(options.env);
    // 注入 factory 时产物位置归宿主所有：此处不得解析 moduleUrl，否则没有 import.meta.url 的宿主会失败。
    url = options.moduleFactory ? undefined : artifactUrl(options.moduleUrl);
  } catch (error) {
    return Promise.reject(error);
  }
  return instantiate(url, environment, options.moduleOptions ?? {}, options.moduleFactory);
}

/** Load the WASM artifact bundled next to dist/index.js. One module instance is shared per URL. */
export function loadPeriWasm(moduleUrl?: string | URL, env?: Readonly<Record<string, string>>): Promise<PeriWasmModule> {
  const key = artifactUrl(moduleUrl).href;
  const environment = normalizeEnvironment(env);
  let loading = modules.get(key);
  if (loading && env !== undefined && JSON.stringify(environments.get(key)) !== JSON.stringify(environment))
    throw new Error("WASM module environment is fixed at initialization; use a separate module URL for another environment");
  if (!loading) {
    environments.set(key, environment);
    loading = instantiate(new URL(key), environment, {}).catch((error: unknown) => {
      modules.delete(key);
      environments.delete(key);
      throw error;
    });
    modules.set(key, loading);
  }
  return loading;
}
