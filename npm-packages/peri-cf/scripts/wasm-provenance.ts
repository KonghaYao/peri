import { createHash, randomUUID } from "node:crypto";
import { mkdir, readFile, readdir, rename, rm, lstat, writeFile } from "node:fs/promises";
import { dirname, relative, resolve, sep } from "node:path";
import { z } from "zod";
import { buildSettings, type WasmProfile } from "./wasm-profile";
import { canonicalRustflags } from "./wasm-tools";

export const REBUILD_COMMAND = "bun run build:wasm";
export const MANIFEST_FILE = "provenance.json";
const checksum = z.string().regex(/^[a-f0-9]{64}$/);
const checksums = z.record(z.string(), checksum);
export const provenanceSchema = z.object({
  version: z.literal(1),
  buildId: z.uuid(),
  builtAt: z.iso.datetime(),
  buildWallMs: z.number().nonnegative(),
  inputs: z.object({ files: checksums, settings: z.record(z.string(), z.unknown()),
    toolchain: z.record(z.string(), z.string()) }).strict(),
  artifacts: checksums,
}).strict();
export type BuildInputs = z.infer<typeof provenanceSchema>["inputs"];
export type Provenance = z.infer<typeof provenanceSchema>;

export function sha256(bytes: Uint8Array | string): string {
  return createHash("sha256").update(bytes).digest("hex");
}

export function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object")
    return `{${Object.entries(value).sort(([left], [right]) => left.localeCompare(right))
      .map(([key, entry]) => `${JSON.stringify(key)}:${canonical(entry)}`).join(",")}}`;
  return JSON.stringify(value);
}

const excludedDirectory = new Set([".git", "target", "node_modules", "dist", ".cache", ".wrangler"]);
const secretFile = /^(?:\.env(?:\..*)?|\.dev\.vars(?:\..*)?|credentials(?:\..*)?|secrets(?:\..*)?)$/i;

async function walk(root: string, visit: (path: string) => Promise<void>): Promise<void> {
  for (const entry of (await readdir(root, { withFileTypes: true })).sort((left, right) => left.name.localeCompare(right.name))) {
    if (secretFile.test(entry.name) || excludedDirectory.has(entry.name)) continue;
    const path = resolve(root, entry.name);
    if (entry.isSymbolicLink()) throw new Error(`Symlinked build input is unsupported: ${path}`);
    if (entry.isDirectory()) await walk(path, visit);
    else if (entry.isFile()) await visit(path);
  }
}

export async function sourceChecksums(repoRoot: string): Promise<Record<string, string>> {
  const files: Record<string, string> = {};
  const add = async (path: string) => {
    const name = relative(repoRoot, path).split(sep).join("/");
    if (name.startsWith("../") || name === "..") throw new Error("Build source escapes repository");
    files[name] = sha256(await readFile(path));
  };
  const cargo = Bun.TOML.parse(await readFile(resolve(repoRoot, "Cargo.toml"), "utf8")) as {
    workspace: { members: string[]; dependencies?: Record<string, { path?: string } | string> };
  };
  const roots = new Set(cargo.workspace.members);
  for (const dependency of Object.values(cargo.workspace.dependencies ?? {}))
    if (typeof dependency === "object" && dependency.path) roots.add(dependency.path);
  for (const member of roots) {
    if (member.includes("*") || resolve(repoRoot, member) === repoRoot)
      throw new Error("Unsupported wildcard or repository-wide Cargo source root");
    const directory = resolve(repoRoot, member);
    if (!directory.startsWith(`${repoRoot}${sep}`)) throw new Error("Local Cargo dependency escapes repository");
    await walk(directory, add);
    const manifest = Bun.TOML.parse(await readFile(resolve(directory, "Cargo.toml"), "utf8")) as Record<string, unknown>;
    const discover = (table: unknown) => {
      if (!table || typeof table !== "object") return;
      for (const dependency of Object.values(table)) {
        if (!dependency || typeof dependency !== "object" || !("path" in dependency)
          || typeof dependency.path !== "string") continue;
        roots.add(relative(repoRoot, resolve(directory, dependency.path)));
      }
    };
    for (const kind of ["dependencies", "dev-dependencies", "build-dependencies"]) discover(manifest[kind]);
    if (manifest.target && typeof manifest.target === "object") {
      for (const target of Object.values(manifest.target)) {
        if (!target || typeof target !== "object") continue;
        for (const kind of ["dependencies", "dev-dependencies", "build-dependencies"])
          discover((target as Record<string, unknown>)[kind]);
      }
    }
  }
  for (const directory of ["patches", ".cargo"]) await walk(resolve(repoRoot, directory), add);
  for (const file of ["Cargo.toml", "Cargo.lock", "scripts/cargo-wasm.sh", "scripts/cargo-rmcp-patched.sh",
    "scripts/prepare-emscripten.sh", "npm-packages/peri-cf/shared/runtime-limits.ts"])
    await add(resolve(repoRoot, file));
  await walk(resolve(repoRoot, "npm-packages/peri-cf/scripts"), async (path) => {
    if (/\/(?:build-wasm|wasm-provenance|wasm-profile|wasm-tools|worker-glue|worker-runtime-glue)\.ts$/.test(path)) await add(path);
  });
  for (const optional of ["rust-toolchain", "rust-toolchain.toml"]) {
    try { await lstat(resolve(repoRoot, optional)); } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") continue;
      throw error;
    }
    await add(resolve(repoRoot, optional));
  }
  return Object.fromEntries(Object.entries(files).sort(([left], [right]) => left.localeCompare(right)));
}

export async function buildInputs(repoRoot: string, profile: WasmProfile,
  toolchain: BuildInputs["toolchain"]): Promise<BuildInputs> {
  const settings = await buildSettings(repoRoot, profile);
  return { files: await sourceChecksums(repoRoot),
    settings: { ...settings, rustflags: await canonicalRustflags(repoRoot, settings) }, toolchain };
}

export function assertSameInputs(before: BuildInputs, after: BuildInputs): void {
  if (canonical(before) !== canonical(after))
    throw new Error(`WASM build inputs changed or are stale; run ${REBUILD_COMMAND} (select the same --profile if needed)`);
}

export async function artifactChecksums(root: string): Promise<Record<string, string>> {
  const files: Record<string, string> = {};
  await walk(root, async (path) => {
    const name = relative(root, path).split(sep).join("/");
    if (name === MANIFEST_FILE) return;
    if (name !== "peri-wasm.js" && name !== "peri_wasm.wasm" && !name.startsWith("snippets/"))
      throw new Error(`Unexpected WASM artifact: ${name}`);
    files[name] = sha256(await readFile(path));
  });
  if (!files["peri-wasm.js"] || !files["peri_wasm.wasm"])
    throw new Error(`Missing complete CF WASM artifacts; run ${REBUILD_COMMAND}`);
  return files;
}

export async function verifyArtifacts(root: string, inputs: BuildInputs): Promise<Provenance> {
  let manifest: Provenance;
  try { manifest = provenanceSchema.parse(JSON.parse(await readFile(resolve(root, MANIFEST_FILE), "utf8"))); }
  catch (error) { throw new Error(`Missing or invalid build-produced WASM provenance; run ${REBUILD_COMMAND}`, { cause: error }); }
  assertSameInputs(manifest.inputs, inputs);
  if (canonical(await artifactChecksums(root)) !== canonical(manifest.artifacts))
    throw new Error(`Mixed or tampered CF WASM artifacts; run ${REBUILD_COMMAND}`);
  return manifest;
}

export async function writeProvenance(root: string, inputs: BuildInputs, buildWallMs: number): Promise<void> {
  const manifest: Provenance = { version: 1, buildId: randomUUID(), builtAt: new Date().toISOString(),
    buildWallMs, inputs, artifacts: await artifactChecksums(root) };
  await writeFile(resolve(root, MANIFEST_FILE), `${JSON.stringify(manifest, null, 2)}\n`);
}

export async function replaceDirectory(staging: string, destination: string): Promise<void> {
  const backup = `${destination}.tmp-backup-${randomUUID()}`;
  let backedUp = false;
  try {
    try { await rename(destination, backup); backedUp = true; } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    }
    try { await rename(staging, destination); } catch (error) {
      if (backedUp) await rename(backup, destination);
      throw error;
    }
    if (backedUp) await rm(backup, { recursive: true, force: true });
  } catch (error) { throw new Error("Failed to publish verified WASM artifacts", { cause: error }); }
}

export async function withDirectoryLock<Result>(path: string, run: () => Promise<Result>): Promise<Result> {
  await mkdir(dirname(path), { recursive: true });
  try { await mkdir(path); } catch (error) {
    throw new Error(`WASM artifact directory is locked: ${path}; do not build/prepare concurrently. Remove the lock only if its process exited.`, { cause: error });
  }
  try { return await run(); } finally { await rm(path, { recursive: true, force: true }); }
}
