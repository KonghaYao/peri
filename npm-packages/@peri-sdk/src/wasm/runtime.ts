import { WasmAcpTransport } from "./transport";
import { DeadlineError, withDeadline } from "../util/deadline";
import { instantiatePeriWasm, type PeriWasmModule } from "./loader";
import type { WasmDnsPort, WasmNetworkPort } from "./ports";
import type { WasmSchedulerPort } from "./scheduler";

/**
 * Owned Emscripten ACP runtime startup for platform-neutral runtimes (browsers, Workers with
 * `nodejs_compat`, Node): only `fetch`, timers and WebAssembly are required; sockets and DNS are
 * optional injected ports.
 *
 * The SDK owns startup ordering, callback/socket ownership and close evidence; the embedder owns
 * the platform module factory, ports and the capacity policy around them. A runtime is only
 * reported as cleaned up after `native.close()`, port cleanup and `native.free()` all settled.
 */

export type WasmRuntimePhase = "module-ready" | "native-ready";

export type WasmRuntimeLifecycleEvent =
  | { type: "phase"; phase: WasmRuntimePhase; elapsedMs: number }
  | { type: "ready"; generationId: string }
  | { type: "closing" }
  | { type: "closed" }
  | { type: "failed" }
  | { type: "close-unconfirmed"; error: unknown };

export type WasmRuntimeDiagnosticKind = "startup" | "cleanup-unconfirmed";

export interface WasmRuntimeCleanupOutcome {
  confirmed: boolean;
  error?: unknown;
}

/** Startup failed; the owner must still await `cleanup` or `eventualCleanup` before reusing capacity. */
export class WasmRuntimeStartupError extends Error {
  constructor(cause: unknown, readonly cleanup: Promise<WasmRuntimeCleanupOutcome>,
    readonly eventualCleanup: Promise<WasmRuntimeCleanupOutcome> = cleanup) {
    super("Peri WASM runtime startup failed; ownership cleanup must be checked", { cause });
  }
}

export interface WasmRuntimePorts {
  dns?: WasmDnsPort;
  network?: WasmNetworkPort;
  scheduler?: WasmSchedulerPort;
  /** Route the artifact's Node socket emulation through the injected network port instead of binding locally. */
  workerSockets?: boolean;
}

export interface WasmRuntimeOptions {
  configJson: string;
  moduleUrl?: string | URL;
  env?: Readonly<Record<string, string>>;
  /**
   * Runtime-owned module factory; the default loads the packaged artifact at `moduleUrl`.
   * 注入时不解析 `moduleUrl`：workerd 打包模块没有 `import.meta.url`，只能走本入口。
   */
  moduleFactory?: (options: Readonly<Record<string, unknown>>) => Promise<PeriWasmModule>;
  /** Extra Emscripten options merged with the injected ports. */
  moduleOptions?: Readonly<Record<string, unknown>>;
  ports?: WasmRuntimePorts;
  signal?: AbortSignal;
  cleanupTimeoutMs?: number;
  onLifecycle?: (event: WasmRuntimeLifecycleEvent) => void;
  onDiagnostic?: (kind: WasmRuntimeDiagnosticKind, error: unknown) => void;
}

export const DEFAULT_WASM_RUNTIME_CLEANUP_TIMEOUT_MS = 20_000;

function runtimeModuleOptions(options: WasmRuntimeOptions): Record<string, unknown> {
  const ports = options.ports ?? {};
  const merged: Record<string, unknown> = { ...options.moduleOptions };
  if (ports.dns) merged.periDns = ports.dns;
  if (ports.network) merged.periNet = ports.network.module;
  if (ports.scheduler) {
    merged.periSetImmediate = ports.scheduler.schedule;
    merged.periClearImmediate = ports.scheduler.cancel;
    merged.periSetTimeout = ports.scheduler.schedule;
    merged.periClearTimeout = ports.scheduler.cancel;
  }
  if (ports.workerSockets !== undefined) merged.periWorkerSockets = ports.workerSockets;
  return merged;
}

export async function startWasmRuntime(options: WasmRuntimeOptions): Promise<WasmAcpTransport> {
  const startedAt = performance.now();
  const elapsed = (): number => Math.round(performance.now() - startedAt);
  const notify = (event: WasmRuntimeLifecycleEvent): void => options.onLifecycle?.(event);
  const diagnose = (kind: WasmRuntimeDiagnosticKind, error: unknown): void => options.onDiagnostic?.(kind, error);
  const signal = options.signal;
  const network = options.ports?.network;
  const scheduler = options.ports?.scheduler;
  const timeout = options.cleanupTimeoutMs ?? DEFAULT_WASM_RUNTIME_CLEANUP_TIMEOUT_MS;

  if (signal?.aborted) {
    notify({ type: "failed" });
    throw new WasmRuntimeStartupError(signal.reason, Promise.resolve({ confirmed: true }));
  }

  let native: Awaited<ReturnType<PeriWasmModule["PeriWasmAcp"]["start"]>> | undefined;
  let transport: WasmAcpTransport | undefined;
  let closed = false;
  let nativeStartupIssued = false;
  let closeNative: Promise<void> | undefined;
  let actualNativeClose: Promise<void> | undefined;
  let cleanupFailure: unknown;

  const stopOwnedPorts = (): void => {
    scheduler?.close();
    network?.close();
  };
  const recordCleanupFailure = (error: unknown, permanent = false): void => {
    if (permanent) cleanupFailure = error;
    notify({ type: "close-unconfirmed", error });
    diagnose("cleanup-unconfirmed", error);
  };
  const closePort = (): Promise<void> => {
    if (!closeNative) {
      notify({ type: "closing" });
      actualNativeClose = Promise.resolve().then(async () => {
        await native!.close();
        network?.close();
        await network?.drain();
      });
      closeNative = withDeadline(actualNativeClose, timeout, "Peri WASM native runtime and network close")
        .catch((error) => {
          try { stopOwnedPorts(); }
          catch (ownedError) { error = new AggregateError([error, ownedError], "Peri WASM runtime and callback cleanup failed"); }
          recordCleanupFailure(error, !(error instanceof DeadlineError));
          if (error instanceof DeadlineError)
            void actualNativeClose!.then(freePort).catch((lateError) => recordCleanupFailure(lateError, true));
          throw error;
        });
    }
    return closeNative;
  };
  const freePort = (): void => {
    if (closed) return;
    if (cleanupFailure) throw cleanupFailure;
    try {
      native!.free?.();
      stopOwnedPorts();
      closed = true;
      notify({ type: "closed" });
      native = undefined;
    } catch (error) {
      try { stopOwnedPorts(); }
      catch (ownedError) { error = new AggregateError([error, ownedError], "Peri WASM free and callback cleanup failed"); }
      recordCleanupFailure(error, true);
      throw error;
    }
  };
  const finishWithoutNative = async (): Promise<void> => {
    if (closed) return;
    stopOwnedPorts();
    await network?.drain();
    closed = true;
    notify({ type: "failed" });
  };

  const starting = Promise.resolve().then(async (): Promise<WasmAcpTransport> => {
    if (signal?.aborted) throw signal.reason;
    const moduleOptions = runtimeModuleOptions(options);
    const module = await instantiatePeriWasm({
      moduleUrl: options.moduleUrl, env: options.env, moduleOptions,
      moduleFactory: options.moduleFactory,
    });
    notify({ type: "phase", phase: "module-ready", elapsedMs: elapsed() });
    if (signal?.aborted) throw signal.reason;
    nativeStartupIssued = true;
    native = await module.PeriWasmAcp.start(options.configJson);
    notify({ type: "phase", phase: "native-ready", elapsedMs: elapsed() });
    if (signal?.aborted) throw signal.reason;
    transport = WasmAcpTransport.fromPort({
      send: (frame) => native!.send(frame), recv: () => native!.recv(), close: closePort, free: freePort,
    });
    notify({ type: "ready", generationId: transport.generationId });
    return transport;
  });

  let abortListener: (() => void) | undefined;
  const aborted = new Promise<never>((_, reject) => {
    if (!signal) return;
    abortListener = () => {
      if (!nativeStartupIssued) scheduler?.close();
      try { network?.close(); }
      catch (error) { recordCleanupFailure(error); }
      reject(signal.reason ?? new Error("Peri WASM startup aborted"));
    };
    signal.addEventListener("abort", abortListener, { once: true });
    if (signal.aborted) abortListener();
  });

  try {
    return await Promise.race([starting, aborted]);
  } catch (error) {
    diagnose("startup", error);
    notify({ type: "closing" });
    const finishing = starting.then(async (late) => {
      try { await late.close(); }
      catch (closeError) {
        if (!(closeError instanceof DeadlineError)) throw closeError;
        await actualNativeClose;
        freePort();
      }
    }, async () => {
      if (native) {
        try { await closePort(); }
        catch (closeError) {
          if (!(closeError instanceof DeadlineError)) throw closeError;
          await actualNativeClose;
        }
        freePort();
      } else if (nativeStartupIssued) {
        stopOwnedPorts();
        throw new Error("Native startup rejected after execution began; no runtime exit proof is available", { cause: error });
      } else await finishWithoutNative();
    });
    const eventualCleanup = finishing.then(
      (): WasmRuntimeCleanupOutcome => ({ confirmed: closed }),
      (cleanupError): WasmRuntimeCleanupOutcome => {
        recordCleanupFailure(cleanupError);
        return { confirmed: false, error: cleanupError };
      },
    );
    const cleanup = withDeadline(eventualCleanup, timeout, "Peri WASM startup ownership cleanup").catch(
      (cleanupError): WasmRuntimeCleanupOutcome => {
        try { stopOwnedPorts(); }
        catch (ownedError) {
          cleanupError = new AggregateError([cleanupError, ownedError], "Peri WASM startup and callback cleanup unconfirmed");
        }
        recordCleanupFailure(cleanupError);
        return { confirmed: false, error: cleanupError };
      },
    );
    throw new WasmRuntimeStartupError(error, cleanup, eventualCleanup);
  } finally {
    if (abortListener) signal?.removeEventListener("abort", abortListener);
  }
}
