#!/usr/bin/env node
// Packs @peri-code/sdk, @peri-code/rivet and peri-sandbox into tarballs for local installs.
// Nothing is published: `npm pack` only writes tgz files to the output directory.
//
// The SDK dist is rebuilt into an isolated staging directory from the current TypeScript sources;
// the existing dist/wasm glue and binary are copied as-is and recorded as unverified. No package's
// dist is deleted or replaced by this script.
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const packageDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const repoRoot = path.resolve(packageDir, '../..');
const sdkDir = path.join(repoRoot, 'npm-packages/@peri-sdk');
const sandboxDir = path.join(repoRoot, 'npm-packages/peri-sandbox');

function fail(message) {
  console.error(`pack-local: ${message}`);
  process.exit(1);
}

function readManifest(dir) {
  return JSON.parse(readFileSync(path.join(dir, 'package.json'), 'utf8'));
}

function tarballName(manifest) {
  return `${manifest.name.replace('@', '').replace('/', '-')}-${manifest.version}.tgz`;
}

function run(command, args, cwd, stdio = 'inherit') {
  try {
    return execFileSync(command, args, { cwd, encoding: 'utf8', stdio });
  } catch (error) {
    fail(`${command} ${args.join(' ')} failed: ${error.stderr?.toString().trim() || error.message}`);
  }
}

function sha256(file) {
  return createHash('sha256').update(readFileSync(file)).digest('hex');
}

function copy(source, destination) {
  if (!existsSync(source)) fail(`missing ${source}`);
  cpSync(source, destination, { recursive: true });
}

/**
 * The tarball manifest must not carry repo-local specs or dev-only wiring. Removing
 * devDependencies also removes the `file:../...` entries that cannot resolve outside this repo.
 */
function writePackedManifest(dir, destination) {
  const manifest = readManifest(dir);
  delete manifest.devDependencies;
  const text = `${JSON.stringify(manifest, null, 2)}\n`;
  if (/\b(?:file|workspace):/.test(text)) {
    fail(`${manifest.name} still contains a repo-local dependency spec`);
  }
  writeFileSync(destination, text);
  return manifest;
}

function verifyTarball(tarball, required, label) {
  const listing = run('tar', ['-tzf', tarball], undefined, 'pipe')
    .split('\n')
    .filter(Boolean);
  const forbidden = /(^|\/)(?:node_modules|\.git|\.data|\.rivetkit|coverage)(\/|$)|(^|\/)\.env(?:\.[^/]*)?(\/|$)|\.(?:log|tgz|sqlite|sqlite3|db)$/;
  const bad = listing.filter((entry) => forbidden.test(entry));
  if (bad.length > 0) fail(`${label} tarball contains runtime/secret paths: ${bad.join(', ')}`);
  for (const entry of required) {
    if (!listing.includes(entry)) fail(`${label} tarball is missing ${entry}`);
  }
  const manifest = JSON.parse(run('tar', ['-xOzf', tarball, 'package/package.json'], undefined, 'pipe'));
  if (/\b(?:file|workspace):/.test(JSON.stringify(manifest.dependencies ?? {}))) {
    fail(`${label} tarball dependencies contain a repo-local spec`);
  }
  if (manifest.devDependencies !== undefined) fail(`${label} tarball still contains devDependencies`);
  const bytes = statSync(tarball).size;
  return { path: tarball, bytes, sha256: sha256(tarball), entries: listing.length, version: manifest.version };
}

const outIndex = process.argv.indexOf('--out');
const requestedOut = outIndex === -1 ? null : process.argv[outIndex + 1];
if (outIndex !== -1 && !requestedOut) fail('--out needs a directory argument');
const sdkStageIndex = process.argv.indexOf('--sdk-stage');
const requestedSdkStage = sdkStageIndex === -1 ? null : process.argv[sdkStageIndex + 1];
if (sdkStageIndex !== -1 && !requestedSdkStage) fail('--sdk-stage needs a directory argument');

// Default keeps each run isolated; --out lets docs pick a stable path.
const outDir = requestedOut ? path.resolve(requestedOut) : mkdtempSync(path.join(os.tmpdir(), 'peri-rivet-pack-'));
const stageRoot = mkdtempSync(path.join(os.tmpdir(), 'peri-pack-stage-'));
mkdirSync(outDir, { recursive: true });

const rivetManifest = readManifest(packageDir);
const rivetkitPeer = rivetManifest.peerDependencies?.rivetkit;
if (typeof rivetkitPeer !== 'string') fail('rivetkit must stay a peer dependency so consumers pick their own runtime version');
const rivetkitVersion = rivetkitPeer.replace(/^[^\d]*/, '');

try {
  const sdkStage = requestedSdkStage === null ? path.join(stageRoot, 'sdk-dist') : path.resolve(requestedSdkStage);
  if (requestedSdkStage === null) {
    const wasmSource = path.join(sdkDir, 'dist/wasm');
    if (!existsSync(path.join(wasmSource, 'peri-wasm.js')) || !existsSync(path.join(wasmSource, 'peri_wasm.wasm'))) {
      fail('no verified WASM artifact at npm-packages/@peri-sdk/dist/wasm; build it once on an Emscripten host or pass --sdk-stage');
    }
    run('bun', ['run', 'openui:check'], sdkDir);
    run('bun', ['run', 'scripts/build.ts', '--outdir', sdkStage, '--wasm-source', wasmSource], sdkDir);
  }
  for (const file of ['index.js', 'view.js', 'entries/view.d.ts', 'wasm/peri-wasm.js', 'wasm/peri_wasm.wasm']) {
    if (!existsSync(path.join(sdkStage, file))) fail(`SDK stage is missing ${file}`);
  }

  const sdkPackage = path.join(stageRoot, 'pkg-sdk');
  mkdirSync(sdkPackage, { recursive: true });
  copy(sdkStage, path.join(sdkPackage, 'dist'));
  copy(path.join(sdkDir, 'src/openui'), path.join(sdkPackage, 'src/openui'));
  copy(path.join(sdkDir, 'README.md'), path.join(sdkPackage, 'README.md'));
  if (existsSync(path.join(sdkDir, 'LICENSE'))) copy(path.join(sdkDir, 'LICENSE'), path.join(sdkPackage, 'LICENSE'));
  const sdkManifest = writePackedManifest(sdkDir, path.join(sdkPackage, 'package.json'));

  const rivetDist = path.join(stageRoot, 'rivet-dist');
  run(path.join(packageDir, 'node_modules/.bin/tsc'), ['-p', 'tsconfig.build.json', '--outDir', rivetDist], packageDir);
  for (const file of ['index.js', 'rivet/index.js', 'sandbox/index.js', 'sandbox/index.d.ts']) {
    if (!existsSync(path.join(rivetDist, file))) fail(`rivet build is missing ${file}`);
  }
  const rivetPackage = path.join(stageRoot, 'pkg-rivet');
  mkdirSync(rivetPackage, { recursive: true });
  copy(rivetDist, path.join(rivetPackage, 'dist'));
  copy(path.join(packageDir, 'src'), path.join(rivetPackage, 'src'));
  copy(path.join(packageDir, 'docs'), path.join(rivetPackage, 'docs'));
  copy(path.join(packageDir, 'examples'), path.join(rivetPackage, 'examples'));
  copy(path.join(packageDir, 'scripts/check-engine.mjs'), path.join(rivetPackage, 'scripts/check-engine.mjs'));
  copy(path.join(packageDir, 'README.md'), path.join(rivetPackage, 'README.md'));
  copy(path.join(packageDir, 'DESIGN.md'), path.join(rivetPackage, 'DESIGN.md'));
  writePackedManifest(packageDir, path.join(rivetPackage, 'package.json'));

  const sandboxDist = path.join(stageRoot, 'sandbox-dist');
  run(path.join(sandboxDir, 'node_modules/.bin/tsc'), ['-p', 'tsconfig.json', '--outDir', sandboxDist], sandboxDir);
  if (!existsSync(path.join(sandboxDist, 'src/index.js'))) fail('sandbox build is missing src/index.js');
  const sandboxPackage = path.join(stageRoot, 'pkg-sandbox');
  mkdirSync(sandboxPackage, { recursive: true });
  copy(path.join(sandboxDist, 'src'), path.join(sandboxPackage, 'dist/src'));
  copy(path.join(sandboxDir, 'scripts/patch-sdk.mjs'), path.join(sandboxPackage, 'scripts/patch-sdk.mjs'));
  copy(path.join(sandboxDir, 'examples'), path.join(sandboxPackage, 'examples'));
  copy(path.join(sandboxDir, 'COMPATIBILITY.md'), path.join(sandboxPackage, 'COMPATIBILITY.md'));
  copy(path.join(sandboxDir, 'README.md'), path.join(sandboxPackage, 'README.md'));
  const sandboxManifest = writePackedManifest(sandboxDir, path.join(sandboxPackage, 'package.json'));

  const pack = (dir, manifest) => {
    run('npm', ['pack', '--ignore-scripts', '--pack-destination', outDir, '--silent'], dir, 'pipe');
    const tarball = path.join(outDir, tarballName(manifest));
    if (!existsSync(tarball)) fail(`npm pack did not produce ${tarball}`);
    return tarball;
  };

  const sdkTarball = pack(sdkPackage, sdkManifest);
  const rivetTarball = pack(rivetPackage, rivetManifest);
  const sandboxTarball = pack(sandboxPackage, sandboxManifest);

  const tarballs = [
    verifyTarball(sdkTarball, [
      'package/dist/index.js',
      'package/dist/view.js',
      'package/dist/entries/view.d.ts',
      'package/dist/wasm/peri-wasm.js',
      'package/dist/wasm/peri_wasm.wasm',
    ], 'sdk'),
    verifyTarball(rivetTarball, [
      'package/dist/index.js',
      'package/dist/rivet/index.js',
      'package/dist/sandbox/index.js',
      'package/dist/sandbox/index.d.ts',
    ], 'rivet'),
    verifyTarball(sandboxTarball, [
      'package/dist/src/index.js',
      'package/scripts/patch-sdk.mjs',
    ], 'peri-sandbox'),
  ];

  // Chat-only consumers keep peri-sandbox out entirely; its actor entry is only reachable
  // through @peri-code/rivet/sandbox, whose peer dependency is optional.
  const chatInstall = `npm install ${sdkTarball} ${rivetTarball} rivetkit@${rivetkitVersion}`;
  const sandboxInstall = `npm install ${sdkTarball} ${rivetTarball} ${sandboxTarball} rivetkit@${rivetkitVersion}`;
  writeFileSync(
    path.join(outDir, 'install.sh'),
    `#!/bin/sh\nset -eu\nmode=\${1:-chat}\nmkdir -p peri-rivet-probe\ncd peri-rivet-probe\ncase "$mode" in\n  chat) ${chatInstall} ;;\n  sandbox) ${sandboxInstall} ;;\n  *) echo "usage: $0 [chat|sandbox]" >&2; exit 2 ;;\nesac\n`,
    { mode: 0o755 },
  );

  const gitCommit = run('git', ['rev-parse', 'HEAD'], repoRoot, 'pipe').trim();
  const gitStatus = run('git', ['status', '--porcelain'], repoRoot, 'pipe').trim();
  const sdkProvenance = existsSync(path.join(sdkStage, 'build-provenance.json'))
    ? JSON.parse(readFileSync(path.join(sdkStage, 'build-provenance.json'), 'utf8'))
    : null;
  writeFileSync(path.join(outDir, 'pack-manifest.json'), `${JSON.stringify({
    generatedAt: new Date().toISOString(),
    source: { gitCommit, dirty: gitStatus.length > 0 },
    sdk: sdkProvenance,
    tarballs,
  }, null, 2)}\n`);

  console.log('\nTarballs (nothing published):');
  for (const tarball of tarballs) {
    console.log(`  ${tarball.path}\n    sha256 ${tarball.sha256}\n    ${tarball.bytes} bytes`);
  }
  console.log('\nChat + Sandbox (install all three in one command):\n\n  ' + sandboxInstall);
  console.log('\nChat only (peri-sandbox stays optional):\n\n  ' + chatInstall);
  console.log(`\nWrote ${path.join(outDir, 'install.sh')} and ${path.join(outDir, 'pack-manifest.json')}`);
  console.log('Verify the engine in the consuming project with node node_modules/@peri-code/rivet/scripts/check-engine.mjs');
  console.log('WASM provenance: ' + (sdkProvenance?.wasm?.compatibility ?? 'not recorded'));
} finally {
  rmSync(stageRoot, { recursive: true, force: true });
}
