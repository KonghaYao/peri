import Module from "./generated/peri-wasm.js";
import wasmModule from "./generated/peri_wasm.wasm";
import { wasmConfig } from "./config";
import type { AcpTransport, Env } from "../types";
import { workerDnsPort } from "./dns";
import { WasmResources } from "./resources";
import { isolateWasmBudget, type WasmBudget } from "./budget";
import {
  PeriWasmHostStartupError, createNodeNetworkPort, createNodeSchedulerPort, startPeriWasmHost,
  type PeriWasmHostLifecycleEvent, type PeriWasmModule,
} from "../sdk";
import { CF_WASM_CLEANUP_TIMEOUT_MS } from "../../shared/runtime-limits";
import { logError } from "../api/http";

/**
 * Platform wiring for the SDK-owned ACP host: CF build profile, Workers module/glue injection and the
 * isolate capacity reservation. Startup ordering, callback/socket ownership and close evidence stay
 * in `startPeriWasmHost`; resource observation only reports what the SDK reports.
 */
export interface WasmHostWiring {
  /** Test seam for the SDK-owned host startup. */
  startHost: typeof startPeriWasmHost;
  /** Test seam for the Emscripten module factory injected into the SDK host. */
  loadModule: (options: Readonly<Record<string, unknown>>, resources: WasmResources,
    signal?: AbortSignal) => Promise<PeriWasmModule>;
  /** Isolate capacity policy; reservations are held until the SDK reports confirmed cleanup. */
  budget: WasmBudget;
}

const instantiateModule: WasmHostWiring["loadModule"] = (options, resources, signal) =>
  new Promise<PeriWasmModule>((resolve, reject) => {
    Module({
      ...options,
      mainScriptUrlOrBlob: "file:///bundle/peri-wasm.js",
      instantiateWasm(imports: WebAssembly.Imports,
        receiveInstance: (instance: WebAssembly.Instance, module: WebAssembly.Module) => void) {
        WebAssembly.instantiate(wasmModule, imports)
          .then((instance) => {
            if (signal?.aborted) throw signal.reason;
            resources.attach(instance);
            receiveInstance(instance, wasmModule);
          }).catch(reject);
      },
    }).then((module) => resolve(module as PeriWasmModule), reject);
  });

export async function startTransport(env: Env, resources = new WasmResources(), signal?: AbortSignal,
  wiring: Partial<WasmHostWiring> = {}): Promise<AcpTransport> {
  const startHost = wiring.startHost ?? startPeriWasmHost;
  const loadModule = wiring.loadModule ?? instantiateModule;
  const budget = wiring.budget ?? isolateWasmBudget;
  let config: string;
  try { config = wasmConfig(env); }
  catch (error) {
    resources.failed();
    logError("Peri WASM configuration failed", error, { instanceId: resources.instanceId });
    throw new PeriWasmHostStartupError(error, Promise.resolve({ confirmed: true }));
  }
  let lease: ReturnType<WasmBudget["acquire"]>;
  try { lease = budget.acquire(); }
  catch (error) {
    resources.failed();
    logError("Peri WASM admission failed", error, { instanceId: resources.instanceId });
    throw new PeriWasmHostStartupError(error, Promise.resolve({ confirmed: true }));
  }
  const startedAt = performance.now();
  const elapsedMs = (): number => Math.round(performance.now() - startedAt);
  let generationId: string | undefined;
  // The reservation is released only where the SDK reports cleanup as confirmed.
  const apply = (event: PeriWasmHostLifecycleEvent): void => {
    if (event.type === "phase") {
      resources.markStartup(event.phase, event.elapsedMs);
      console.info("Peri WASM startup", { stage: event.phase, elapsedMs: event.elapsedMs });
      return;
    }
    if (event.type === "ready") { generationId = event.generationId; resources.ready(event.generationId); return; }
    if (event.type === "closing") { resources.closing(); return; }
    if (event.type === "closed") { resources.closed(); lease.releaseAfterCleanup(); return; }
    if (event.type === "failed") { resources.failed(); lease.releaseAfterCleanup(); return; }
    resources.closeUnconfirmed();
  };
  const transport = await startHost({
    configJson: config,
    signal,
    cleanupTimeoutMs: CF_WASM_CLEANUP_TIMEOUT_MS,
    ports: {
      dns: workerDnsPort(env.PERI_DNS_OVERRIDES),
      network: createNodeNetworkPort(),
      scheduler: createNodeSchedulerPort(),
      workerSockets: true,
    },
    moduleFactory: (options) => loadModule(options, resources, signal),
    onLifecycle: apply,
    onDiagnostic: (kind, error) => logError(
      kind === "startup" ? "Peri WASM startup failed" : "Peri WASM ownership cleanup unconfirmed",
      error, { instanceId: resources.instanceId, generationId }),
  });
  resources.markStartup("acp-ready", elapsedMs());
  console.info("Peri WASM startup", { stage: "acp-ready", elapsedMs: elapsedMs() });
  return transport;
}
