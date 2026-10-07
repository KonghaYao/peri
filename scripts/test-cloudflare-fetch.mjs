import { access } from 'node:fs/promises';

const profile = process.env.PERI_WASM_PROFILE ?? 'release';
if (!/^[a-zA-Z0-9_-]+$/.test(profile)) throw new Error('Invalid PERI_WASM_PROFILE');
const artifact = new URL(`../target/wasm32-unknown-emscripten/${profile}/examples/cloudflare_contract.js`, import.meta.url);
try {
  await access(artifact);
} catch (error) {
  throw new Error('Build the harness first: EMSDK_PYTHON=$(command -v python3) ./scripts/cargo-wasm.sh build --locked --release --target wasm32-unknown-emscripten -p peri-model --features cloudflare --example cloudflare-contract', { cause: error });
}

const deadline = setTimeout(() => {
  console.error('Native fetch WASM contracts exceeded the 30-second deadline');
  process.exit(1);
}, 30_000);
try {
  const { default: Module } = await import(artifact.href);
  const wasm = await Module();
  if (typeof wasm.runNativeFetchContracts !== 'function') throw new Error('WASM artifact is missing the native fetch contract entry point');
  const contracts = JSON.parse(await wasm.runNativeFetchContracts());
  if (!Array.isArray(contracts) || !contracts.length || contracts.some((name) => typeof name !== 'string')
    || new Set(contracts).size !== contracts.length) throw new Error('Invalid or empty native fetch contract results');
  console.log(JSON.stringify({ passed: contracts.length, contracts }));
} finally {
  clearTimeout(deadline);
}
