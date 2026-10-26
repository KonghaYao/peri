import { resolve } from "node:path";
import { cleanBuildEnvironment, PINNED_TOOLCHAIN } from "./wasm-profile";

export async function capture(command: string[], cwd: string, env?: NodeJS.ProcessEnv): Promise<string> {
  const process = Bun.spawn(command, { cwd, env, stdout: "pipe", stderr: "pipe" });
  const [stdout, stderr, status] = await Promise.all([new Response(process.stdout).text(),
    new Response(process.stderr).text(), process.exited]);
  if (status !== 0) throw new Error(`${command.join(" ")} exited with status ${status}: ${stderr}`);
  return stdout.trim();
}

export async function compilerEnvironment(repoRoot: string): Promise<NodeJS.ProcessEnv> {
  const env = cleanBuildEnvironment(process.env);
  const validate = "import sys; assert sys.version_info >= (3, 10)";
  const python = env.EMSDK_PYTHON ?? Bun.which("python3");
  if (!python) throw new Error("Emscripten requires Python 3.10 or newer");
  try { await capture([python, "-c", validate], repoRoot, env); env.EMSDK_PYTHON = python; }
  catch (error) {
    const fallback = Bun.which("python3");
    if (!fallback || fallback === python) throw error;
    await capture([fallback, "-c", validate], repoRoot, env);
    env.EMSDK_PYTHON = fallback;
  }
  return env;
}

export async function toolchainVersions(repoRoot: string): Promise<Record<string, string>> {
  const env = await compilerEnvironment(repoRoot);
  const [rustc, cargo, emcc, wasmBindgen] = await Promise.all([
    capture(["rustc", "-Vv"], repoRoot, env), capture(["cargo", "-V"], repoRoot, env),
    capture(["emcc", "--version"], repoRoot, env), capture(["wasm-bindgen", "--version"], repoRoot, env),
  ]);
  if (!rustc.includes(`release: ${PINNED_TOOLCHAIN.rust}\n`)
    || !rustc.includes(`commit-hash: ${PINNED_TOOLCHAIN.rustCommit}\n`)
    || !cargo.startsWith(`cargo ${PINNED_TOOLCHAIN.rust} `)
    || !new RegExp(`\\b${PINNED_TOOLCHAIN.emscripten.replaceAll(".", "\\.")}\\b`).test(emcc)
    || wasmBindgen !== `wasm-bindgen ${PINNED_TOOLCHAIN.wasmBindgen}`)
    throw new Error(`CF WASM requires Rust/Cargo ${PINNED_TOOLCHAIN.rust}, Emscripten ${PINNED_TOOLCHAIN.emscripten} and wasm-bindgen ${PINNED_TOOLCHAIN.wasmBindgen}`);
  return { rustc, cargo, emcc, wasmBindgen };
}

export async function canonicalRustflags(repoRoot: string, settings: Record<string, unknown>): Promise<string> {
  const env = cleanBuildEnvironment(process.env);
  env.PERI_WASM_STACK_SIZE = String(settings.stackBytes);
  env.PERI_WASM_MAX_MEMORY = String(settings.maxMemoryBytes);
  env.PERI_WASM_MEMORY_GROWTH_LINEAR_STEP = String(settings.growthStepBytes);
  return capture(["bash", resolve(repoRoot, "scripts/cargo-wasm.sh"), "--print-rustflags"], repoRoot, env);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** WASM 入口以 SDK `package.json#exports["./wasm"]` 为唯一权威，不假设 dist 布局。 */
async function publicWasmExports(packageRoot: string): Promise<{ sdkRoot: string; runtime: string; types: string }> {
  const sdkRoot = resolve(packageRoot, "../@peri-sdk");
  const manifestPath = resolve(sdkRoot, "package.json");
  let manifest: unknown;
  try { manifest = await Bun.file(manifestPath).json(); }
  catch (error) {
    throw new Error(`Cannot read @peri-code/sdk manifest at ${manifestPath}; prepare the SDK package before building CF.`, { cause: error });
  }
  const wasm = isRecord(manifest) && isRecord(manifest.exports) ? manifest.exports["./wasm"] : undefined;
  const types = isRecord(wasm) && typeof wasm.types === "string" ? wasm.types : undefined;
  const runtime = isRecord(wasm) && typeof wasm.import === "string" ? wasm.import : undefined;
  if (!types || !runtime)
    throw new Error(`@peri-code/sdk package.json must declare exports["./wasm"].types and exports["./wasm"].import (${manifestPath}).`);
  return { sdkRoot, runtime, types };
}

export async function requirePublicTransport(packageRoot: string): Promise<void> {
  const { sdkRoot, runtime, types } = await publicWasmExports(packageRoot);
  for (const [kind, relative] of [["types", types], ["import", runtime]] as const) {
    const target = resolve(sdkRoot, relative);
    if (!(await Bun.file(target).exists()))
      throw new Error(`Missing @peri-code/sdk WASM ${kind} entry ${relative} (resolved to ${target}); rebuild @peri-code/sdk before building CF.`);
  }
}

export async function runBuild(command: string[], cwd: string, env: NodeJS.ProcessEnv): Promise<void> {
  const process = Bun.spawn(command, { cwd, env, stdin: "inherit", stdout: "inherit", stderr: "inherit" });
  const status = await process.exited;
  if (status !== 0) throw new Error(`${command[0]} exited with status ${status}`);
}
