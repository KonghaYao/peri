import { WasmAcpTransport } from "../sdk";
import { CF_WASM_CLEANUP_TIMEOUT_MS } from "../../shared/runtime-limits";
import { DeadlineError, withDeadline } from "../chat/deadline";
import { logError } from "../api/http";
import { isolateWasmBudget, type WasmBudget } from "./budget";
import { workerNetwork } from "./network";
import { workerScheduler } from "./scheduler";
import { WasmResources } from "./resources";

export interface NativeWasmHost {
  send(frame: string): Promise<void>;
  recv(): Promise<string | null | undefined>;
  close(): Promise<void>;
  free(): void;
}

export interface StartupCleanupOutcome {
  confirmed: boolean;
  error?: unknown;
}

export class WasmStartupError extends Error {
  constructor(cause: unknown, readonly cleanup: Promise<StartupCleanupOutcome>,
    readonly eventualCleanup: Promise<StartupCleanupOutcome> = cleanup) {
    super("WASM host startup failed; ownership cleanup must be checked", { cause });
  }
}

export interface WasmHostOwnership {
  network: ReturnType<typeof workerNetwork>;
  scheduler: ReturnType<typeof workerScheduler>;
  resources: WasmResources;
  signal?: AbortSignal;
}

export interface WasmHostStartup {
  config: string;
  load(ownership: WasmHostOwnership): Promise<{ PeriWasmAcp: { start(config: string): Promise<NativeWasmHost> } }>;
  resources?: WasmResources;
  signal?: AbortSignal;
  budget?: WasmBudget;
  cleanupTimeoutMs?: number;
}

export async function startOwnedWasmHost(options: WasmHostStartup): Promise<WasmAcpTransport> {
  const resources = options.resources ?? new WasmResources();
  const signal = options.signal;
  if (signal?.aborted) {
    resources.failed();
    throw new WasmStartupError(signal.reason, Promise.resolve({ confirmed: true }));
  }
  let lease: ReturnType<WasmBudget["acquire"]>;
  try { lease = (options.budget ?? isolateWasmBudget).acquire(); }
  catch (error) {
    resources.failed();
    logError("Peri WASM admission failed", error, { instanceId: resources.instanceId });
    throw new WasmStartupError(error, Promise.resolve({ confirmed: true }));
  }
  const ownership: WasmHostOwnership = {
    network: workerNetwork(), scheduler: workerScheduler(), resources, signal,
  };
  const timeout = options.cleanupTimeoutMs ?? CF_WASM_CLEANUP_TIMEOUT_MS;
  let native: NativeWasmHost | undefined;
  let transport: WasmAcpTransport | undefined;
  let closed = false;
  let nativeStartupIssued = false;
  let closeNative: Promise<void> | undefined;
  let actualNativeClose: Promise<void> | undefined;
  let cleanupFailure: unknown;
  const stopOwnedCallbacks = (): void => {
    ownership.scheduler.close();
    ownership.network.close();
  };
  const recordCleanupFailure = (error: unknown, permanent = false): void => {
    if (permanent) cleanupFailure = error;
    resources.closeUnconfirmed();
    logError("Peri WASM ownership cleanup unconfirmed", error, {
      instanceId: resources.instanceId, generationId: transport?.generationId,
    });
  };
  const closePort = (): Promise<void> => {
    if (!closeNative) {
      resources.closing();
      actualNativeClose = Promise.resolve().then(async () => {
        await native!.close();
        ownership.network.close();
        await ownership.network.drain();
      });
      closeNative = withDeadline(actualNativeClose, timeout, "WASM native host and network close")
        .catch((error) => {
          try { stopOwnedCallbacks(); }
          catch (ownedError) { error = new AggregateError([error, ownedError], "WASM host and callback cleanup failed"); }
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
      native!.free();
      stopOwnedCallbacks();
      closed = true;
      resources.closed();
      native = undefined;
      lease.releaseAfterCleanup();
    } catch (error) {
      try { stopOwnedCallbacks(); }
      catch (ownedError) { error = new AggregateError([error, ownedError], "WASM free and callback cleanup failed"); }
      recordCleanupFailure(error, true);
      throw error;
    }
  };
  const finishWithoutNative = async (): Promise<void> => {
    if (closed) return;
    stopOwnedCallbacks();
    await ownership.network.drain();
    closed = true;
    resources.failed();
    lease.releaseAfterCleanup();
  };
  const starting = Promise.resolve().then(async () => {
    if (signal?.aborted) throw signal.reason;
    const module = await options.load(ownership);
    if (signal?.aborted) throw signal.reason;
    nativeStartupIssued = true;
    native = await module.PeriWasmAcp.start(options.config);
    if (signal?.aborted) throw signal.reason;
    transport = WasmAcpTransport.fromPort({
      send: (frame) => native!.send(frame), recv: () => native!.recv(), close: closePort, free: freePort,
    });
    resources.ready(transport.generationId);
    return transport;
  });
  let abortListener: (() => void) | undefined;
  const aborted = new Promise<never>((_, reject) => {
    if (!signal) return;
    abortListener = () => {
      if (!nativeStartupIssued) ownership.scheduler.close();
      try { ownership.network.close(); }
      catch (error) { recordCleanupFailure(error); }
      reject(signal.reason ?? new Error("WASM startup aborted"));
    };
    signal.addEventListener("abort", abortListener, { once: true });
    if (signal.aborted) abortListener();
  });
  try {
    return await Promise.race([starting, aborted]);
  } catch (error) {
    logError("Peri WASM startup failed", error, { instanceId: resources.instanceId, generationId: transport?.generationId });
    resources.closing();
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
        stopOwnedCallbacks();
        throw new Error("Native startup rejected after execution began; no host exit proof is available", { cause: error });
      } else await finishWithoutNative();
    });
    const eventualCleanup = finishing.then(
      (): StartupCleanupOutcome => ({ confirmed: closed }),
      (cleanupError): StartupCleanupOutcome => {
        recordCleanupFailure(cleanupError);
        return { confirmed: false, error: cleanupError };
      },
    );
    const cleanup = withDeadline(eventualCleanup, timeout, "WASM startup ownership cleanup").catch(
      (cleanupError): StartupCleanupOutcome => {
        try { stopOwnedCallbacks(); }
        catch (ownedError) { cleanupError = new AggregateError([cleanupError, ownedError], "WASM startup and callback cleanup unconfirmed"); }
        recordCleanupFailure(cleanupError);
        return { confirmed: false, error: cleanupError };
      },
    );
    throw new WasmStartupError(error, cleanup, eventualCleanup);
  } finally {
    if (abortListener) signal?.removeEventListener("abort", abortListener);
  }
}
