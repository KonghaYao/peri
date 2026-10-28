/**
 * Process-wide cache for the compiled Peri WASM artifact.
 *
 * Emscripten can instantiate one `WebAssembly.Module` repeatedly, but JavaScript engines do not
 * reliably share compiled code between separate `WebAssembly.compile` calls (JSC recompiles each
 * time). Keying the module by its resolved artifact URL keeps the compile one-time per process
 * while every runtime still gets its own instance.
 */
const artifacts = new Map<string, Promise<WebAssembly.Module | null>>();

/**
 * Budget for one artifact read (request plus body). Kept below the runtime startup budget
 * (`budget.ts`'s 20s upper bound) so a stalled network read fails over before the caller's
 * deadline; that deadline remains the final veto and is not replaced by this value.
 */
export const ARTIFACT_READ_TIMEOUT_MS = 15_000;

/** Resolve the binary Emscripten loads beside the glue module; mirrors its `findWasmBinary`. */
export function wasmArtifactUrl(moduleUrl: URL): URL {
  return new URL("peri_wasm.wasm", moduleUrl);
}

/**
 * Read and compile the artifact once per URL.
 *
 * The cached promise is the single-flight slot: concurrent callers share one read and one compile.
 * Read failures (including a timed-out read) resolve `null` and are evicted so the caller can fall
 * back to the glue's own loading path; compile failures reject and are evicted so the next
 * activation retries instead of caching a broken module. Nothing that never settles stays cached:
 * a timed-out fetch is aborted, and its entry leaves the map before callers observe `null`.
 *
 * `readTimeoutMs` exists for tests to exercise the timeout without waiting the production budget;
 * it is not part of the published API.
 */
export function loadWasmArtifact(url: URL,
  readTimeoutMs: number = ARTIFACT_READ_TIMEOUT_MS): Promise<WebAssembly.Module | null> {
  const key = url.href;
  const cached = artifacts.get(key);
  if (cached) return cached;
  const loading = readWasmArtifact(key, readTimeoutMs).then(
    (module) => {
      if (module === null) artifacts.delete(key);
      return module;
    },
    (error: unknown) => {
      artifacts.delete(key);
      throw error;
    },
  );
  artifacts.set(key, loading);
  return loading;
}

async function readWasmArtifact(url: string, readTimeoutMs: number): Promise<WebAssembly.Module | null> {
  let bytes: ArrayBuffer;
  try {
    // The signal also covers body consumption, so a server that sends headers and then stalls
    // still aborts the read instead of hanging the shared cache slot forever.
    const response = await fetch(url, { signal: AbortSignal.timeout(readTimeoutMs) });
    if (!response.ok) return null;
    bytes = await response.arrayBuffer();
  } catch {
    // Locating or reading the binary is optional: the glue can still load it itself. Timeouts
    // land here too; the caller evicts the slot before resolving `null`, so the next activation
    // starts a fresh read.
    return null;
  }
  // Compilation errors are not read failures; the caller must see them instead of silently
  // falling back to a second compile path.
  return WebAssembly.compile(bytes);
}
