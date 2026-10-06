import { WasmAcpTransport } from "../sdk";
import Module from "./generated/peri-wasm.js";
import wasmModule from "./generated/peri_wasm.wasm";
import { wasmConfig } from "./config";
import type { AcpTransport, Env } from "../types";

interface WasmModule {
  PeriWasmAcp: {
    start(configJson: string): Promise<{
      send(frame: string): Promise<void>;
      recv(): Promise<string | null | undefined>;
      close(): Promise<void>;
      free(): void;
    }>;
  };
}

let loaded: Promise<WasmModule> | undefined;

export async function startTransport(env: Env): Promise<AcpTransport> {
  const config = wasmConfig(env);
  loaded ??= new Promise<WasmModule>((resolve, reject) => {
    Module({
      mainScriptUrlOrBlob: "file:///bundle/peri-wasm.js",
      instantiateWasm(imports: WebAssembly.Imports, receiveInstance: (instance: WebAssembly.Instance, module: WebAssembly.Module) => void) {
        WebAssembly.instantiate(wasmModule, imports)
          .then((instance) => receiveInstance(instance, wasmModule)).catch(reject);
      },
    }).then(resolve, reject);
  }).catch((error: unknown) => {
    loaded = undefined;
    throw error;
  });
  const wasm = await loaded;
  const native = await wasm.PeriWasmAcp.start(config);
  return WasmAcpTransport.fromPort(native);
}
