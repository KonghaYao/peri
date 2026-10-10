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
   * 静态导入的 Emscripten factory；注入时宿主已拥有产物位置，`moduleUrl` 既不解析也不要求可解析。
   * workerd 打包模块的 `import.meta.url` 是 `undefined`，这类宿主只能走本入口。
   */
  moduleFactory?: (options: Record<string, unknown>) => Promise<PeriWasmModule>;
  /** Host-owned Emscripten options (network, timers, DNS, `instantiateWasm`) for this instance. */
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

/** 解析产物位置；失败时给出宿主可执行的结论，而不是裸 `TypeError: Invalid URL string`。 */
function artifactUrl(moduleUrl?: string | URL): URL {
  const specifier = String(moduleUrl ?? DEFAULT_ARTIFACT);
  try {
    return new URL(specifier, import.meta.url);
  } catch (cause) {
    throw new Error(
      `Cannot resolve the Peri WASM artifact URL from ${JSON.stringify(specifier)}: this runtime does not ` +
      `provide a usable import.meta.url (workerd reports undefined for bundled modules). Pass an absolute ` +
      `artifact URL, or inject moduleFactory so the host owns the artifact location.`,
      { cause },
    );
  }
}

async function instantiate(url: URL | undefined, environment: Record<string, string>,
  moduleOptions: Readonly<Record<string, unknown>>,
  factory?: PeriWasmInstantiateOptions["moduleFactory"]): Promise<PeriWasmModule> {
  if (!factory) {
    if (!url) throw new Error("Peri WASM artifact URL is unavailable; pass moduleUrl or inject moduleFactory");
    const source = await import(/* @vite-ignore */ url.href) as { default?: PeriWasmModuleFactory };
    factory = source.default;
  }
  if (typeof factory !== "function") throw new Error("Invalid peri-wasm module factory");
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
  const module = await factory({ ...moduleOptions, preRun });
  if (typeof module.PeriWasmAcp?.start !== "function")
    throw new Error("peri-wasm artifact does not expose the ACP Host");
  return module;
}

/**
 * Build a fresh module instance for one host; nothing is shared with other instances or URLs.
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
