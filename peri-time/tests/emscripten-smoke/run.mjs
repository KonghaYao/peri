import Module from './target/wasm32-unknown-emscripten/debug/peri-time-emscripten-smoke.js';

const wasm = await Module();
const result = await wasm.run_smoke();
if (result !== 'ok') throw new Error(`unexpected result: ${result}`);

const originalSet = globalThis.setTimeout;
const originalClear = globalThis.clearTimeout;
const scheduled = [];
const cleared = new Set();
globalThis.setTimeout = (callback, ms, ...args) => {
  const id = originalSet(callback, ms, ...args);
  scheduled.push([Number(id), ms]);
  return id;
};
globalThis.clearTimeout = (id) => {
  cleared.add(id);
  return originalClear(id);
};
try {
  wasm.test_timer_edges();
} finally {
  globalThis.setTimeout = originalSet;
  globalThis.clearTimeout = originalClear;
}
const edges = scheduled.slice(-2);
if (edges.length !== 2 || edges[0][1] !== 1 || edges[1][1] !== 2147483647 || edges.some(([id]) => !cleared.has(id))) {
  throw new Error(`timer edge/cancellation mismatch: ${JSON.stringify({ edges, cleared: [...cleared] })}`);
}
console.log('peri-time emscripten smoke: ok');
