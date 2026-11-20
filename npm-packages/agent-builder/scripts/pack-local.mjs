#!/usr/bin/env node
// Packs @peri-code/agent-builder as a private tarball plus an installable four-tarball set.
// Nothing is published. The SDK/rivet/sandbox tarballs come from an existing verified pack
// directory; this script never rebuilds or deletes their dist/wasm artifacts.
import { execFileSync } from 'node:child_process';
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { writeLocalKit } from './local-kit.mjs';

const packageDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const repoRoot = path.resolve(packageDir, '../..');

function fail(message) {
  console.error(`pack-local: ${message}`);
  process.exit(1);
}

function option(name) {
  const index = process.argv.indexOf(name);
  if (index === -1) return null;
  const value = process.argv[index + 1];
  if (!value || value.startsWith('--')) fail(`${name} needs a value`);
  return path.resolve(value);
}

function run(command, args, cwd = packageDir, stdio = 'inherit') {
  try {
    return execFileSync(command, args, { cwd, encoding: 'utf8', stdio });
  } catch (error) {
    if (error?.code === 'ENOSPC') fail('ENOSPC: insufficient disk space');
    fail(`${command} failed; child exit ${Number.isInteger(error?.status) ? error.status : 'unknown'}`);
  }
}

function readJson(file) {
  return JSON.parse(readFileSync(file, 'utf8'));
}

function listingOf(tarball) {
  return run('tar', ['-tzf', tarball], undefined, 'pipe').split('\n').filter(Boolean);
}

function manifestOf(tarball) {
  return JSON.parse(run('tar', ['-xOzf', tarball, 'package/package.json'], undefined, 'pipe'));
}

function tarballName(manifest) {
  return `${manifest.name.replace('@', '').replace('/', '-')}-${manifest.version}.tgz`;
}

const packDir = option('--pack-dir') ?? process.env.PERI_PACK_DIR;
if (!packDir && !['--sdk-tgz', '--rivet-tgz', '--sandbox-tgz'].every((name) => process.argv.includes(name))) {
  fail('provide --pack-dir or all three dependency tarballs');
}
const outDir = option('--out') ?? mkdtempSync(path.join(os.tmpdir(), 'agent-builder-pack-'));
const input = {
  sdk: option('--sdk-tgz') ?? path.join(packDir, 'peri-code-sdk-0.0.0.tgz'),
  rivet: option('--rivet-tgz') ?? path.join(packDir, 'peri-code-rivet-0.1.0.tgz'),
  sandbox: option('--sandbox-tgz') ?? path.join(packDir, 'peri-sandbox-0.1.0.tgz'),
};

for (const [name, file] of Object.entries(input)) {
  if (!existsSync(file)) fail(`missing ${name} tarball: ${file}`);
}
mkdirSync(outDir, { recursive: true });

const manifests = Object.fromEntries(
  Object.entries(input).map(([name, file]) => [name, manifestOf(file)]),
);
if (manifests.sdk.name !== '@peri-code/sdk') fail('SDK tarball has the wrong package name');
if (manifests.rivet.name !== '@peri-code/rivet') fail('rivet tarball has the wrong package name');
if (manifests.sandbox.name !== 'peri-sandbox') fail('sandbox tarball has the wrong package name');

// Fail early if the reusable set predates the API agent-builder calls.
const sdkView = run('tar', ['-xOzf', input.sdk, 'package/dist/entries/view.d.ts'], undefined, 'pipe');
if (!sdkView.includes('ChatState')) fail('SDK tarball does not export ChatState');
const rivetIndex = run('tar', ['-xOzf', input.rivet, 'package/dist/rivet/index.d.ts'], undefined, 'pipe');
if (!rivetIndex.includes('createHostResourceOwner')) fail('rivet tarball does not export createHostResourceOwner');
const rivetSandbox = run('tar', ['-xOzf', input.rivet, 'package/dist/sandbox/index.d.ts'], undefined, 'pipe');
if (!rivetSandbox.includes('createPeriSandboxMcpRuntime')) fail('rivet tarball has no sandbox adapter');

// Build current sources: the SPA plus the bundled Bun server. This does not touch SDK/rivet dist.
run('bun', ['run', 'build']);

const stage = path.join(mkdtempSync(path.join(os.tmpdir(), 'agent-builder-stage-')), 'package');
mkdirSync(stage, { recursive: true });
for (const entry of ['bin', 'dist']) {
  cpSync(path.join(packageDir, entry), path.join(stage, entry), { recursive: true });
}
mkdirSync(path.join(stage, 'scripts'), { recursive: true });
cpSync(path.join(packageDir, 'scripts/rivet-engine-wrapper.sh'), path.join(stage, 'scripts/rivet-engine-wrapper.sh'));
cpSync(path.join(packageDir, 'README.md'), path.join(stage, 'README.md'));
cpSync(path.join(packageDir, '.env.example'), path.join(stage, '.env.example'));
// README links to docs/agents-api.md, so the installed kit must carry the document it points at.
cpSync(path.join(packageDir, 'docs'), path.join(stage, 'docs'), { recursive: true });

const base = readJson(path.join(packageDir, 'package.json'));
const dependencies = { ...base.dependencies };
dependencies['@peri-code/sdk'] = manifests.sdk.version;
dependencies['@peri-code/rivet'] = manifests.rivet.version;
dependencies['peri-sandbox'] = manifests.sandbox.version;

const manifest = {
  name: base.name,
  version: base.version,
  private: true,
  type: 'module',
  description: 'Local single-user Peri Chat + Sandbox web app (private tarball)',
  bin: base.bin,
  engines: base.engines,
  dependencies,
};
const manifestText = `${JSON.stringify(manifest, null, 2)}\n`;
if (/\b(?:file|workspace):|(^|["\s])\.\.?\//m.test(JSON.stringify(dependencies))) {
  fail('packed dependencies still contain a repo-local spec');
}
writeFileSync(path.join(stage, 'package.json'), manifestText);

const packed = path.join(outDir, tarballName(manifest));
run('npm', ['pack', stage, '--pack-destination', outDir], packageDir, 'pipe');
if (!existsSync(packed)) fail(`npm pack did not produce ${packed}`);

const listing = listingOf(packed);
const forbidden =
  /(^|\/)(?:node_modules|\.git|\.data|\.rivetkit|coverage|credentials?|sandboxes?)(\/|$)|(^|\/)\.env$|(^|\/)\.env\.(?!example$)[^/]+$|\.(?:log|tgz|sqlite|sqlite3|db|key|pem)$/;
const bad = listing.filter((entry) => forbidden.test(entry));
if (bad.length > 0) fail(`agent-builder tarball contains forbidden paths: ${bad.join(', ')}`);
for (const required of [
  'package/package.json',
  'package/bin/agent-builder.js',
  'package/dist/index.html',
  'package/dist/server/main.js',
  'package/scripts/rivet-engine-wrapper.sh',
  'package/README.md',
  'package/.env.example',
]) {
  if (!listing.includes(required)) fail(`agent-builder tarball is missing ${required}`);
}
if (!listing.some((entry) => entry.startsWith('package/dist/assets/'))) {
  fail('agent-builder tarball has no frontend assets');
}
const packedManifest = JSON.parse(
  run('tar', ['-xOzf', packed, 'package/package.json'], undefined, 'pipe'),
);
if (packedManifest.devDependencies !== undefined) fail('packed manifest still has devDependencies');
if (/\b(?:file|workspace):/.test(JSON.stringify(packedManifest.dependencies))) {
  fail('packed manifest dependencies contain a repo-local spec');
}

const commit = run('git', ['rev-parse', 'HEAD'], repoRoot, 'pipe').trim();
const status = run('git', ['status', '--porcelain'], repoRoot, 'pipe').trim();
const archives = {
  agentBuilder: { path: packed, version: manifest.version },
  ...Object.fromEntries(Object.entries(input).map(([name, file]) => [name, { path: file, version: manifests[name].version }])),
};

const result = writeLocalKit(outDir, archives, {
  provenance: {
    commit,
    worktreeDirty: status.length > 0,
    upstream: 'caller-supplied verified archives; identified by sha256; WASM compatibility unverified',
  },
  package: base.name,
  version: manifest.version,
  dependencies,
  build: 'bun run build (vite build + bun build server/main.ts --target bun --packages external)',
});
rmSync(path.dirname(stage), { recursive: true, force: true });
console.log(JSON.stringify(result, null, 2));
