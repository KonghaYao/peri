import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { copyFile, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { isAbsolute, relative, resolve, sep } from "node:path";

const packageRoot = resolve(import.meta.dir, "..");
const repoRoot = resolve(packageRoot, "../..");
const cargoWasmSource = resolve(repoRoot, "target/wasm32-unknown-emscripten/release");

function option(name: string): string | null {
  const index = process.argv.indexOf(name);
  if (index === -1) return null;
  const value = process.argv[index + 1];
  if (value === undefined || value.startsWith("--")) throw new Error(`${name} needs a value`);
  return value;
}

const outArg = option("--outdir");
// `--no-wasm` keeps the echo/transport entries buildable on hosts without Emscripten; the
// runtime entry stays in dist, but its artifact is absent, so `runtime.kind: "wasm"` fails at startup.
const skipWasm = process.argv.includes("--no-wasm");
const wasmSourceArg = option("--wasm-source");
const legacyOutput = resolve(packageRoot, "dist");
const output = outArg === null ? legacyOutput : resolve(packageRoot, outArg);
const staging = outArg !== null;

// Staging is the non-destructive path: it never touches the package's existing dist or its
// WASM artifact, and it has to name the artifact it copies (or state that none is included).
if (!staging) {
  if (wasmSourceArg !== null) throw new Error("--wasm-source requires --outdir <staging>");
  console.warn("build: legacy full build replaces dist; use --outdir <dir> --wasm-source <dir> for a staged build");
} else {
  if (output === legacyOutput || output.startsWith(legacyOutput + sep)) {
    throw new Error("--outdir must not be the package dist or a directory inside it");
  }
  if (skipWasm === (wasmSourceArg !== null)) {
    throw new Error("staged builds need exactly one of --wasm-source <dir> or --no-wasm");
  }
}

async function run(command: string, args: string[], cwd: string): Promise<void> {
  const process = Bun.spawn([command, ...args], { cwd, stdin: "inherit", stdout: "inherit", stderr: "inherit" });
  const status = await process.exited;
  if (status !== 0) throw new Error(`${command} exited with status ${status}`);
}

function sha256(data: Uint8Array | string): string {
  return createHash("sha256").update(data).digest("hex");
}

function repoState(): { commit: string; dirty: boolean } {
  try {
    const commit = execFileSync("git", ["rev-parse", "HEAD"], { cwd: repoRoot, encoding: "utf8" }).trim();
    const status = execFileSync("git", ["status", "--porcelain"], { cwd: repoRoot, encoding: "utf8" });
    return { commit, dirty: status.trim().length > 0 };
  } catch {
    return { commit: "unknown", dirty: true };
  }
}

function sourceLabel(source: string): string {
  const rel = relative(repoRoot, source);
  return rel.startsWith("..") ? source : rel;
}

type CopiedAsset = { path: string; bytes: number; sha256: string };

/**
 * Copies an already-built Emscripten artifact. The wasm bytes and the JS glue are verified
 * together (magic + referenced snippets), but compatibility with the current Rust source is not
 * re-proven here: no toolchain is invoked and the artifact is recorded as unverified.
 */
async function copyWasmAssets(source: string, destination: string): Promise<CopiedAsset[]> {
  const glueFile = resolve(source, "peri-wasm.js");
  const wasmFile = resolve(source, "peri_wasm.wasm");
  const glue = await readFile(glueFile);
  const wasm = await readFile(wasmFile);
  const glueText = glue.toString("utf8");
  if (!glueText.includes("peri_wasm.wasm")) {
    throw new Error(`WASM glue does not reference peri_wasm.wasm: ${glueFile}`);
  }
  if (!wasm.subarray(0, 4).equals(Buffer.from([0x00, 0x61, 0x73, 0x6d]))) {
    throw new Error(`WASM artifact has no wasm magic header: ${wasmFile}`);
  }

  const copied: CopiedAsset[] = [];
  const copy = async (from: string, to: string, label: string): Promise<void> => {
    const data = await readFile(from);
    await mkdir(resolve(to, ".."), { recursive: true });
    await copyFile(from, to);
    copied.push({ path: label, bytes: data.byteLength, sha256: sha256(data) });
  };

  await copy(glueFile, resolve(destination, "peri-wasm.js"), "peri-wasm.js");
  await copy(wasmFile, resolve(destination, "peri_wasm.wasm"), "peri_wasm.wasm");

  for (const match of glueText.matchAll(/from\s+["']\.\/snippets\/([^"']+)["']/g)) {
    const snippet = match[1];
    if (!snippet || isAbsolute(snippet) || snippet.split("/").includes("..")) {
      throw new Error(`WASM glue references an unsafe snippet path: ${snippet ?? "<empty>"}`);
    }
    const from = [resolve(source, "snippets", snippet), resolve(source, "deps/snippets", snippet)]
      .find((candidate) => existsSync(candidate));
    if (from === undefined) throw new Error(`WASM glue references a missing snippet: ${snippet}`);
    await copy(from, resolve(destination, "snippets", snippet), `snippets/${snippet}`);
  }
  return copied;
}

if (!staging && !skipWasm) {
  await run(resolve(repoRoot, "scripts/cargo-wasm.sh"), [
    "build", "--locked", "--release", "-p", "peri-wasm", "--target", "wasm32-unknown-emscripten",
  ], repoRoot);
}

if (staging) {
  const marker = `${output}.peri-staging`;
  if (existsSync(output)) {
    if (!existsSync(marker)) {
      throw new Error(`refusing to replace non-staging directory ${output}; remove it or pick another --outdir`);
    }
    await rm(output, { recursive: true, force: true });
  }
  await mkdir(output, { recursive: true });
  await writeFile(marker, "created by @peri-code/sdk build.ts --outdir\n");
} else {
  await rm(output, { recursive: true, force: true });
}

const result = await Bun.build({
  entrypoints: [resolve(packageRoot, "src/entries/index.ts")],
  outdir: output,
  target: "bun",
  format: "esm",
  external: ["yjs"],
});
if (!result.success) throw new Error(`SDK bundle failed: ${result.logs.join("; ")}`);
const browser = await Bun.build({
  entrypoints: [resolve(packageRoot, "src/entries/wasm.ts")],
  outdir: output,
  target: "browser",
  format: "esm",
});
if (!browser.success) throw new Error(`WASM SDK bundle failed: ${browser.logs.join("; ")}`);
const view = await Bun.build({
  entrypoints: [resolve(packageRoot, "src/entries/view.ts")],
  outdir: output,
  target: "browser",
  format: "esm",
  external: ["yjs"],
});
if (!view.success) throw new Error(`Session view bundle failed: ${view.logs.join("; ")}`);
const portable = await Bun.build({
  entrypoints: [resolve(packageRoot, "src/entries/portable.ts")],
  outdir: output,
  target: "bun",
  format: "esm",
  external: ["yjs"],
});
if (!portable.success) throw new Error(`Portable SDK bundle failed: ${portable.logs.join("; ")}`);
const sandbox = await Bun.build({
  entrypoints: [resolve(packageRoot, "src/entries/sandbox.ts")],
  outdir: output,
  target: "node",
  format: "esm",
});
if (!sandbox.success) throw new Error(`Sandbox SDK bundle failed: ${sandbox.logs.join("; ")}`);
const observe = await Bun.build({
  entrypoints: [resolve(packageRoot, "src/entries/observe.ts")],
  outdir: output,
  target: "node",
  format: "esm",
});
if (!observe.success) throw new Error(`Observe SDK bundle failed: ${observe.logs.join("; ")}`);
const wasmNode = await Bun.build({
  entrypoints: [resolve(packageRoot, "src/entries/wasm-node.ts")],
  outdir: output,
  target: "node",
  external: ["dns", "net", "node:dns", "node:net"],
  format: "esm",
});
if (!wasmNode.success) throw new Error(`WASM node bundle failed: ${wasmNode.logs.join("; ")}`);
await run(resolve(packageRoot, "node_modules/.bin/tsc"), [
  "--noEmit", "false", "--emitDeclarationOnly", "--declaration", "--outDir", output,
], packageRoot);

const wasmOutput = resolve(output, "wasm");
let copiedWasm: CopiedAsset[] = [];
if (wasmSourceArg !== null) {
  copiedWasm = await copyWasmAssets(resolve(packageRoot, wasmSourceArg), wasmOutput);
} else if (!skipWasm) {
  copiedWasm = await copyWasmAssets(cargoWasmSource, wasmOutput);
}

if (staging) {
  for (const file of ["index.js", "view.js", "entries/index.d.ts", "entries/view.d.ts"]) {
    if (!existsSync(resolve(output, file))) throw new Error(`staged build is missing ${file}`);
  }
  const viewTypes = await readFile(resolve(output, "entries/view.d.ts"), "utf8");
  const chatTypes = await readFile(resolve(output, "entries/chat.d.ts"), "utf8");
  const viewJs = await readFile(resolve(output, "view.js"), "utf8");
  if (!viewTypes.includes("ChatState") || !chatTypes.includes("ChatState") || !viewJs.includes("ChatState")) {
    throw new Error("staged build does not expose ChatState from the main and /view entries");
  }
  const source = repoState();
  await writeFile(resolve(output, "build-provenance.json"), JSON.stringify({
    kind: "sdk-staged-build",
    builtAt: new Date().toISOString(),
    source,
    wasm: skipWasm
      ? { mode: "absent", compatibility: "no wasm artifact included (--no-wasm)" }
      : {
        mode: "copied-prebuilt",
        source: sourceLabel(resolve(packageRoot, wasmSourceArg ?? cargoWasmSource)),
        compatibility: "unverified: copied without rebuilding the current Rust source",
        files: copiedWasm,
      },
  }, null, 2) + "\n");
  console.log(`Built @peri-code/sdk into ${output} without touching ${legacyOutput}`);
} else if (skipWasm) {
  console.log("Built @peri-code/sdk without the peri-wasm artifact (TypeScript-only build).");
  console.log("Runtime backends { kind: \"wasm\" } are unavailable in this dist; use transport/echo runtimes.");
} else {
  console.log("Built @peri-code/sdk with the workspace peri-wasm artifact");
}
