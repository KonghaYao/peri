import { copyFile, mkdir, readFile, rm } from "node:fs/promises";
import { dirname, resolve, sep } from "node:path";
import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import { assertSameInputs, buildInputs, replaceDirectory, withDirectoryLock, writeProvenance } from "./wasm-provenance";
import { wasmMemory, wasmProfile, WASM_TARGET } from "./wasm-profile";
import { compilerEnvironment, requirePublicTransport, runBuild, toolchainVersions } from "./wasm-tools";
import { workerRuntimeGlue } from "./worker-runtime-glue";

export async function buildWasm(packageRoot: string, args: string[]): Promise<void> {
  const repoRoot = resolve(packageRoot, "../..");
  const profile = wasmProfile(args);
  const cache = resolve(packageRoot, ".cache/wasm", profile);
  await withDirectoryLock(resolve(packageRoot, ".cache/wasm/build.lock"), async () => {
    await requirePublicTransport(packageRoot);
    const versions = await toolchainVersions(repoRoot);
    const before = await buildInputs(repoRoot, profile, versions);
    const staging = `${cache}.tmp-${randomUUID()}`;
    const env = await compilerEnvironment(repoRoot);
    env.PERI_WASM_STACK_SIZE = String(before.settings.stackBytes);
    env.PERI_WASM_MAX_MEMORY = String(before.settings.maxMemoryBytes);
    env.PERI_WASM_MEMORY_GROWTH_LINEAR_STEP = String(before.settings.growthStepBytes);
    env.CARGO_TARGET_DIR = resolve(repoRoot, "target");
    const started = performance.now();
    try {
      await runBuild([resolve(repoRoot, "scripts/cargo-wasm.sh"), "build", "--locked", "--profile", profile,
        "-p", "peri-wasm", "--features", "cloudflare", "--target", WASM_TARGET], repoRoot, env);
      const output = resolve(repoRoot, "target", WASM_TARGET, profile);
      const glue = await readFile(resolve(output, "peri-wasm.js"), "utf8");
      workerRuntimeGlue(glue);
      wasmMemory(await readFile(resolve(output, "peri_wasm.wasm")));
      await mkdir(staging, { recursive: true });
      for (const file of ["peri-wasm.js", "peri_wasm.wasm"])
        await copyFile(resolve(output, file), resolve(staging, file));
      const snippets = [...glue.matchAll(/\b(?:from\s*|import\s*)["']\.\/snippets\/([^"']+)["']/g)];
      for (const match of snippets) {
        const destination = resolve(staging, "snippets", match[1]);
        if (!destination.startsWith(resolve(staging, "snippets") + sep))
          throw new Error("Invalid WASM snippet path");
        await mkdir(dirname(destination), { recursive: true });
        await copyFile(resolve(output, "deps/snippets", match[1]), destination);
      }
      assertSameInputs(before, await buildInputs(repoRoot, profile, await toolchainVersions(repoRoot)));
      await writeProvenance(staging, before, performance.now() - started);
      await replaceDirectory(staging, cache);
      console.log(`Built and recorded CF WASM (${profile}) in ${cache}`);
    } finally { await rm(staging, { recursive: true, force: true }); }
  });
}

if (import.meta.main)
  await buildWasm(resolve(dirname(fileURLToPath(import.meta.url)), ".."), process.argv.slice(2));
