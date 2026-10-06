import { copyFile, mkdir, readFile, rm } from "node:fs/promises";
import { dirname, resolve } from "node:path";

const packageRoot = resolve(import.meta.dir, "..");
const repoRoot = resolve(packageRoot, "../..");
const wasmSource = resolve(repoRoot, "target/wasm32-unknown-emscripten/release");
const output = resolve(packageRoot, "dist");
const wasmOutput = resolve(output, "wasm");

async function run(command: string, args: string[], cwd: string): Promise<void> {
  const process = Bun.spawn([command, ...args], { cwd, stdin: "inherit", stdout: "inherit", stderr: "inherit" });
  const status = await process.exited;
  if (status !== 0) throw new Error(`${command} exited with status ${status}`);
}

await run(resolve(repoRoot, "scripts/cargo-wasm.sh"), [
  "build", "--locked", "--release", "-p", "peri-wasm", "--target", "wasm32-unknown-emscripten",
], repoRoot);

await rm(output, { recursive: true, force: true });

const result = await Bun.build({
  entrypoints: [resolve(packageRoot, "src/sdk/index.ts")],
  outdir: output,
  target: "bun",
  format: "esm",
  external: ["yjs"],
});
if (!result.success) throw new Error(`SDK bundle failed: ${result.logs.join("; ")}`);
const dispatcher = await Bun.build({
  entrypoints: [resolve(packageRoot, "src/execution/sidecar.ts")],
  outdir: resolve(output, "execution"),
  target: "bun",
  format: "esm",
});
if (!dispatcher.success) throw new Error(`SDK execution dispatcher bundle failed: ${dispatcher.logs.join("; ")}`);
const browser = await Bun.build({
  entrypoints: [resolve(packageRoot, "src/wasm.ts")],
  outdir: output,
  target: "browser",
  format: "esm",
});
if (!browser.success) throw new Error(`WASM SDK bundle failed: ${browser.logs.join("; ")}`);
const view = await Bun.build({
  entrypoints: [resolve(packageRoot, "src/view/index.ts")],
  outdir: resolve(output, "view"),
  target: "browser",
  format: "esm",
  external: ["yjs"],
});
if (!view.success) throw new Error(`Session view bundle failed: ${view.logs.join("; ")}`);
await run(resolve(packageRoot, "node_modules/.bin/tsc"), ["--noEmit", "false", "--emitDeclarationOnly", "--declaration", "--outDir", "dist"], packageRoot);

await mkdir(wasmOutput, { recursive: true });
const glue = await readFile(resolve(wasmSource, "peri-wasm.js"), "utf8");
const imports = [...glue.matchAll(/from\s+["']\.\/snippets\/([^"']+)["']/g)];
for (const [, path] of imports) {
  if (!path) continue;
  await mkdir(dirname(resolve(wasmOutput, "snippets", path)), { recursive: true });
  await copyFile(resolve(wasmSource, "deps/snippets", path), resolve(wasmOutput, "snippets", path));
}
await copyFile(resolve(wasmSource, "peri-wasm.js"), resolve(wasmOutput, "peri-wasm.js"));
await copyFile(resolve(wasmSource, "peri_wasm.wasm"), resolve(wasmOutput, "peri_wasm.wasm"));
console.log("Built @peri-code/sdk with the workspace peri-wasm artifact");
