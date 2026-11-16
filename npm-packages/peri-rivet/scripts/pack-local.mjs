#!/usr/bin/env node
// Packs @peri-code/sdk and @peri-code/rivet into tarballs for local installs.
// Nothing is published: `npm pack` only writes tgz files to the output directory.
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const packageDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const repoRoot = path.resolve(packageDir, '../..');
const sdkDir = path.join(repoRoot, 'npm-packages/@peri-sdk');

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

function pack(dir, outDir, manifest) {
  if (!existsSync(path.join(dir, 'dist'))) {
    fail(`${manifest.name} has no dist; run "bun run build" in ${dir} first`);
  }
  try {
    execFileSync('npm', ['pack', '--pack-destination', outDir, '--silent'], { cwd: dir, stdio: 'pipe' });
  } catch (error) {
    fail(`npm pack failed for ${manifest.name}: ${error.stderr?.toString() ?? error.message}`);
  }
  const tarball = path.join(outDir, tarballName(manifest));
  if (!existsSync(tarball)) {
    fail(`npm pack did not produce ${tarball}`);
  }
  return tarball;
}

const rivetManifest = readManifest(packageDir);
const localDeps = Object.entries(rivetManifest.dependencies ?? {}).filter(([, spec]) =>
  String(spec).startsWith('file:'),
);
if (localDeps.length > 0) {
  // file: specs cannot resolve outside the repo, so they must not ship in dependencies.
  fail(
    `dependencies still contain repo-local specs: ${localDeps.map(([name]) => name).join(', ')}; ` +
      'move them to devDependencies and declare them as peers',
  );
}

const sdkManifest = readManifest(sdkDir);
const rivetkitPeer = rivetManifest.peerDependencies?.rivetkit;
if (typeof rivetkitPeer !== 'string') {
  fail('rivetkit must stay a peer dependency so consumers pick their own runtime version');
}

const outIndex = process.argv.indexOf('--out');
const requestedOut = outIndex === -1 ? null : process.argv[outIndex + 1];
if (outIndex !== -1 && !requestedOut) {
  fail('--out needs a directory argument');
}
// Default keeps each run isolated; --out lets CI or docs pick a stable path.
const outDir = requestedOut
  ? path.resolve(requestedOut)
  : mkdtempSync(path.join(os.tmpdir(), 'peri-rivet-pack-'));
mkdirSync(outDir, { recursive: true });
const sdkTarball = pack(sdkDir, outDir, sdkManifest);
const rivetTarball = pack(packageDir, outDir, rivetManifest);

// The SDK is a peer, so both tarballs go into the same install command; npm resolves
// the peer from the sibling tarball instead of the registry.
const install = `npm install ${sdkTarball} ${rivetTarball} rivetkit@${rivetkitPeer.replace(/^\D+/, '')}`;
writeFileSync(
  path.join(outDir, 'install.sh'),
  `#!/bin/sh\nset -eu\nmkdir -p peri-rivet-probe\ncd peri-rivet-probe\n${install}\nnode -e "import('@peri-code/rivet/rivet')"\n`,
);

console.log(`\nTarballs (nothing published):\n  ${sdkTarball}\n  ${rivetTarball}`);
console.log(`\nInstall both packages in one command; @peri-code/sdk is a peer dependency:\n\n  ${install}\n`);
console.log(`\nWrote ${path.join(outDir, 'install.sh')}`);
console.log('Build both packages first (bun run build) after changing their src.');
