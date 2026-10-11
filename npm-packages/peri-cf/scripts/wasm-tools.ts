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

export async function requirePublicTransport(packageRoot: string): Promise<void> {
  for (const file of ["wasm.js", "wasm.d.ts"]) {
    try { await Bun.file(resolve(packageRoot, "../@peri-sdk/dist", file)).text(); }
    catch (error) {
      throw new Error(`Missing SDK public WASM entry dist/${file}; prepare @peri-code/sdk JS entries before building CF (reuse an existing SDK dist).`, { cause: error });
    }
  }
}

export async function runBuild(command: string[], cwd: string, env: NodeJS.ProcessEnv): Promise<void> {
  const process = Bun.spawn(command, { cwd, env, stdin: "inherit", stdout: "inherit", stderr: "inherit" });
  const status = await process.exited;
  if (status !== 0) throw new Error(`${command[0]} exited with status ${status}`);
}
