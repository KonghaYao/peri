import { copyFile, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import { workerRuntimeGlue } from "./worker-runtime-glue";
import { artifactChecksums, assertSameInputs, buildInputs, canonical, replaceDirectory, verifyArtifacts, withDirectoryLock, type BuildInputs } from "./wasm-provenance";
import { DEFAULT_WASM_PROFILE, wasmMemory } from "./wasm-profile";
import { requirePublicTransport, toolchainVersions } from "./wasm-tools";

export async function prepareArtifacts(source: string, destination: string, inputs: BuildInputs,
  validateInputs: () => Promise<void> = async () => {}): Promise<void> {
  await withDirectoryLock(`${destination}.tmp-lock`, async () => {
    const manifest = await verifyArtifacts(source, inputs);
    const staging = `${destination}.tmp-${randomUUID()}`;
    try {
      await mkdir(staging, { recursive: true });
      for (const file of Object.keys(manifest.artifacts)) {
        const target = resolve(staging, file);
        if (!target.startsWith(`${staging}/`)) throw new Error("Invalid WASM artifact path");
        await mkdir(dirname(target), { recursive: true });
        await copyFile(resolve(source, file), target);
      }
      if (canonical(await artifactChecksums(staging)) !== canonical(manifest.artifacts))
        throw new Error("CF WASM artifacts changed while preparing; rebuild before retrying");
      wasmMemory(await readFile(resolve(staging, "peri_wasm.wasm")));
      const glue = await readFile(resolve(staging, "peri-wasm.js"), "utf8");
      await writeFile(resolve(staging, "peri-wasm.js"), workerRuntimeGlue(glue));
      await verifyArtifacts(source, inputs);
      await validateInputs();
      await replaceDirectory(staging, destination);
    } finally { await rm(staging, { recursive: true, force: true }); }
  });
}

export async function prepareWasm(packageRoot: string): Promise<void> {
  const repoRoot = resolve(packageRoot, "../..");
  await requirePublicTransport(packageRoot);
  const source = resolve(packageRoot, ".cache/wasm", DEFAULT_WASM_PROFILE);
  const destination = resolve(packageRoot, "worker/wasm/generated");
  await withDirectoryLock(resolve(packageRoot, ".cache/wasm/build.lock"), async () => {
    const versions = await toolchainVersions(repoRoot);
    const inputs = await buildInputs(repoRoot, DEFAULT_WASM_PROFILE, versions);
    await prepareArtifacts(source, destination, inputs,
      async () => assertSameInputs(inputs, await buildInputs(repoRoot, DEFAULT_WASM_PROFILE, versions)));
  });
  console.log(`Prepared verified CF WASM artifacts in ${destination}`);
}

if (import.meta.main)
  await prepareWasm(resolve(dirname(fileURLToPath(import.meta.url)), ".."));
