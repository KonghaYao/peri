import Module from "./generated/peri-wasm.js";
import wasmModule from "./generated/peri_wasm.wasm";
import { wasmConfig } from "./config";
import type { AcpTransport, Env } from "../types";
import { workerDns } from "./dns";
import { WasmResources } from "./resources";
import { startOwnedWasmHost, WasmStartupError, type NativeWasmHost } from "./lifecycle";
import { logError } from "../api/http";

interface WasmModule {
  PeriWasmAcp: {
    start(configJson: string): Promise<NativeWasmHost>;
  };
}

export async function startTransport(env: Env, resources = new WasmResources(), signal?: AbortSignal): Promise<AcpTransport> {
  let config: string;
  try { config = wasmConfig(env); }
  catch (error) {
    resources.failed();
    logError("Peri WASM configuration failed", error, { instanceId: resources.instanceId });
    throw new WasmStartupError(error, Promise.resolve({ confirmed: true }));
  }
  return startOwnedWasmHost({ config, resources, signal, load: (ownership) => new Promise<WasmModule>((resolve, reject) => {
    console.info("Peri WASM startup", { stage: "module-loading" });
    Module({
      periDns: workerDns(env.PERI_DNS_OVERRIDES),
      periWorkerSockets: true,
      periNet: ownership.network.module,
      periSetImmediate: ownership.scheduler.schedule,
      periClearImmediate: ownership.scheduler.cancel,
      periSetTimeout: ownership.scheduler.schedule,
      periClearTimeout: ownership.scheduler.cancel,
      mainScriptUrlOrBlob: "file:///bundle/peri-wasm.js",
      instantiateWasm(imports: WebAssembly.Imports, receiveInstance: (instance: WebAssembly.Instance, module: WebAssembly.Module) => void) {
        WebAssembly.instantiate(wasmModule, imports)
          .then((instance) => {
            if (signal?.aborted) throw signal.reason;
            resources.attach(instance);
            receiveInstance(instance, wasmModule);
          }).catch(reject);
      },
    }).then(resolve, reject);
  }) });
}
