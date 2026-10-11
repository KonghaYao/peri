import Module from './peri-time-emscripten-smoke.js';
import wasmModule from './peri_time_emscripten_smoke.wasm';

let loaded;
export default {
  async fetch() {
    try {
      loaded ??= Module({
        mainScriptUrlOrBlob: 'file:///bundle/peri-time-emscripten-smoke.js',
        instantiateWasm(imports, receiveInstance) {
          WebAssembly.instantiate(wasmModule, imports).then(receiveInstance);
        },
      });
      const wasm = await loaded;
      const result = await wasm.run_smoke();
      if (result !== 'ok') throw new Error(`unexpected result: ${result}`);
      return new Response('peri-time workerd smoke: ok');
    } catch (error) {
      return new Response(String(error?.stack ?? error), { status: 500 });
    }
  },
};
