import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

interface PackageMetadata {
  name: string;
  version: string;
  periBuildId: string;
  periProtocolVersion: number;
}

const packageJson = JSON.parse(await readFile("package.json", "utf8")) as PackageMetadata;
const source = await readFile("src/adapter.ts", "utf8");
const types = await readFile("src/types.ts", "utf8");
const rustArtifact = await readFile("../../peri-js-runtime/src/artifact.rs", "utf8").catch(() => readFile("../../../peri-js-runtime/src/artifact.rs", "utf8"));
const buildId = `${packageJson.name}@${packageJson.version}`;

assert.equal(packageJson.version, "0.2.2");
assert.equal(packageJson.periBuildId, buildId);
assert.match(source, new RegExp(`PTC_BUILD_ID = ["']${buildId.replaceAll("/", "\\/")}["']`));
assert.match(types, new RegExp(`PTC_PROTOCOL_VERSION = ${packageJson.periProtocolVersion}`));
assert.match(rustArtifact, new RegExp(`PACKAGE_NAME: &str = ["']${packageJson.name.replaceAll("/", "\\/")}["']`));
assert.match(rustArtifact, new RegExp(`PACKAGE_VERSION: &str = ["']${packageJson.version}["']`));
assert.match(rustArtifact, new RegExp(`PROTOCOL_VERSION: u64 = ${packageJson.periProtocolVersion}`));
assert.match(rustArtifact, new RegExp(`BUILD_ID: &str = ["']${buildId.replaceAll("/", "\\/")}["']`));

const roots = await Promise.all(["a", "b"].map((suffix) => mkdtemp(join(tmpdir(), `peri-ptc-dist-${suffix}-`))));
const artifactNames = ["peri-ptc.js", "index.js", "index.d.ts", "adapter.d.ts", "types.d.ts"];

try {
  for (const root of roots) {
    run("bun", ["build", "src/cli.ts", `--outfile=${join(root, "peri-ptc.js")}`, "--target=node", "--format=esm", "--banner=#!/usr/bin/env node"]);
    run("bun", ["build", "src/index.ts", `--outfile=${join(root, "index.js")}`, "--target=node", "--format=esm"]);
    run("bunx", ["tsc", "--declaration", "--emitDeclarationOnly", "--outDir", root, "--noEmit", "false"]);
  }

  const hashes = await Promise.all(roots.map((root) => Promise.all(artifactNames.map(async (name) =>
    createHash("sha256").update(await readFile(join(root, name))).digest("hex")
  ))));
  assert.deepEqual(hashes[1], hashes[0], "dist build is not byte-reproducible");
} finally {
  await Promise.all(roots.map((root) => rm(root, { recursive: true, force: true })));
}

function run(command: string, args: string[]): void {
  const result = spawnSync(command, args, { stdio: "inherit" });
  assert.ifError(result.error);
  assert.equal(result.status, 0, `${command} failed`);
}
