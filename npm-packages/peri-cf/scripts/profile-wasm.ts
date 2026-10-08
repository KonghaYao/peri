import { readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { brotliCompressSync, gzipSync } from "node:zlib";
import { buildInputs, verifyArtifacts, withDirectoryLock } from "./wasm-provenance";
import { WASM_PROFILES, wasmMemory } from "./wasm-profile";
import { toolchainVersions } from "./wasm-tools";

export async function profileReport(packageRoot: string) {
  const repoRoot = resolve(packageRoot, "../..");
  const versions = await toolchainVersions(repoRoot);
  return withDirectoryLock(resolve(packageRoot, ".cache/wasm/build.lock"), async () => {
    const rows = [];
    for (const profile of WASM_PROFILES) {
      const root = resolve(packageRoot, ".cache/wasm", profile);
      const manifest = await verifyArtifacts(root, await buildInputs(repoRoot, profile, versions));
      const wasm = await readFile(resolve(root, "peri_wasm.wasm"));
      rows.push({ profile, buildId: manifest.buildId, cargoProfile: manifest.inputs.settings.cargoProfile,
        wasmBytes: wasm.length, jsBytes: (await readFile(resolve(root, "peri-wasm.js"))).length,
        gzipWasmBytes: gzipSync(wasm).length, brotliWasmBytes: brotliCompressSync(wasm).length,
        buildWallMs: manifest.buildWallMs, memory: wasmMemory(wasm) });
    }
    return { note: "Size and observed build wall time only; runtime CPU, latency and throughput are not benchmarked. Build both profiles explicitly with build:wasm --profile <name>.", profiles: rows };
  });
}

if (import.meta.main)
  console.log(JSON.stringify(await profileReport(resolve(dirname(fileURLToPath(import.meta.url)), "..")), null, 2));
