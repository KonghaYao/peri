import { access, copyFile, cp, mkdir, readFile, rename, rm } from "node:fs/promises";
import { dirname, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const sdkRoot = resolve(packageRoot, "../@peri-sdk");
const source = resolve(sdkRoot, "dist/wasm");
const destination = resolve(packageRoot, "worker/wasm/generated");
const staging = `${destination}.tmp-${process.pid}`;

async function required(path: string): Promise<void> {
  try { await access(path); } catch {
    throw new Error(`Missing built SDK artifact: ${path}. Reuse a complete @peri-code/sdk dist build or explicitly build that SDK first (Rust WASM build is expensive). This script never builds the SDK.`);
  }
}

try {
  for (const file of ["peri-wasm.js", "peri_wasm.wasm"]) await required(resolve(source, file));
  for (const file of ["dist/wasm.js", "dist/wasm.d.ts"])
    await required(resolve(sdkRoot, file));
  const glue = await readFile(resolve(source, "peri-wasm.js"), "utf8");
  const snippets = [...glue.matchAll(/\b(?:from\s*|import\s*)["']\.\/snippets\/([^"']+)["']/g)].map((match) => match[1]);
  for (const snippet of snippets) {
    const path = resolve(source, "snippets", snippet);
    if (!path.startsWith(resolve(source, "snippets") + sep)) throw new Error("Invalid SDK snippet path");
    await required(path);
  }
  await mkdir(staging, { recursive: true });
  for (const file of ["peri-wasm.js", "peri_wasm.wasm"]) await copyFile(resolve(source, file), resolve(staging, file));
  if (snippets.length) await cp(resolve(source, "snippets"), resolve(staging, "snippets"), { recursive: true });
  await rm(destination, { recursive: true, force: true });
  await rename(staging, destination);
  console.log(`Prepared existing SDK WASM artifacts in ${destination}`);
} finally {
  await rm(staging, { recursive: true, force: true });
}
