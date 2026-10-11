import type { PeriWasmModule } from "@peri-code/sdk/wasm";
import Module from "../wasm/generated/peri-wasm.js";
import wasmUrl from "../wasm/generated/peri_wasm.wasm?url";

/**
 * 浏览器 Emscripten 模块工厂：wasm 二进制由 Vite 作为同源资源提供，实例化由宿主显式接管，
 * 不进入 Emscripten 自带的 locateFile/instantiateAsync 探测（其 Node 分支在浏览器不可用）。
 * 编译结果在页面内复用，宿主实例之间互不共享状态。
 */

let compiled: Promise<WebAssembly.Module> | undefined;

function compiledWasm(): Promise<WebAssembly.Module> {
  compiled ??= fetch(wasmUrl).then(async (response) => {
    if (!response.ok) throw new Error(`Failed to load peri_wasm.wasm: HTTP ${response.status}`);
    return WebAssembly.compile(await response.arrayBuffer());
  }).catch((error: unknown) => {
    compiled = undefined;
    throw error;
  });
  return compiled;
}

export function loadPeriWasmModule(options: Readonly<Record<string, unknown>>,
  signal?: AbortSignal): Promise<PeriWasmModule> {
  return new Promise<PeriWasmModule>((resolve, reject) => {
    Module({
      ...options,
      instantiateWasm(imports: WebAssembly.Imports,
        receiveInstance: (instance: WebAssembly.Instance, module: WebAssembly.Module) => void) {
        compiledWasm()
          .then(async (module) => {
            if (signal?.aborted) throw signal.reason;
            return { module, instance: await WebAssembly.instantiate(module, imports) };
          })
          .then(({ module, instance }) => receiveInstance(instance, module))
          .catch(reject);
      },
    }).then((module) => resolve(module as PeriWasmModule), reject);
  });
}
